use crate::media::player::Player;
use crate::media::program_player::{ProgramClip, ProgramPlayer};
use crate::model::clip::{AudioChannelMode, Clip, ClipKind, Phase1KeyframeProperty};
use crate::model::media_library::{
    media_keyword_summary, FrameRateFilter, MediaCollection, MediaFilterCriteria, MediaItem,
    MediaKeywordRange, MediaKindFilter, MediaLibrary, MediaRating, MediaRatingFilter,
    ResolutionFilter, SourceMarks,
};
use crate::model::project::{FrameRate, Project};
use crate::model::track::TrackKind;
use crate::model::transition::{
    supported_transition_definitions, supported_transition_kinds,
    validate_track_transition_request, TransitionAlignment,
};
use crate::recent;
use crate::ui::timecode;
use crate::ui::timeline::{build_timeline_panel, MusicGenerationTarget, TimelineState};
use crate::ui::{
    audio_effects_browser, effects_browser, inspector, keyframe_editor, media_browser, preferences,
    preview, program_monitor, title_templates, titles_browser, toolbar,
};
use crate::undo::{EditCommand, SetClipAutoCropCommand, TrackClipsChange};
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

const SOURCE_KEYWORD_NEW_ID: &str = "__new__";

/// Check whether the focused widget is a text-input widget.
/// In GTK4, `Entry` delegates keyboard focus to an internal `gtk4::Text`
/// child, so `focused.is::<Entry>()` returns false when the user is typing
/// in an Entry.  We check for `Text`, `Entry`, `SearchEntry`, `TextView`,
/// and `SpinButton` (which also contains a Text internally).
fn is_text_input_focused(focused: &gtk4::Widget) -> bool {
    focused.is::<gtk4::Text>()
        || focused.is::<gtk4::Entry>()
        || focused.is::<gtk4::SearchEntry>()
        || focused.is::<gtk4::TextView>()
        || focused.is::<gtk4::SpinButton>()
}

fn workspace_paned_extent(paned: &Paned) -> i32 {
    match paned.orientation() {
        Orientation::Horizontal => paned.allocation().width(),
        Orientation::Vertical => paned.allocation().height(),
        _ => 0,
    }
}

fn workspace_paned_child_min_size(child: Option<gtk::Widget>, orientation: Orientation) -> i32 {
    child
        .map(|widget| widget.measure(orientation, -1).0.max(0))
        .unwrap_or(0)
}

fn clamp_workspace_paned_position(paned: &Paned, desired: i32) -> i32 {
    let desired = desired.max(0);
    let total = workspace_paned_extent(paned);
    if total <= 0 {
        return desired;
    }
    let orientation = paned.orientation();
    let min_start = workspace_paned_child_min_size(paned.start_child(), orientation);
    let min_end = workspace_paned_child_min_size(paned.end_child(), orientation);
    let min_bound = min_start.min(total);
    let max_bound = total.saturating_sub(min_end);
    if max_bound < min_bound {
        desired.clamp(0, total)
    } else {
        desired.clamp(min_bound, max_bound)
    }
}

fn capture_workspace_paned_state(
    paned: &Paned,
    fallback_position: i32,
    fallback_ratio_permille: Option<u16>,
) -> (i32, Option<u16>) {
    let total = workspace_paned_extent(paned);
    if total <= 0 {
        return (fallback_position.max(0), fallback_ratio_permille);
    }
    let position = clamp_workspace_paned_position(paned, paned.position());
    (
        position,
        crate::ui_state::workspace_split_ratio_from_pixels(position, total),
    )
}

fn workspace_target_paned_position(
    paned: &Paned,
    position: i32,
    ratio_permille: Option<u16>,
) -> Option<i32> {
    let total = workspace_paned_extent(paned);
    if total <= 0 {
        return None;
    }
    let scaled =
        crate::ui_state::workspace_split_position_from_ratio(ratio_permille, total, position);
    Some(clamp_workspace_paned_position(paned, scaled))
}

fn schedule_workspace_layout_apply_completion(
    apply_generation: u64,
    workspace_layout_apply_generation: Rc<Cell<u64>>,
    workspace_layouts_applying: Rc<Cell<bool>>,
    workspace_layout_pending_name: Rc<RefCell<Option<String>>>,
    sync_workspace_layout_state: Rc<dyn Fn()>,
    apply_split_positions: Rc<dyn Fn()>,
    pane_positions_ready: Rc<dyn Fn() -> bool>,
    remaining_attempts: u8,
) {
    glib::timeout_add_local_once(std::time::Duration::from_millis(16), move || {
        if workspace_layout_apply_generation.get() != apply_generation {
            return;
        }
        if !pane_positions_ready() {
            if remaining_attempts > 0 {
                schedule_workspace_layout_apply_completion(
                    apply_generation,
                    workspace_layout_apply_generation.clone(),
                    workspace_layouts_applying.clone(),
                    workspace_layout_pending_name.clone(),
                    sync_workspace_layout_state.clone(),
                    apply_split_positions.clone(),
                    pane_positions_ready.clone(),
                    remaining_attempts - 1,
                );
            } else {
                workspace_layout_pending_name.borrow_mut().take();
                workspace_layouts_applying.set(false);
            }
            return;
        }
        apply_split_positions();
        workspace_layouts_applying.set(false);
        sync_workspace_layout_state();
    });
}

fn media_kind_filter_id(filter: MediaKindFilter) -> &'static str {
    match filter {
        MediaKindFilter::All => "all",
        MediaKindFilter::Video => "video",
        MediaKindFilter::Audio => "audio",
        MediaKindFilter::Image => "image",
        MediaKindFilter::Offline => "offline",
    }
}

fn resolution_filter_id(filter: ResolutionFilter) -> &'static str {
    match filter {
        ResolutionFilter::All => "all",
        ResolutionFilter::SdOrSmaller => "sd",
        ResolutionFilter::Hd => "hd",
        ResolutionFilter::FullHd => "fhd",
        ResolutionFilter::UltraHd => "uhd",
    }
}

fn frame_rate_filter_id(filter: FrameRateFilter) -> &'static str {
    match filter {
        FrameRateFilter::All => "all",
        FrameRateFilter::Fps24OrLess => "fps24",
        FrameRateFilter::Fps25To30 => "fps25_30",
        FrameRateFilter::Fps31To59 => "fps31_59",
        FrameRateFilter::Fps60Plus => "fps60",
    }
}

fn media_rating_id(rating: MediaRating) -> &'static str {
    match rating {
        MediaRating::None => "none",
        MediaRating::Favorite => "favorite",
        MediaRating::Reject => "reject",
    }
}

fn media_rating_filter_id(filter: MediaRatingFilter) -> &'static str {
    match filter {
        MediaRatingFilter::All => "all",
        MediaRatingFilter::Favorite => "favorite",
        MediaRatingFilter::Reject => "reject",
        MediaRatingFilter::Unrated => "unrated",
    }
}

fn parse_media_kind_filter(id: Option<&str>) -> Option<MediaKindFilter> {
    match id {
        Some("all") => Some(MediaKindFilter::All),
        Some("video") => Some(MediaKindFilter::Video),
        Some("audio") => Some(MediaKindFilter::Audio),
        Some("image") => Some(MediaKindFilter::Image),
        Some("offline") => Some(MediaKindFilter::Offline),
        Some(_) => None,
        None => Some(MediaKindFilter::All),
    }
}

fn parse_resolution_filter(id: Option<&str>) -> Option<ResolutionFilter> {
    match id {
        Some("all") => Some(ResolutionFilter::All),
        Some("sd") => Some(ResolutionFilter::SdOrSmaller),
        Some("hd") => Some(ResolutionFilter::Hd),
        Some("fhd") => Some(ResolutionFilter::FullHd),
        Some("uhd") => Some(ResolutionFilter::UltraHd),
        Some(_) => None,
        None => Some(ResolutionFilter::All),
    }
}

fn parse_frame_rate_filter(id: Option<&str>) -> Option<FrameRateFilter> {
    match id {
        Some("all") => Some(FrameRateFilter::All),
        Some("fps24") => Some(FrameRateFilter::Fps24OrLess),
        Some("fps25_30") => Some(FrameRateFilter::Fps25To30),
        Some("fps31_59") => Some(FrameRateFilter::Fps31To59),
        Some("fps60") => Some(FrameRateFilter::Fps60Plus),
        Some(_) => None,
        None => Some(FrameRateFilter::All),
    }
}

fn parse_media_rating_filter(id: Option<&str>) -> Option<MediaRatingFilter> {
    match id {
        Some("all") => Some(MediaRatingFilter::All),
        Some("favorite") => Some(MediaRatingFilter::Favorite),
        Some("reject") => Some(MediaRatingFilter::Reject),
        Some("unrated") => Some(MediaRatingFilter::Unrated),
        Some(_) => None,
        None => Some(MediaRatingFilter::All),
    }
}

fn collection_criteria_from_mcp(
    search_text: Option<String>,
    kind: Option<String>,
    resolution: Option<String>,
    frame_rate: Option<String>,
    rating: Option<String>,
) -> Result<MediaFilterCriteria, String> {
    Ok(MediaFilterCriteria {
        search_text: search_text.unwrap_or_default(),
        kind: parse_media_kind_filter(kind.as_deref())
            .ok_or_else(|| "invalid kind filter".to_string())?,
        resolution: parse_resolution_filter(resolution.as_deref())
            .ok_or_else(|| "invalid resolution filter".to_string())?,
        frame_rate: parse_frame_rate_filter(frame_rate.as_deref())
            .ok_or_else(|| "invalid frame_rate filter".to_string())?,
        rating: parse_media_rating_filter(rating.as_deref())
            .ok_or_else(|| "invalid rating filter".to_string())?,
    })
}

fn format_source_keyword_time(ns: u64) -> String {
    let total_seconds = ns / 1_000_000_000;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn format_source_keyword_range(range: &MediaKeywordRange) -> String {
    let label = range.label.trim();
    let label = if label.is_empty() { "Untitled" } else { label };
    let start = format_source_keyword_time(range.start_ns);
    let end_ns = range.end_ns.max(range.start_ns);
    if end_ns == range.start_ns {
        format!("{label} @ {start}")
    } else {
        let end = format_source_keyword_time(end_ns);
        format!("{label} ({start} - {end})")
    }
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

fn clip_kind_supports_audio_match(kind: &ClipKind) -> bool {
    matches!(kind, ClipKind::Video | ClipKind::Audio)
}

const AUDIO_MATCH_SPEECH_PAD_NS: u64 = 80_000_000;

fn collect_audio_match_speech_regions(
    clip: &Clip,
) -> Vec<crate::media::audio_match::AnalysisRegionNs> {
    let clip_len_ns = clip.source_duration();
    if clip_len_ns == 0 {
        return Vec::new();
    }

    let mut regions = Vec::new();
    for segment in &clip.subtitle_segments {
        if segment.words.is_empty() {
            if let Some(region) =
                padded_audio_match_region(segment.start_ns, segment.end_ns, clip_len_ns)
            {
                regions.push(region);
            }
            continue;
        }

        for word in &segment.words {
            if let Some(region) = padded_audio_match_region(word.start_ns, word.end_ns, clip_len_ns)
            {
                regions.push(region);
            }
        }
    }

    merge_audio_match_speech_regions(regions)
}

fn padded_audio_match_region(
    start_ns: u64,
    end_ns: u64,
    clip_len_ns: u64,
) -> Option<crate::media::audio_match::AnalysisRegionNs> {
    let start_ns = start_ns.saturating_sub(AUDIO_MATCH_SPEECH_PAD_NS);
    let end_ns = end_ns
        .saturating_add(AUDIO_MATCH_SPEECH_PAD_NS)
        .min(clip_len_ns);
    (end_ns > start_ns).then_some(crate::media::audio_match::AnalysisRegionNs { start_ns, end_ns })
}

fn merge_audio_match_speech_regions(
    mut regions: Vec<crate::media::audio_match::AnalysisRegionNs>,
) -> Vec<crate::media::audio_match::AnalysisRegionNs> {
    if regions.is_empty() {
        return regions;
    }

    regions.sort_by_key(|region| region.start_ns);
    let mut merged = Vec::with_capacity(regions.len());
    let mut current = regions[0];
    for region in regions.into_iter().skip(1) {
        if region.start_ns <= current.end_ns {
            current.end_ns = current.end_ns.max(region.end_ns);
        } else {
            merged.push(current);
            current = region;
        }
    }
    merged.push(current);
    merged
}

#[derive(Debug, Clone)]
struct AudioMatchClipInfo {
    source_path: String,
    source_in: u64,
    source_out: u64,
    duration_ns: u64,
    speech_regions: Vec<crate::media::audio_match::AnalysisRegionNs>,
    volume: f32,
    measured_loudness_lufs: Option<f64>,
    eq_bands: [crate::model::clip::EqBand; 3],
    match_eq_bands: Vec<crate::model::clip::EqBand>,
    audio_channel_mode: crate::model::clip::AudioChannelMode,
    kind: ClipKind,
}

fn full_audio_match_region(duration_ns: u64) -> crate::media::audio_match::AnalysisRegionNs {
    crate::media::audio_match::AnalysisRegionNs {
        start_ns: 0,
        end_ns: duration_ns,
    }
}

fn resolve_audio_match_region(
    clip: &AudioMatchClipInfo,
    requested: Option<crate::media::audio_match::AnalysisRegionNs>,
    label: &str,
) -> Result<crate::media::audio_match::AnalysisRegionNs, String> {
    let region = requested.unwrap_or_else(|| full_audio_match_region(clip.duration_ns));
    if region.end_ns <= region.start_ns {
        return Err(format!("{label} range end must be after start."));
    }
    if region.end_ns > clip.duration_ns {
        return Err(format!("{label} range exceeds clip duration."));
    }
    Ok(region)
}

fn overlap_audio_match_regions(
    a: crate::media::audio_match::AnalysisRegionNs,
    b: crate::media::audio_match::AnalysisRegionNs,
) -> Option<crate::media::audio_match::AnalysisRegionNs> {
    let start_ns = a.start_ns.max(b.start_ns);
    let end_ns = a.end_ns.min(b.end_ns);
    (end_ns > start_ns).then_some(crate::media::audio_match::AnalysisRegionNs { start_ns, end_ns })
}

fn region_scoped_audio_match_clip_info(
    clip: &AudioMatchClipInfo,
    region: crate::media::audio_match::AnalysisRegionNs,
) -> AudioMatchClipInfo {
    let speech_regions = clip
        .speech_regions
        .iter()
        .filter_map(|speech_region| overlap_audio_match_regions(*speech_region, region))
        .map(|overlap| crate::media::audio_match::AnalysisRegionNs {
            start_ns: overlap.start_ns.saturating_sub(region.start_ns),
            end_ns: overlap.end_ns.saturating_sub(region.start_ns),
        })
        .collect();
    AudioMatchClipInfo {
        source_in: clip.source_in.saturating_add(region.start_ns),
        source_out: clip.source_in.saturating_add(region.end_ns),
        duration_ns: region.end_ns.saturating_sub(region.start_ns),
        speech_regions,
        ..clip.clone()
    }
}

#[derive(Debug, Clone)]
struct PreparedAudioMatch {
    clip_id: String,
    source_region: crate::media::audio_match::AnalysisRegionNs,
    reference_region: crate::media::audio_match::AnalysisRegionNs,
    source_channel_mode: crate::media::audio_match::AudioMatchChannelMode,
    reference_channel_mode: crate::media::audio_match::AudioMatchChannelMode,
    old_volume: f32,
    new_volume: f32,
    old_measured_loudness: Option<f64>,
    new_measured_loudness: Option<f64>,
    old_eq_bands: [crate::model::clip::EqBand; 3],
    new_eq_bands: [crate::model::clip::EqBand; 3],
    old_match_eq_bands: Vec<crate::model::clip::EqBand>,
    new_match_eq_bands: Vec<crate::model::clip::EqBand>,
    source_loudness_lufs: f64,
    reference_loudness_lufs: f64,
    volume_gain: f64,
    source_profile: crate::media::audio_match::SpectralProfile,
    reference_profile: crate::media::audio_match::SpectralProfile,
}

fn collect_audio_match_clip_info(project: &Project, clip_id: &str) -> Option<AudioMatchClipInfo> {
    let clip = project.clip_ref(clip_id)?;
    Some(AudioMatchClipInfo {
        source_path: clip.source_path.clone(),
        source_in: clip.source_in,
        source_out: clip.source_out,
        duration_ns: clip.source_duration(),
        speech_regions: collect_audio_match_speech_regions(clip),
        volume: clip.volume,
        measured_loudness_lufs: clip.measured_loudness_lufs,
        eq_bands: clip.eq_bands,
        match_eq_bands: clip.match_eq_bands.clone(),
        audio_channel_mode: clip.audio_channel_mode,
        kind: clip.kind.clone(),
    })
}

fn run_audio_match_for_clips(
    source_clip_id: &str,
    source: &AudioMatchClipInfo,
    source_region: Option<crate::media::audio_match::AnalysisRegionNs>,
    source_channel_mode: crate::media::audio_match::AudioMatchChannelMode,
    reference_clip_id: &str,
    reference: &AudioMatchClipInfo,
    reference_region: Option<crate::media::audio_match::AnalysisRegionNs>,
    reference_channel_mode: crate::media::audio_match::AudioMatchChannelMode,
) -> Result<PreparedAudioMatch, String> {
    if source_clip_id == reference_clip_id {
        return Err("Source and reference clips must be different.".to_string());
    }

    if !clip_kind_supports_audio_match(&source.kind) {
        return Err("Source clip does not support audio matching.".to_string());
    }
    if !clip_kind_supports_audio_match(&reference.kind) {
        return Err("Reference clip does not support audio matching.".to_string());
    }

    let source_region = resolve_audio_match_region(source, source_region, "Source")?;
    let reference_region = resolve_audio_match_region(reference, reference_region, "Reference")?;
    let source = region_scoped_audio_match_clip_info(source, source_region);
    let reference = region_scoped_audio_match_clip_info(reference, reference_region);

    let outcome =
        crate::media::audio_match::run_audio_match(&crate::media::audio_match::AudioMatchParams {
            source_path: source.source_path.clone(),
            source_in_ns: source.source_in,
            source_out_ns: source.source_out,
            source_speech_regions: source.speech_regions.clone(),
            source_channel_mode,
            source_clip_channel_mode: source.audio_channel_mode,
            reference_path: reference.source_path.clone(),
            reference_in_ns: reference.source_in,
            reference_out_ns: reference.source_out,
            reference_speech_regions: reference.speech_regions.clone(),
            reference_channel_mode,
            reference_clip_channel_mode: reference.audio_channel_mode,
        })
        .map_err(|e| e.to_string())?;

    Ok(PreparedAudioMatch {
        clip_id: source_clip_id.to_string(),
        source_region,
        reference_region,
        source_channel_mode: outcome.source_resolved_channel_mode,
        reference_channel_mode: outcome.reference_resolved_channel_mode,
        old_volume: source.volume,
        new_volume: (source.volume as f64 * outcome.volume_gain).clamp(0.0, 4.0) as f32,
        old_measured_loudness: source.measured_loudness_lufs,
        new_measured_loudness: Some(outcome.source_loudness_lufs),
        old_eq_bands: source.eq_bands,
        new_eq_bands: outcome.eq_bands,
        old_match_eq_bands: source.match_eq_bands.clone(),
        new_match_eq_bands: outcome.match_eq_bands,
        source_loudness_lufs: outcome.source_loudness_lufs,
        reference_loudness_lufs: outcome.reference_loudness_lufs,
        volume_gain: outcome.volume_gain,
        source_profile: outcome.source_profile,
        reference_profile: outcome.reference_profile,
    })
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
    use crate::model::transform_bounds::{
        CROP_MAX_PX, CROP_MIN_PX, ROTATE_MAX_DEG, ROTATE_MIN_DEG,
    };
    let rotate = clip
        .value_for_phase1_property_at_timeline_ns(Phase1KeyframeProperty::Rotate, playhead_ns)
        .round()
        .clamp(ROTATE_MIN_DEG, ROTATE_MAX_DEG) as i32;
    let crop_left = clip
        .value_for_phase1_property_at_timeline_ns(Phase1KeyframeProperty::CropLeft, playhead_ns)
        .round()
        .clamp(CROP_MIN_PX, CROP_MAX_PX) as i32;
    let crop_right = clip
        .value_for_phase1_property_at_timeline_ns(Phase1KeyframeProperty::CropRight, playhead_ns)
        .round()
        .clamp(CROP_MIN_PX, CROP_MAX_PX) as i32;
    let crop_top = clip
        .value_for_phase1_property_at_timeline_ns(Phase1KeyframeProperty::CropTop, playhead_ns)
        .round()
        .clamp(CROP_MIN_PX, CROP_MAX_PX) as i32;
    let crop_bottom = clip
        .value_for_phase1_property_at_timeline_ns(Phase1KeyframeProperty::CropBottom, playhead_ns)
        .round()
        .clamp(CROP_MIN_PX, CROP_MAX_PX) as i32;
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

fn evaluate_mask_geometry_at_local_ns(
    mask: &crate::model::clip::ClipMask,
    local_time_ns: u64,
) -> (f64, f64, f64, f64, f64) {
    (
        Clip::evaluate_keyframed_value(&mask.center_x_keyframes, local_time_ns, mask.center_x),
        Clip::evaluate_keyframed_value(&mask.center_y_keyframes, local_time_ns, mask.center_y),
        Clip::evaluate_keyframed_value(&mask.width_keyframes, local_time_ns, mask.width),
        Clip::evaluate_keyframed_value(&mask.height_keyframes, local_time_ns, mask.height),
        Clip::evaluate_keyframed_value(&mask.rotation_keyframes, local_time_ns, mask.rotation),
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
    // Default content inset to 0 (will be overridden by caller if program player available)
    transform_overlay.set_content_inset(0.0, 0.0);
    match selected_clip_id {
        Some(cid) => {
            if let Some(c) = project.clip_ref(cid) {
                if c.kind != ClipKind::Audio {
                    transform_overlay.set_adjustment_mode(clip_uses_direct_position_mode(c));
                    let (scale, pos_x, pos_y, rotate, cl, cr, ct, cb) =
                        evaluate_clip_transform_at(c, playhead_ns);
                    transform_overlay.set_transform(scale, pos_x, pos_y);
                    transform_overlay.set_rotation(rotate);
                    transform_overlay.set_crop(cl, cr, ct, cb);
                    if let Some(mask) = c.masks.first() {
                        let local_time_ns = c.local_timeline_position_ns(playhead_ns);
                        let (center_x, center_y, width, height, rotation) =
                            evaluate_mask_geometry_at_local_ns(mask, local_time_ns);
                        transform_overlay.set_mask(
                            mask.enabled,
                            match mask.shape {
                                crate::model::clip::MaskShape::Rectangle => 0,
                                crate::model::clip::MaskShape::Ellipse => 1,
                                crate::model::clip::MaskShape::Path => 2,
                            },
                            center_x,
                            center_y,
                            width,
                            height,
                            rotation,
                            mask.path.as_ref().map(|p| p.points.as_slice()),
                        );
                    } else {
                        transform_overlay.set_mask(false, 0, 0.5, 0.5, 0.25, 0.25, 0.0, None);
                    }
                    transform_overlay.set_clip_selected(true);
                } else {
                    transform_overlay.set_adjustment_mode(false);
                    transform_overlay.set_clip_selected(false);
                }
            } else {
                transform_overlay.set_adjustment_mode(false);
                transform_overlay.set_clip_selected(false);
            }
        }
        None => {
            transform_overlay.set_adjustment_mode(false);
            transform_overlay.set_clip_selected(false);
        }
    }
}

fn sync_transform_overlay_to_playhead_from_program_clips(
    transform_overlay: &crate::ui::transform_overlay::TransformOverlay,
    clips: &[ProgramClip],
    selected_clip_id: Option<&str>,
    playhead_ns: u64,
) {
    transform_overlay.set_content_inset(0.0, 0.0);
    match selected_clip_id {
        Some(cid) => {
            if let Some(clip) = clips.iter().find(|clip| clip.id == cid) {
                if !clip.is_audio_only {
                    transform_overlay
                        .set_adjustment_mode(program_clip_uses_direct_position_mode(clip));
                    transform_overlay.set_transform(
                        clip.scale_at_timeline_ns(playhead_ns),
                        clip.position_x_at_timeline_ns(playhead_ns),
                        clip.position_y_at_timeline_ns(playhead_ns),
                    );
                    transform_overlay.set_rotation(clip.rotate_at_timeline_ns(playhead_ns));
                    transform_overlay.set_crop(
                        clip.crop_left_at_timeline_ns(playhead_ns),
                        clip.crop_right_at_timeline_ns(playhead_ns),
                        clip.crop_top_at_timeline_ns(playhead_ns),
                        clip.crop_bottom_at_timeline_ns(playhead_ns),
                    );
                    if let Some(mask) = clip.masks.first() {
                        let local_time_ns = clip.local_timeline_position_ns(playhead_ns);
                        let (center_x, center_y, width, height, rotation) =
                            evaluate_mask_geometry_at_local_ns(mask, local_time_ns);
                        transform_overlay.set_mask(
                            mask.enabled,
                            match mask.shape {
                                crate::model::clip::MaskShape::Rectangle => 0,
                                crate::model::clip::MaskShape::Ellipse => 1,
                                crate::model::clip::MaskShape::Path => 2,
                            },
                            center_x,
                            center_y,
                            width,
                            height,
                            rotation,
                            mask.path.as_ref().map(|p| p.points.as_slice()),
                        );
                    } else {
                        transform_overlay.set_mask(false, 0, 0.5, 0.5, 0.25, 0.25, 0.0, None);
                    }
                    transform_overlay.set_clip_selected(true);
                    return;
                }
            }
        }
        None => {}
    }
    transform_overlay.set_clip_selected(false);
    transform_overlay.set_adjustment_mode(false);
}

fn sync_transform_overlay_to_playhead_resolved(
    transform_overlay: &crate::ui::transform_overlay::TransformOverlay,
    project: &Project,
    program_player: &ProgramPlayer,
    selected_clip_id: Option<&str>,
    playhead_ns: u64,
) {
    if let Some(cid) = selected_clip_id {
        if let Some(runtime_clip) = program_player.visual_clip_snapshot(cid) {
            sync_transform_overlay_to_playhead_from_program_clips(
                transform_overlay,
                std::slice::from_ref(&runtime_clip),
                Some(cid),
                playhead_ns,
            );
            return;
        }
    }
    sync_transform_overlay_to_playhead(transform_overlay, project, selected_clip_id, playhead_ns);
}

fn clip_uses_direct_position_mode(clip: &Clip) -> bool {
    matches!(clip.kind, ClipKind::Adjustment | ClipKind::Title) || clip.tracking_binding.is_some()
}

fn program_clip_uses_direct_position_mode(clip: &ProgramClip) -> bool {
    clip.is_adjustment || clip.is_title || clip.tracking_binding.is_some()
}

fn sync_transform_overlay_tracking_region(
    transform_overlay: &crate::ui::transform_overlay::TransformOverlay,
    project: &Project,
    selected_clip_id: Option<&str>,
    selected_tracker_id: Option<&str>,
    editing: bool,
) {
    let tracker = selected_clip_id
        .and_then(|clip_id| project.clip_ref(clip_id))
        .and_then(|clip| {
            selected_tracker_id.and_then(|tracker_id| clip.motion_tracker_ref(tracker_id))
        });
    if let Some(tracker) = tracker {
        transform_overlay.set_tracking_region(
            true,
            editing,
            tracker.analysis_region.center_x,
            tracker.analysis_region.center_y,
            tracker.analysis_region.width,
            tracker.analysis_region.height,
            tracker.analysis_region.rotation_deg,
        );
    } else {
        transform_overlay.set_tracking_region(false, false, 0.5, 0.5, 0.25, 0.25, 0.0);
    }
}

/// Default extra headroom around a tracked region when auto-cropping.
const AUTO_CROP_DEFAULT_PADDING: f64 = 0.1;

/// Outcome of a single [`run_auto_crop_track_for_clip`] invocation. The
/// string messages are meant to be surfaced through the tracking status
/// bar in the inspector.
#[derive(Debug, Clone)]
enum AutoCropOutcome {
    /// Cached tracker samples were applied immediately; the clip is
    /// already reframed at the project aspect ratio.
    Ok { message: String },
    /// A new tracking job was enqueued in the background. The binding is
    /// already set so as soon as samples arrive the compositor will pick
    /// up the reframe.
    Queued { message: String },
    /// Setup failed (wrong clip kind, missing dimensions, no region). The
    /// clip state is unchanged.
    Err { message: String },
}

/// Install an auto-crop-and-track binding on `clip_id` that uses the
/// existing motion tracker `tracker_id` as its motion source, then
/// enqueue (or look up a cached) tracking job to populate the samples.
///
/// Returns `(outcome, Some(command))` — the command is an undoable
/// [`SetClipAutoCropCommand`] snapshotting the old + new state. The
/// command has **not** been executed yet; the caller must push it
/// through `on_execute_command` / `history.execute` so the mutation
/// lands in the undo stack.
fn run_auto_crop_track_for_clip(
    project: &Rc<RefCell<Project>>,
    tracking_cache: &Rc<RefCell<crate::media::tracking::TrackingCache>>,
    tracking_job_owner_by_key: &Rc<RefCell<HashMap<String, String>>>,
    tracking_job_key_by_clip: &Rc<RefCell<HashMap<String, String>>>,
    clip_id: &str,
    tracker_id: &str,
    padding: f64,
) -> (AutoCropOutcome, Option<Box<dyn EditCommand>>) {
    // Snapshot everything we need from the project under a short borrow.
    let (
        project_width,
        project_height,
        job,
        source_path,
        region,
        old_tracking_binding,
        old_motion_trackers,
        old_first_mask_binding,
    ) = {
        let proj = project.borrow();
        let project_width = proj.width;
        let project_height = proj.height;
        let Some(clip) = proj.clip_ref(clip_id) else {
            return (
                AutoCropOutcome::Err {
                    message: "Selected clip no longer exists.".to_string(),
                },
                None,
            );
        };
        if let Err(message) = clip_supports_tracking_analysis(clip) {
            return (
                AutoCropOutcome::Err {
                    message: message.to_string(),
                },
                None,
            );
        }
        let Some(tracker) = clip.motion_tracker_ref(tracker_id) else {
            return (
                AutoCropOutcome::Err {
                    message: "Select a tracker before auto-cropping.".to_string(),
                },
                None,
            );
        };
        let region = tracker.analysis_region;
        let analysis_start_ns = tracker.analysis_start_ns.min(clip.source_duration() - 1);
        let mut analysis_end_ns = tracker
            .analysis_end_ns
            .unwrap_or_else(|| clip.source_duration())
            .min(clip.source_duration());
        if analysis_end_ns <= analysis_start_ns {
            analysis_end_ns = clip.source_duration();
        }
        if analysis_end_ns <= analysis_start_ns {
            return (
                AutoCropOutcome::Err {
                    message: "Auto-crop needs a non-empty source range.".to_string(),
                },
                None,
            );
        }
        let mut job = crate::media::tracking::TrackingJob::new(
            tracker.id.clone(),
            tracker.label.clone(),
            clip.source_path.clone(),
            clip.source_in,
            analysis_start_ns,
            analysis_end_ns,
            region,
        );
        // Resolve "every source frame" into a concrete step (ns per
        // frame) via the probe cache. Done now, before the job goes
        // into the cache key, so repeat requests for the same source
        // hit the same cache entry.
        job.frame_step_ns =
            crate::media::tracking::source_frame_step_ns(&clip.source_path);
        let source_path = clip.source_path.clone();
        let old_tracking_binding = clip.tracking_binding.clone();
        let old_motion_trackers = clip.motion_trackers.clone();
        let old_first_mask_binding = clip
            .masks
            .first()
            .map(|mask| mask.tracking_binding.clone());
        (
            project_width,
            project_height,
            job,
            source_path,
            region,
            old_tracking_binding,
            old_motion_trackers,
            old_first_mask_binding,
        )
    };

    // Resolve source pixel dimensions. Blocking ffprobe/GStreamer call on
    // first invocation; cached for subsequent probes.
    let metadata = crate::media::probe_cache::probe_media_metadata(&source_path);
    let source_width = metadata.video_width.unwrap_or(0);
    let source_height = metadata.video_height.unwrap_or(0);
    if source_width == 0 || source_height == 0 {
        return (
            AutoCropOutcome::Err {
                message: "Could not determine source clip dimensions for auto-crop."
                    .to_string(),
            },
            None,
        );
    }

    // Compute the binding transform from the drawn region + aspects.
    let binding = crate::media::tracking::compute_auto_crop_binding(
        clip_id,
        tracker_id,
        &crate::media::tracking::AutoCropInputs {
            region,
            source_width,
            source_height,
            project_width,
            project_height,
            padding,
        },
    );

    // Enqueue / look up the tracking job. Same flow as the Track Region
    // button, but we *always* want an analysis pass for the auto-crop
    // use case so motion samples keep the region centered over time.
    // The cache lookup happens *before* we build the new motion_trackers
    // snapshot so any cached samples get folded into the undo state.
    let (cache_key, cached_tracker, pending) = {
        let cache_key = tracking_cache.borrow_mut().request(job.clone());
        let cached_tracker = tracking_cache.borrow().get_for_job(&job);
        let pending = tracking_cache.borrow().job_progress(&cache_key).is_some();
        (cache_key, cached_tracker, pending)
    };

    // Build the new motion_trackers vec: copy the old list, then upsert
    // the cached tracker samples onto the matching tracker (if any).
    let mut new_motion_trackers = old_motion_trackers.clone();
    let mut sample_count = 0usize;
    if let Some(tracker) = &cached_tracker {
        sample_count = tracker.samples.len();
        if let Some(existing) = new_motion_trackers
            .iter_mut()
            .find(|t| t.id == tracker.id)
        {
            *existing = tracker.clone();
        } else {
            new_motion_trackers.push(tracker.clone());
        }
    }

    // First-mask binding is always cleared when auto-cropping (the clip's
    // own transform owns the binding). `None` here means "no mask on the
    // clip at all — no mask state to restore"; `Some(None)` means "mask
    // exists, clear its binding".
    let new_first_mask_binding = old_first_mask_binding.as_ref().map(|_| None);

    let command = SetClipAutoCropCommand {
        clip_id: clip_id.to_string(),
        old_tracking_binding,
        old_motion_trackers,
        old_first_mask_binding,
        new_tracking_binding: Some(binding),
        new_motion_trackers,
        new_first_mask_binding,
    };

    let outcome = if cached_tracker.is_some() {
        AutoCropOutcome::Ok {
            message: format!(
                "Auto-crop active at project aspect ({sample_count} tracked samples)."
            ),
        }
    } else if pending {
        tracking_job_owner_by_key
            .borrow_mut()
            .insert(cache_key.clone(), clip_id.to_string());
        tracking_job_key_by_clip
            .borrow_mut()
            .insert(clip_id.to_string(), cache_key);
        AutoCropOutcome::Queued {
            message: "Auto-crop framing applied. Tracking…".to_string(),
        }
    } else {
        AutoCropOutcome::Err {
            message: "Failed to queue auto-crop tracking analysis.".to_string(),
        }
    };

    (outcome, Some(Box::new(command)))
}

/// Re-apply the auto-crop framing on a clip in-place (no undo step).
///
/// Called by the tracker-region / padding sliders so the crop stays
/// locked onto the region as the user drags. Skips clips that don't
/// currently have an auto-crop binding (i.e., where `tracking_binding`
/// points at a tracker on a different clip — that's the manual
/// "follow tracker" case, which should not get rewritten under the
/// user's feet).
///
/// Returns `true` when the binding was recomputed, `false` when the
/// clip wasn't in an auto-crop state or we couldn't resolve source
/// dimensions.  No-op on failure.
fn reapply_auto_crop_in_place(
    project: &Rc<RefCell<Project>>,
    library: &Rc<RefCell<MediaLibrary>>,
    clip_id: &str,
    padding: f64,
) -> bool {
    // Snapshot the data we need from the project under a short borrow.
    let (project_width, project_height, source_path, region, tracker_id) = {
        let proj = project.borrow();
        let Some(clip) = proj.clip_ref(clip_id) else {
            return false;
        };
        let Some(binding) = clip.tracking_binding.as_ref() else {
            return false;
        };
        // Only auto-crop bindings point at a tracker on the clip itself.
        if binding.source_clip_id != clip_id {
            return false;
        }
        let tracker_id = binding.tracker_id.clone();
        let Some(tracker) = clip.motion_tracker_ref(&tracker_id) else {
            return false;
        };
        (
            proj.width,
            proj.height,
            clip.source_path.clone(),
            tracker.analysis_region,
            tracker_id,
        )
    };

    // Resolve source dims from the media library first (cheap cache
    // hit), fall back to a synchronous ffprobe only if the item hasn't
    // been probed yet.  The ffprobe path is ~100ms so we avoid it on
    // slider drags when possible.
    let (source_width, source_height) = {
        let lib = library.borrow();
        let item = lib.items.iter().find(|i| i.source_path == source_path);
        match item {
            Some(i) if i.video_width.is_some() && i.video_height.is_some() => {
                (i.video_width.unwrap(), i.video_height.unwrap())
            }
            _ => {
                drop(lib);
                let metadata = crate::media::probe_cache::probe_media_metadata(&source_path);
                (
                    metadata.video_width.unwrap_or(0),
                    metadata.video_height.unwrap_or(0),
                )
            }
        }
    };
    if source_width == 0 || source_height == 0 {
        return false;
    }

    let values = crate::media::tracking::compute_auto_crop_binding_values(
        &crate::media::tracking::AutoCropInputs {
            region,
            source_width,
            source_height,
            project_width,
            project_height,
            padding,
        },
    );

    // Update the binding in place.
    let mut proj = project.borrow_mut();
    let Some(clip) = proj.clip_mut(clip_id) else {
        return false;
    };
    let Some(binding) = clip.tracking_binding.as_mut() else {
        return false;
    };
    if binding.tracker_id != tracker_id || binding.source_clip_id != clip_id {
        return false;
    }
    binding.scale_multiplier = values.scale_multiplier;
    binding.offset_x = values.offset_x;
    binding.offset_y = values.offset_y;
    proj.dirty = true;
    true
}

fn upsert_motion_tracker_on_clip(
    project: &mut Project,
    clip_id: &str,
    tracker: crate::model::clip::MotionTracker,
) -> bool {
    if let Some(clip) = project.clip_mut(clip_id) {
        if let Some(existing) = clip.motion_tracker_mut(&tracker.id) {
            *existing = tracker;
        } else {
            clip.motion_trackers.push(tracker);
        }
        project.dirty = true;
        true
    } else {
        false
    }
}

fn apply_tracking_binding_selection(
    clip: &mut Clip,
    target_is_mask: bool,
    reference: Option<&crate::model::project::MotionTrackerReference>,
) -> bool {
    let before = (
        clip.tracking_binding.clone(),
        clip.masks
            .first()
            .and_then(|mask| mask.tracking_binding.clone()),
    );
    clip.tracking_binding = None;
    if let Some(mask) = clip.masks.first_mut() {
        mask.tracking_binding = None;
    }
    if let Some(reference) = reference {
        let binding = crate::model::clip::TrackingBinding::new(
            reference.source_clip_id.clone(),
            reference.tracker_id.clone(),
        );
        if target_is_mask && !clip.masks.is_empty() {
            if let Some(mask) = clip.masks.first_mut() {
                mask.tracking_binding = Some(binding);
            }
        } else {
            clip.tracking_binding = Some(binding);
        }
    }
    let after = (
        clip.tracking_binding.clone(),
        clip.masks
            .first()
            .and_then(|mask| mask.tracking_binding.clone()),
    );
    before != after
}

fn clip_supports_tracking_analysis(clip: &Clip) -> Result<(), &'static str> {
    match clip.kind {
        ClipKind::Video => {}
        _ => {
            return Err("Tracking analysis currently requires a video clip with decodable frames.")
        }
    }
    if clip.source_path.trim().is_empty() {
        return Err("Tracking analysis is unavailable because this clip has no source media path.");
    }
    if (clip.speed - 1.0).abs() > f64::EPSILON || !clip.speed_keyframes.is_empty() {
        return Err("Tracking analysis currently requires an unretimed source clip.");
    }
    if clip.source_duration() == 0 {
        return Err("Tracking analysis needs a clip with visible source duration.");
    }
    Ok(())
}

fn selected_clip_is_adjustment(project: &Project, selected_clip_id: Option<&str>) -> bool {
    selected_clip_id
        .and_then(|cid| project.clip_ref(cid))
        .map(|clip| clip.kind == ClipKind::Adjustment)
        .unwrap_or(false)
}

fn selected_clip_is_static_image(project: &Project, selected_clip_id: Option<&str>) -> bool {
    selected_clip_id
        .and_then(|cid| project.clip_ref(cid))
        .map(|clip| clip.kind == ClipKind::Image && !clip.animated_svg)
        .unwrap_or(false)
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

#[allow(deprecated)]
fn present_text_entry_dialog(
    window: &gtk::ApplicationWindow,
    title: &str,
    accept_label: &str,
    hint: &str,
    initial_text: &str,
    placeholder: Option<&str>,
    on_accept: Rc<dyn Fn(String) -> Result<(), String>>,
) {
    let dialog = gtk::Dialog::builder()
        .title(title)
        .transient_for(window)
        .modal(true)
        .default_width(360)
        .build();
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button(accept_label, gtk::ResponseType::Accept);
    dialog.set_default_response(gtk::ResponseType::Accept);

    let content = dialog.content_area();
    if !hint.is_empty() {
        let hint_label = gtk::Label::new(Some(hint));
        hint_label.set_halign(gtk::Align::Start);
        hint_label.add_css_class("dim-label");
        hint_label.set_wrap(true);
        content.append(&hint_label);
    }

    let entry = gtk::Entry::new();
    entry.set_text(initial_text);
    entry.set_placeholder_text(placeholder);
    entry.set_activates_default(true);
    content.append(&entry);

    let error_label = gtk::Label::new(None);
    error_label.set_halign(gtk::Align::Start);
    error_label.set_wrap(true);
    error_label.add_css_class("error");
    error_label.set_visible(false);
    content.append(&error_label);

    entry.connect_changed({
        let error_label = error_label.clone();
        move |_| error_label.set_visible(false)
    });

    let entry_for_response = entry.clone();
    dialog.connect_response(move |d, resp| {
        if resp != gtk::ResponseType::Accept {
            d.close();
            return;
        }
        match on_accept(entry_for_response.text().to_string()) {
            Ok(()) => d.close(),
            Err(err) => {
                error_label.set_text(&err);
                error_label.set_visible(true);
            }
        }
    });

    dialog.present();
    entry.grab_focus();
    entry.select_region(0, -1);
}

fn lookup_source_timecode_base_ns_in_tracks(
    tracks: &[crate::model::track::Track],
    source_path: &str,
) -> Option<u64> {
    for track in tracks {
        for clip in &track.clips {
            if clip.source_path == source_path {
                if let Some(base) = clip.source_timecode_base_ns {
                    return Some(base);
                }
            }
            if let Some(ref compound_tracks) = clip.compound_tracks {
                if let Some(base) =
                    lookup_source_timecode_base_ns_in_tracks(compound_tracks, source_path)
                {
                    return Some(base);
                }
            }
        }
    }
    None
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
        .or_else(|| lookup_source_timecode_base_ns_in_tracks(&project.tracks, source_path))
}

fn lookup_source_audio_channel_mode_in_tracks(
    tracks: &[crate::model::track::Track],
    source_path: &str,
    fallback: &mut Option<AudioChannelMode>,
) -> Option<AudioChannelMode> {
    for track in tracks {
        for clip in &track.clips {
            if clip.source_path == source_path {
                if fallback.is_none() {
                    *fallback = Some(clip.audio_channel_mode);
                }
                if clip.audio_channel_mode != AudioChannelMode::Stereo {
                    return Some(clip.audio_channel_mode);
                }
            }
            if let Some(ref compound_tracks) = clip.compound_tracks {
                if let Some(mode) = lookup_source_audio_channel_mode_in_tracks(
                    compound_tracks,
                    source_path,
                    fallback,
                ) {
                    return Some(mode);
                }
            }
        }
    }
    None
}

fn lookup_source_audio_channel_mode(project: &Project, source_path: &str) -> AudioChannelMode {
    let mut fallback = None;
    lookup_source_audio_channel_mode_in_tracks(&project.tracks, source_path, &mut fallback)
        .or(fallback)
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq)]
struct ProjectLibraryEntry {
    sync_key: String,
    item_id: String,
    source_path: String,
    duration_ns: u64,
    source_timecode_base_ns: Option<u64>,
    is_animated_svg: bool,
    clip_kind: Option<ClipKind>,
    label: String,
    title_text: Option<String>,
}

impl ProjectLibraryEntry {
    fn from_clip(clip: &Clip) -> Self {
        let clip_kind = clip.source_path.is_empty().then_some(clip.kind.clone());
        let title_text = if clip.kind == ClipKind::Title && !clip.title_text.trim().is_empty() {
            Some(clip.title_text.clone())
        } else {
            None
        };
        Self {
            sync_key: if clip.source_path.is_empty() {
                format!("clip:{}", clip.id)
            } else {
                clip.source_path.clone()
            },
            item_id: clip.id.clone(),
            source_path: clip.source_path.clone(),
            duration_ns: if clip.source_path.is_empty() {
                clip.duration()
            } else {
                clip.media_duration_ns.unwrap_or(0)
            },
            source_timecode_base_ns: clip.source_timecode_base_ns,
            is_animated_svg: clip.animated_svg,
            clip_kind,
            label: clip.label.clone(),
            title_text,
        }
    }

    fn apply_to_item(&self, item: &mut MediaItem) {
        if self.source_path.is_empty() {
            item.duration_ns = self.duration_ns;
            item.is_audio_only = false;
            item.has_audio = false;
            item.is_image = false;
            item.video_width = None;
            item.video_height = None;
            item.frame_rate_num = None;
            item.frame_rate_den = None;
            item.codec_summary = None;
            item.file_size_bytes = None;
            item.is_missing = false;
            item.label = self.label.clone();
            item.clip_kind = self.clip_kind.clone();
            item.title_text = self.title_text.clone();
        } else if item.duration_ns == 0 && self.duration_ns > 0 {
            item.duration_ns = self.duration_ns;
        }
        if item.source_timecode_base_ns.is_none() && self.source_timecode_base_ns.is_some() {
            item.source_timecode_base_ns = self.source_timecode_base_ns;
        }
        item.is_animated_svg = self.is_animated_svg;
    }

    fn into_media_item(self) -> MediaItem {
        let mut item = MediaItem::new(self.source_path, self.duration_ns);
        item.id = self.item_id;
        item.source_timecode_base_ns = self.source_timecode_base_ns;
        item.is_animated_svg = self.is_animated_svg;
        item.clip_kind = self.clip_kind;
        item.title_text = self.title_text;
        if !item.has_backing_file() {
            item.label = self.label;
            item.is_missing = false;
        }
        item
    }
}

fn collect_project_library_entries(project: &Project) -> Vec<ProjectLibraryEntry> {
    let mut seen = HashSet::new();
    project
        .tracks
        .iter()
        .flat_map(|track| track.clips.iter())
        .filter_map(|clip| {
            let entry = ProjectLibraryEntry::from_clip(clip);
            seen.insert(entry.sync_key.clone()).then_some(entry)
        })
        .collect()
}

fn sync_library_with_project_entries(library: &mut MediaLibrary, entries: &[ProjectLibraryEntry]) {
    for entry in entries {
        if let Some(item) = library
            .items
            .iter_mut()
            .find(|item| item.matches_library_key(&entry.sync_key))
        {
            entry.apply_to_item(item);
        }
    }

    let existing: HashSet<String> = library.items.iter().map(MediaItem::library_key).collect();
    for entry in entries {
        if !existing.contains(&entry.sync_key) {
            library.items.push(entry.clone().into_media_item());
        }
    }
}

fn collect_media_source_paths(project: &Project, library: &[MediaItem]) -> HashSet<String> {
    let mut paths: HashSet<String> = project
        .tracks
        .iter()
        .flat_map(|track| track.clips.iter())
        .filter(|clip| !clip.source_path.is_empty())
        .map(|clip| clip.source_path.clone())
        .collect();
    paths.extend(
        library
            .iter()
            .filter(|item| !item.source_path.is_empty())
            .map(|item| item.source_path.clone()),
    );
    paths
}

fn build_media_availability_index(
    project: &Project,
    library: &[MediaItem],
) -> HashMap<String, bool> {
    let mut availability = HashMap::new();
    for path in collect_media_source_paths(project, library) {
        availability.insert(
            path.clone(),
            crate::model::media_library::source_path_exists(&path),
        );
    }
    availability
}

fn refresh_media_availability_state(
    project: &Project,
    library: &mut [MediaItem],
    timeline_state: &mut TimelineState,
) -> HashSet<String> {
    let availability = build_media_availability_index(project, library);
    let missing_paths: HashSet<String> = availability
        .iter()
        .filter_map(|(path, exists)| if *exists { None } else { Some(path.clone()) })
        .collect();
    for item in library.iter_mut() {
        item.is_missing = missing_paths.contains(&item.source_path);
    }
    timeline_state.missing_media_paths = missing_paths.clone();
    missing_paths
}

fn collect_missing_source_paths(project: &Project, library: &[MediaItem]) -> Vec<String> {
    let availability = build_media_availability_index(project, library);
    let mut missing: Vec<String> = availability
        .into_iter()
        .filter_map(|(path, exists)| if exists { None } else { Some(path) })
        .collect();
    missing.sort_unstable();
    missing
}

fn collect_files_recursive(root: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files_recursive(&path, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

fn path_tail_match_score(original: &std::path::Path, candidate: &std::path::Path) -> usize {
    let orig_parts: Vec<String> = original
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    let cand_parts: Vec<String> = candidate
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    let mut score = 0usize;
    while score < orig_parts.len() && score < cand_parts.len() {
        let oi = orig_parts.len() - 1 - score;
        let ci = cand_parts.len() - 1 - score;
        if !orig_parts[oi].eq_ignore_ascii_case(&cand_parts[ci]) {
            break;
        }
        score += 1;
    }
    score
}

fn choose_relink_candidate(
    original_path: &str,
    candidates: &[std::path::PathBuf],
) -> Option<std::path::PathBuf> {
    if candidates.len() == 1 {
        return candidates.first().cloned();
    }
    let original = std::path::Path::new(original_path);
    let original_depth = original.components().count() as i64;
    let mut best_score = 0usize;
    let mut best_depth_delta = i64::MAX;
    let mut best_path: Option<std::path::PathBuf> = None;
    for candidate in candidates {
        let score = path_tail_match_score(original, candidate);
        if score == 0 {
            continue;
        }
        let depth_delta = (candidate.components().count() as i64 - original_depth).abs();
        let candidate_str = candidate.to_string_lossy();
        let best_str = best_path.as_ref().map(|p| p.to_string_lossy());
        if score > best_score {
            best_score = score;
            best_depth_delta = depth_delta;
            best_path = Some(candidate.clone());
            continue;
        }
        if score == best_score && depth_delta < best_depth_delta {
            best_depth_delta = depth_delta;
            best_path = Some(candidate.clone());
            continue;
        }
        if score == best_score
            && depth_delta == best_depth_delta
            && best_str
                .as_ref()
                .is_none_or(|best| candidate_str.as_ref() < best.as_ref())
        {
            best_path = Some(candidate.clone());
        }
    }
    best_path
}

#[derive(Debug, Clone)]
struct RelinkSummary {
    scanned_files: usize,
    remapped: Vec<(String, String)>,
    unresolved: Vec<String>,
    updated_clip_count: usize,
    updated_library_count: usize,
}

fn relink_missing_media_under_root(
    project: &mut Project,
    library: &mut [MediaItem],
    root: &std::path::Path,
) -> RelinkSummary {
    let missing = collect_missing_source_paths(project, library);
    let mut scanned_files: Vec<std::path::PathBuf> = Vec::new();
    collect_files_recursive(root, &mut scanned_files);

    let mut by_name: HashMap<String, Vec<std::path::PathBuf>> = HashMap::new();
    for path in &scanned_files {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        by_name
            .entry(name.to_ascii_lowercase())
            .or_default()
            .push(path.clone());
    }

    let mut remapped: Vec<(String, String)> = Vec::new();
    let mut unresolved: Vec<String> = Vec::new();
    for original in missing {
        let key = std::path::Path::new(&original)
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_ascii_lowercase());
        let Some(key) = key else {
            unresolved.push(original);
            continue;
        };
        let Some(candidates) = by_name.get(&key) else {
            unresolved.push(original);
            continue;
        };
        let Some(chosen) = choose_relink_candidate(&original, candidates) else {
            unresolved.push(original);
            continue;
        };
        remapped.push((original, chosen.to_string_lossy().to_string()));
    }

    let remap_map: HashMap<String, String> = remapped.iter().cloned().collect();
    let mut updated_clip_count = 0usize;
    for track in project.tracks.iter_mut() {
        for clip in track.clips.iter_mut() {
            if let Some(new_path) = remap_map.get(&clip.source_path) {
                if clip.source_path != *new_path {
                    clip.source_path = new_path.clone();
                    updated_clip_count += 1;
                }
            }
        }
    }
    let mut updated_library_count = 0usize;
    for item in library.iter_mut() {
        if let Some(new_path) = remap_map.get(&item.source_path) {
            if item.source_path != *new_path {
                item.source_path = new_path.clone();
                updated_library_count += 1;
            }
        }
    }
    if updated_clip_count > 0 {
        project.dirty = true;
    }

    unresolved.sort_unstable();
    RelinkSummary {
        scanned_files: scanned_files.len(),
        remapped,
        unresolved,
        updated_clip_count,
        updated_library_count,
    }
}

fn apply_collected_files_manifest_to_project_state(
    project: &Rc<RefCell<Project>>,
    library: &Rc<RefCell<MediaLibrary>>,
    source_marks: &Rc<RefCell<crate::model::media_library::SourceMarks>>,
    on_source_selected: &Rc<dyn Fn(String, u64)>,
    on_project_changed: &Rc<dyn Fn()>,
    manifest: &crate::fcpxml::writer::CollectFilesManifest,
) -> crate::fcpxml::writer::ApplyCollectedFilesResult {
    let remapped_source = {
        let current_path = source_marks.borrow().path.clone();
        if current_path.is_empty() {
            None
        } else {
            manifest
                .source_to_destination_path
                .get(&current_path)
                .map(|path| path.to_string_lossy().to_string())
        }
    };
    let summary = {
        let mut proj = project.borrow_mut();
        let mut lib = library.borrow_mut();
        crate::fcpxml::writer::apply_collected_files_manifest(
            &mut proj,
            lib.items.as_mut_slice(),
            manifest,
        )
    };
    if !summary.updated_any() {
        return summary;
    }
    if let Some(new_path) = remapped_source {
        let duration_ns = library
            .borrow()
            .items
            .iter()
            .find(|item| item.source_path == new_path)
            .map(|item| item.duration_ns)
            .unwrap_or_else(|| source_marks.borrow().duration_ns);
        on_source_selected(new_path, duration_ns);
    }
    on_project_changed();
    summary
}

#[derive(Clone, Copy)]
struct SourcePlacementInfo {
    is_audio_only: bool,
    has_audio: bool,
    is_image: bool,
    is_animated_svg: bool,
    source_timecode_base_ns: Option<u64>,
    audio_channel_mode: AudioChannelMode,
}

// ─────────────────────────────────────────────────────────────────────────

fn lookup_source_placement_info(
    library: &[MediaItem],
    project: &Project,
    source_path: &str,
) -> SourcePlacementInfo {
    let item = library.iter().find(|item| item.source_path == source_path);
    let mut is_audio_only = item.map(|item| item.is_audio_only).unwrap_or(false);
    let mut has_audio = item.map(|item| item.has_audio).unwrap_or(false);
    let is_animated_svg = item.map(|item| item.is_animated_svg).unwrap_or_else(|| {
        crate::model::clip::is_svg_file(source_path)
            && crate::media::animated_svg::analyze_svg_path(source_path)
                .map(|analysis| analysis.is_animated)
                .unwrap_or(false)
    });
    let is_image = item
        .map(|item| item.is_image)
        .unwrap_or_else(|| crate::model::clip::is_image_file(source_path));

    if item.is_none() || (!has_audio && !is_audio_only) {
        let metadata = crate::media::probe_cache::probe_media_metadata(source_path);
        is_audio_only = metadata.is_audio_only;
        has_audio = metadata.has_audio;
    }

    // Images are never audio-only; override Discoverer misclassification.
    if is_image {
        is_audio_only = false;
        has_audio = false;
    }

    SourcePlacementInfo {
        is_audio_only,
        has_audio,
        is_image,
        is_animated_svg,
        source_timecode_base_ns: lookup_source_timecode_base_ns(library, project, source_path),
        audio_channel_mode: lookup_source_audio_channel_mode(project, source_path),
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
    let auto_link_pair = source_monitor_auto_link_av
        && !source_info.is_audio_only
        && source_info.has_audio
        && !source_info.is_image;
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
    let clip_kind = if source_info.is_image {
        ClipKind::Image
    } else if target_kind == TrackKind::Audio {
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

fn ensure_matching_source_track_exists(
    project: &mut Project,
    source_info: SourcePlacementInfo,
) -> bool {
    let target_kind = if source_info.is_audio_only {
        TrackKind::Audio
    } else {
        TrackKind::Video
    };
    if project.tracks.iter().any(|track| track.kind == target_kind) {
        return false;
    }
    match target_kind {
        TrackKind::Video => project.add_video_track(),
        TrackKind::Audio => project.add_audio_track(),
    }
    true
}

fn build_source_clips_for_plan(
    plan: &SourcePlacementPlan,
    source_path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
    timeline_start_ns: u64,
    source_timecode_base_ns: Option<u64>,
    audio_channel_mode: AudioChannelMode,
    media_duration_ns: Option<u64>,
    animated_svg: bool,
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
                audio_channel_mode,
                plan.link_group_id.as_deref(),
                media_duration_ns,
            );
            clip.animated_svg = animated_svg;
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
    audio_channel_mode: AudioChannelMode,
    link_group_id: Option<&str>,
    media_duration_ns: Option<u64>,
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
    clip.audio_channel_mode = audio_channel_mode;
    clip.link_group_id = link_group_id.map(str::to_string);
    clip.media_duration_ns = media_duration_ns;
    clip
}

fn media_item_frame_rate(item: &MediaItem) -> Option<FrameRate> {
    let (numerator, denominator) = item.frame_rate_num.zip(item.frame_rate_den)?;
    if numerator == 0 || denominator == 0 {
        return None;
    }
    Some(FrameRate {
        numerator,
        denominator,
    })
}

fn resolve_ltc_frame_rate_for_source(
    library: &MediaLibrary,
    project: &Project,
    source_path: &str,
    frame_rate_override: Option<FrameRate>,
) -> FrameRate {
    frame_rate_override
        .or_else(|| {
            library
                .items
                .iter()
                .find(|item| item.source_path == source_path)
                .and_then(media_item_frame_rate)
        })
        .unwrap_or_else(|| project.frame_rate.clone())
}

#[derive(Debug, Clone)]
struct LtcConversionContext {
    source_path: String,
    source_in: u64,
    source_out: u64,
    frame_rate: FrameRate,
}

#[derive(Debug, Clone)]
struct PreparedLtcConversion {
    context: LtcConversionContext,
    decode: crate::media::ltc::LtcDecodeResult,
}

#[derive(Debug, Clone)]
struct AppliedLtcConversion {
    source_path: String,
    source_timecode_base_ns: u64,
    updated_clip_count: usize,
    muted_clip_count: usize,
    applied_audio_channel_mode: Option<AudioChannelMode>,
    resolved_channel: crate::media::ltc::ResolvedLtcChannel,
    frame_rate: FrameRate,
}

fn resolve_ltc_conversion_context(
    project: &Project,
    library: &MediaLibrary,
    clip_id: &str,
    frame_rate_override: Option<FrameRate>,
) -> Result<LtcConversionContext, String> {
    let clip = project
        .clip_ref(clip_id)
        .ok_or_else(|| "Clip not found.".to_string())?;
    if clip.source_path.is_empty() {
        return Err("Selected clip does not reference source media.".to_string());
    }
    if !matches!(clip.kind, ClipKind::Video | ClipKind::Audio) {
        return Err(
            "LTC conversion currently supports audio and video source clips only.".to_string(),
        );
    }
    if clip.source_out <= clip.source_in {
        return Err("Selected clip has an empty source range.".to_string());
    }

    Ok(LtcConversionContext {
        source_path: clip.source_path.clone(),
        source_in: clip.source_in,
        source_out: clip.source_out,
        frame_rate: resolve_ltc_frame_rate_for_source(
            library,
            project,
            &clip.source_path,
            frame_rate_override,
        ),
    })
}

fn apply_ltc_conversion_to_tracks(
    tracks: &mut [crate::model::track::Track],
    source_path: &str,
    source_timecode_base_ns: u64,
    audio_repair: crate::media::ltc::LtcAudioRepair,
) -> (usize, usize) {
    let mut updated_clip_count = 0usize;
    let mut muted_clip_count = 0usize;

    for track in tracks {
        for clip in &mut track.clips {
            if clip.source_path == source_path {
                clip.source_timecode_base_ns = Some(source_timecode_base_ns);
                if audio_repair.mute {
                    clip.volume = 0.0;
                    muted_clip_count += 1;
                } else if let Some(channel_mode) = audio_repair.channel_mode {
                    clip.audio_channel_mode = channel_mode;
                }
                updated_clip_count += 1;
            }
            if let Some(ref mut compound_tracks) = clip.compound_tracks {
                let (nested_updated, nested_muted) = apply_ltc_conversion_to_tracks(
                    compound_tracks,
                    source_path,
                    source_timecode_base_ns,
                    audio_repair,
                );
                updated_clip_count += nested_updated;
                muted_clip_count += nested_muted;
            }
        }
    }

    (updated_clip_count, muted_clip_count)
}

fn apply_prepared_ltc_conversion(
    project: &mut Project,
    library: &mut MediaLibrary,
    source_marks: Option<&mut SourceMarks>,
    prepared: PreparedLtcConversion,
) -> AppliedLtcConversion {
    let audio_repair = crate::media::ltc::audio_repair_for_ltc_channel(
        prepared.decode.channel_count,
        prepared.decode.resolved_channel,
    );
    let (updated_clip_count, muted_clip_count) = apply_ltc_conversion_to_tracks(
        &mut project.tracks,
        &prepared.context.source_path,
        prepared.decode.source_timecode_base_ns,
        audio_repair,
    );
    for item in library
        .items
        .iter_mut()
        .filter(|item| item.source_path == prepared.context.source_path)
    {
        item.source_timecode_base_ns = Some(prepared.decode.source_timecode_base_ns);
    }
    if let Some(source_marks) = source_marks {
        if source_marks.path == prepared.context.source_path {
            source_marks.source_timecode_base_ns = Some(prepared.decode.source_timecode_base_ns);
            if let Some(channel_mode) = audio_repair.channel_mode {
                source_marks.audio_channel_mode = channel_mode;
            }
        }
    }
    if updated_clip_count > 0 {
        project.dirty = true;
    }

    AppliedLtcConversion {
        source_path: prepared.context.source_path,
        source_timecode_base_ns: prepared.decode.source_timecode_base_ns,
        updated_clip_count,
        muted_clip_count,
        applied_audio_channel_mode: audio_repair.channel_mode,
        resolved_channel: prepared.decode.resolved_channel,
        frame_rate: prepared.context.frame_rate,
    }
}

fn format_ltc_conversion_status(applied: &AppliedLtcConversion) -> String {
    let timecode_label =
        timecode::format_ns_as_timecode(applied.source_timecode_base_ns, &applied.frame_rate);
    let clip_count = applied.updated_clip_count;
    if applied.muted_clip_count > 0 {
        format!("Converted LTC to {timecode_label} on {clip_count} clip(s) — muted clip audio")
    } else if let Some(channel_mode) = applied.applied_audio_channel_mode {
        let routed = match channel_mode {
            AudioChannelMode::Left => "left-channel program audio",
            AudioChannelMode::Right => "right-channel program audio",
            AudioChannelMode::MonoMix => "mono-mix program audio",
            AudioChannelMode::Stereo => "stereo program audio",
        };
        format!("Converted LTC to {timecode_label} on {clip_count} clip(s) — using {routed}")
    } else {
        format!("Converted LTC to {timecode_label} on {clip_count} clip(s)")
    }
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

    fn audio_match_clip_info_with_regions(
        duration_ns: u64,
        speech_regions: Vec<crate::media::audio_match::AnalysisRegionNs>,
    ) -> AudioMatchClipInfo {
        AudioMatchClipInfo {
            source_path: "/tmp/source.wav".to_string(),
            source_in: 10_000_000_000,
            source_out: 10_000_000_000 + duration_ns,
            duration_ns,
            speech_regions,
            volume: 1.0,
            measured_loudness_lufs: None,
            eq_bands: crate::model::clip::default_eq_bands(),
            match_eq_bands: Vec::new(),
            audio_channel_mode: crate::model::clip::AudioChannelMode::Stereo,
            kind: ClipKind::Audio,
        }
    }

    #[test]
    fn resolve_audio_match_region_defaults_to_full_clip() {
        let clip = audio_match_clip_info_with_regions(5_000_000_000, Vec::new());
        let region = resolve_audio_match_region(&clip, None, "Source")
            .expect("default region should cover the full clip");
        assert_eq!(region, full_audio_match_region(clip.duration_ns));
    }

    #[test]
    fn resolve_audio_match_region_rejects_out_of_bounds_ranges() {
        let clip = audio_match_clip_info_with_regions(5_000_000_000, Vec::new());
        let err = resolve_audio_match_region(
            &clip,
            Some(crate::media::audio_match::AnalysisRegionNs {
                start_ns: 1_000_000_000,
                end_ns: 6_000_000_000,
            }),
            "Reference",
        )
        .expect_err("range beyond clip duration should fail");
        assert_eq!(err, "Reference range exceeds clip duration.");
    }

    #[test]
    fn region_scoped_audio_match_clip_info_rebases_speech_regions() {
        let clip = audio_match_clip_info_with_regions(
            8_000_000_000,
            vec![
                crate::media::audio_match::AnalysisRegionNs {
                    start_ns: 500_000_000,
                    end_ns: 2_000_000_000,
                },
                crate::media::audio_match::AnalysisRegionNs {
                    start_ns: 3_000_000_000,
                    end_ns: 6_500_000_000,
                },
            ],
        );

        let scoped = region_scoped_audio_match_clip_info(
            &clip,
            crate::media::audio_match::AnalysisRegionNs {
                start_ns: 1_000_000_000,
                end_ns: 5_000_000_000,
            },
        );

        assert_eq!(scoped.source_in, 11_000_000_000);
        assert_eq!(scoped.source_out, 15_000_000_000);
        assert_eq!(scoped.duration_ns, 4_000_000_000);
        assert_eq!(
            scoped.speech_regions,
            vec![
                crate::media::audio_match::AnalysisRegionNs {
                    start_ns: 0,
                    end_ns: 1_000_000_000,
                },
                crate::media::audio_match::AnalysisRegionNs {
                    start_ns: 2_000_000_000,
                    end_ns: 4_000_000_000,
                },
            ]
        );
    }

    #[test]
    fn collect_project_library_entries_keeps_distinct_sourceless_titles() {
        let mut project = Project::new("Test");
        let video_idx = project
            .tracks
            .iter()
            .position(|track| track.is_video())
            .expect("video track should exist");

        let mut title_a = Clip::new("", 4_000_000_000, 0, ClipKind::Title);
        title_a.id = "title-a".to_string();
        title_a.label = "Lower Third".to_string();
        title_a.title_text = "Jane Doe".to_string();

        let mut title_b = Clip::new("", 5_000_000_000, 6_000_000_000, ClipKind::Title);
        title_b.id = "title-b".to_string();
        title_b.label = "Lower Third".to_string();
        title_b.title_text = "John Doe".to_string();

        project.tracks[video_idx].clips.push(title_a);
        project.tracks[video_idx].clips.push(title_b);

        let entries = collect_project_library_entries(&project);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].sync_key, "clip:title-a");
        assert_eq!(entries[1].sync_key, "clip:title-b");
        assert_eq!(entries[0].title_text.as_deref(), Some("Jane Doe"));
        assert_eq!(entries[1].title_text.as_deref(), Some("John Doe"));
    }

    #[test]
    fn source_monitor_plan_links_video_and_audio_when_both_tracks_exist() {
        let project = Project::new("Test");
        let preferred_audio_track_id = project
            .tracks
            .iter()
            .find(|track| track.is_audio())
            .map(|track| track.id.clone())
            .expect("audio track should exist");
        let source_info = SourcePlacementInfo {
            is_audio_only: false,
            has_audio: true,
            is_image: false,
            is_animated_svg: false,
            source_timecode_base_ns: Some(42),
            audio_channel_mode: AudioChannelMode::Stereo,
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
            source_info.audio_channel_mode,
            None,
            false,
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
    fn source_monitor_created_clips_inherit_audio_channel_mode() {
        let project = Project::new("Test");
        let source_info = SourcePlacementInfo {
            is_audio_only: false,
            has_audio: true,
            is_image: false,
            is_animated_svg: false,
            source_timecode_base_ns: None,
            audio_channel_mode: AudioChannelMode::Right,
        };

        let plan = build_source_placement_plan_by_track_id(&project, None, source_info, false);
        let created = build_source_clips_for_plan(
            &plan,
            "/tmp/program-with-ltc.wav",
            0,
            500_000_000,
            0,
            None,
            source_info.audio_channel_mode,
            None,
            false,
        );

        assert!(!created.is_empty());
        assert!(created
            .iter()
            .all(|(_, clip)| clip.audio_channel_mode == AudioChannelMode::Right));
    }

    #[test]
    fn apply_prepared_ltc_conversion_updates_matching_clips_and_source_marks() {
        let mut project = Project::new("Test");
        let audio_track_idx = project
            .tracks
            .iter()
            .position(|track| track.is_audio())
            .expect("audio track should exist");

        let mut root_clip = build_source_clip(
            "/tmp/program-with-ltc.wav",
            0,
            1_000_000_000,
            0,
            ClipKind::Audio,
            None,
            AudioChannelMode::Stereo,
            None,
            None,
        );
        root_clip.id = "root-clip".to_string();
        project.tracks[audio_track_idx].add_clip(root_clip);

        let mut nested_clip = build_source_clip(
            "/tmp/program-with-ltc.wav",
            0,
            1_000_000_000,
            0,
            ClipKind::Audio,
            None,
            AudioChannelMode::Stereo,
            None,
            None,
        );
        nested_clip.id = "nested-clip".to_string();
        let mut nested_track = crate::model::track::Track::new_audio("Nested");
        nested_track.add_clip(nested_clip);
        let mut compound = Clip::new("", 1_000_000_000, 0, ClipKind::Compound);
        compound.id = "compound-clip".to_string();
        compound.compound_tracks = Some(vec![nested_track]);
        project.tracks[audio_track_idx].add_clip(compound);

        let mut library = MediaLibrary::default();
        library
            .items
            .push(MediaItem::new("/tmp/program-with-ltc.wav", 1_000_000_000));

        let mut source_marks = SourceMarks {
            path: "/tmp/program-with-ltc.wav".to_string(),
            duration_ns: 1_000_000_000,
            out_ns: 1_000_000_000,
            has_audio: true,
            is_audio_only: true,
            ..SourceMarks::default()
        };

        let prepared = PreparedLtcConversion {
            context: LtcConversionContext {
                source_path: "/tmp/program-with-ltc.wav".to_string(),
                source_in: 0,
                source_out: 1_000_000_000,
                frame_rate: FrameRate::fps_24(),
            },
            decode: crate::media::ltc::LtcDecodeResult {
                source_timecode_base_ns: 3_600_000_000_000,
                resolved_channel: crate::media::ltc::ResolvedLtcChannel::Left,
                channel_count: 2,
                frame_start_sample: 0,
                decoded_frame_count: 2,
            },
        };

        let applied = apply_prepared_ltc_conversion(
            &mut project,
            &mut library,
            Some(&mut source_marks),
            prepared,
        );

        assert_eq!(applied.updated_clip_count, 2);
        assert_eq!(applied.muted_clip_count, 0);
        assert_eq!(
            applied.applied_audio_channel_mode,
            Some(AudioChannelMode::Right)
        );
        assert_eq!(
            project
                .clip_ref("root-clip")
                .unwrap()
                .source_timecode_base_ns,
            Some(3_600_000_000_000)
        );
        assert_eq!(
            project
                .clip_ref("nested-clip")
                .unwrap()
                .source_timecode_base_ns,
            Some(3_600_000_000_000)
        );
        assert_eq!(
            project.clip_ref("root-clip").unwrap().audio_channel_mode,
            AudioChannelMode::Right
        );
        assert_eq!(
            project.clip_ref("nested-clip").unwrap().audio_channel_mode,
            AudioChannelMode::Right
        );
        assert_eq!(
            library.items[0].source_timecode_base_ns,
            Some(3_600_000_000_000)
        );
        assert_eq!(
            source_marks.source_timecode_base_ns,
            Some(3_600_000_000_000)
        );
        assert_eq!(source_marks.audio_channel_mode, AudioChannelMode::Right);
        assert!(project.dirty);
    }

    #[test]
    fn source_monitor_plan_with_auto_link_disabled_uses_single_video_clip_for_av_sources() {
        let project = Project::new("Test");
        let source_info = SourcePlacementInfo {
            is_audio_only: false,
            has_audio: true,
            is_image: false,
            is_animated_svg: false,
            source_timecode_base_ns: None,
            audio_channel_mode: AudioChannelMode::Stereo,
        };

        let plan = build_source_placement_plan_by_track_id(&project, None, source_info, false);

        assert_eq!(plan.targets.len(), 1);
        assert_eq!(plan.targets[0].clip_kind, ClipKind::Video);
        assert!(plan.link_group_id.is_none());
    }

    #[test]
    fn source_monitor_plan_falls_back_to_single_kind_when_pair_not_possible() {
        let mut project_video_only = Project::new("Test");
        project_video_only.tracks.retain(|track| track.is_video());
        let mut project_audio_only = Project::new("Test");
        project_audio_only.tracks.retain(|track| track.is_audio());
        let source_info = SourcePlacementInfo {
            is_audio_only: false,
            has_audio: true,
            is_image: false,
            is_animated_svg: false,
            source_timecode_base_ns: None,
            audio_channel_mode: AudioChannelMode::Stereo,
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
            is_image: false,
            is_animated_svg: false,
            source_timecode_base_ns: None,
            audio_channel_mode: AudioChannelMode::Stereo,
        };
        let silent_video = SourcePlacementInfo {
            is_audio_only: false,
            has_audio: false,
            is_image: false,
            is_animated_svg: false,
            source_timecode_base_ns: None,
            audio_channel_mode: AudioChannelMode::Stereo,
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
            is_image: false,
            is_animated_svg: false,
            source_timecode_base_ns: None,
            audio_channel_mode: AudioChannelMode::Stereo,
        };

        let plan = build_source_placement_plan_by_track_id(&project, None, source_info, true);
        assert!(plan.targets.is_empty());
        assert!(plan.link_group_id.is_none());
    }

    #[test]
    fn ensure_matching_source_track_exists_adds_video_track_for_image_sources() {
        let mut project = Project::new("Test");
        project.tracks.retain(|track| track.is_audio());
        let source_info = SourcePlacementInfo {
            is_audio_only: false,
            has_audio: false,
            is_image: true,
            is_animated_svg: false,
            source_timecode_base_ns: None,
            audio_channel_mode: AudioChannelMode::Stereo,
        };

        assert!(ensure_matching_source_track_exists(
            &mut project,
            source_info
        ));
        assert!(project.tracks.iter().any(|track| track.is_video()));

        let plan = build_source_placement_plan_by_track_id(&project, None, source_info, true);
        assert_eq!(plan.targets.len(), 1);
        assert_eq!(plan.targets[0].clip_kind, ClipKind::Image);
    }

    #[test]
    fn ensure_matching_source_track_exists_adds_audio_track_for_audio_only_sources() {
        let mut project = Project::new("Test");
        project.tracks.retain(|track| track.is_video());
        let source_info = SourcePlacementInfo {
            is_audio_only: true,
            has_audio: true,
            is_image: false,
            is_animated_svg: false,
            source_timecode_base_ns: None,
            audio_channel_mode: AudioChannelMode::Stereo,
        };

        assert!(ensure_matching_source_track_exists(
            &mut project,
            source_info
        ));
        assert!(project.tracks.iter().any(|track| track.is_audio()));

        let plan = build_source_placement_plan_by_track_id(&project, None, source_info, true);
        assert_eq!(plan.targets.len(), 1);
        assert_eq!(plan.targets[0].clip_kind, ClipKind::Audio);
    }

    #[test]
    fn mcp_track_index_plan_matches_track_id_for_silent_video_audio_target() {
        let project = Project::new("Test");
        let preferred_audio_track = project
            .tracks
            .iter()
            .enumerate()
            .find(|(_, track)| track.is_audio())
            .expect("audio track should exist");
        let source_info = SourcePlacementInfo {
            is_audio_only: false,
            has_audio: false,
            is_image: false,
            is_animated_svg: false,
            source_timecode_base_ns: None,
            audio_channel_mode: AudioChannelMode::Stereo,
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
            .find(|(_, track)| track.is_video())
            .map(|(idx, _)| idx)
            .expect("video track should exist");
        let preferred_audio_track_idx = project
            .tracks
            .iter()
            .enumerate()
            .find(|(_, track)| track.is_audio())
            .map(|(idx, _)| idx)
            .expect("audio track should exist");
        let source_info = SourcePlacementInfo {
            is_audio_only: true,
            has_audio: true,
            is_image: false,
            is_animated_svg: false,
            source_timecode_base_ns: None,
            audio_channel_mode: AudioChannelMode::Stereo,
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
    fn track_index_plan_with_auto_link_disabled_uses_single_video_clip_for_av_sources() {
        let project = Project::new("Test");
        let preferred_video_track_idx = project
            .tracks
            .iter()
            .enumerate()
            .find(|(_, track)| track.is_video())
            .map(|(idx, _)| idx)
            .expect("video track should exist");
        let source_info = SourcePlacementInfo {
            is_audio_only: false,
            has_audio: true,
            is_image: false,
            is_animated_svg: false,
            source_timecode_base_ns: None,
            audio_channel_mode: AudioChannelMode::Stereo,
        };

        let plan = build_source_placement_plan_by_track_index(
            &project,
            Some(preferred_video_track_idx),
            source_info,
            false,
        );
        let created = build_source_clips_for_plan(
            &plan,
            "/tmp/source.mp4",
            0,
            1_000,
            2_000,
            source_info.source_timecode_base_ns,
            source_info.audio_channel_mode,
            Some(10_000),
            false,
        );

        assert_eq!(plan.targets.len(), 1);
        assert_eq!(plan.targets[0].track_index, preferred_video_track_idx);
        assert_eq!(plan.targets[0].clip_kind, ClipKind::Video);
        assert!(plan.link_group_id.is_none());
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].0, preferred_video_track_idx);
        assert_eq!(created[0].1.kind, ClipKind::Video);
        assert!(created[0].1.link_group_id.is_none());
    }

    #[test]
    fn mcp_track_index_plan_returns_empty_without_matching_tracks() {
        let mut project = Project::new("Test");
        project.tracks.retain(|track| track.is_video());
        let source_info = SourcePlacementInfo {
            is_audio_only: true,
            has_audio: true,
            is_image: false,
            is_animated_svg: false,
            source_timecode_base_ns: None,
            audio_channel_mode: AudioChannelMode::Stereo,
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
            is_image: false,
            is_animated_svg: false,
            source_timecode_base_ns: None,
            audio_channel_mode: AudioChannelMode::Stereo,
        };

        project.tracks[0].add_clip(build_source_clip(
            "/tmp/existing-video.mp4",
            0,
            1_000_000_000,
            1_500_000_000,
            ClipKind::Video,
            None,
            AudioChannelMode::Stereo,
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
            AudioChannelMode::Stereo,
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
            source_info.audio_channel_mode,
            None,
            false,
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
            AudioChannelMode::Stereo,
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
            AudioChannelMode::Stereo,
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
            source_info.audio_channel_mode,
            None,
            false,
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

    #[test]
    fn choose_relink_candidate_prefers_longest_tail_match() {
        let original = "/media/shoot/day1/camA/scene01/clip.mp4";
        let candidates = vec![
            std::path::PathBuf::from("/tmp/other/clip.mp4"),
            std::path::PathBuf::from("/mnt/archive/day1/camA/scene01/clip.mp4"),
        ];
        let chosen = choose_relink_candidate(original, &candidates).expect("candidate");
        assert_eq!(
            chosen,
            std::path::PathBuf::from("/mnt/archive/day1/camA/scene01/clip.mp4")
        );
    }

    #[test]
    fn choose_relink_candidate_breaks_ties_deterministically() {
        let original = "/media/shoot/day1/camA/clip.mp4";
        let candidates = vec![
            std::path::PathBuf::from("/z-archive/day1/camA/clip.mp4"),
            std::path::PathBuf::from("/a-archive/day1/camA/clip.mp4"),
        ];
        let chosen = choose_relink_candidate(original, &candidates).expect("candidate");
        assert_eq!(
            chosen,
            std::path::PathBuf::from("/a-archive/day1/camA/clip.mp4")
        );
    }

    #[test]
    fn relink_missing_media_remaps_project_and_library_paths() {
        let root = std::env::temp_dir().join(format!(
            "ultimateslice-relink-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let nested = root.join("show/day1/camA");
        std::fs::create_dir_all(&nested).expect("create relink test dirs");
        let target = nested.join("clip.mp4");
        std::fs::write(&target, b"test").expect("write target media");

        let mut project = Project::new("Relink");
        project.tracks[0].clips.clear();
        project.tracks[1].clips.clear();
        let missing_path = "/missing/media/show/day1/camA/clip.mp4";
        project.tracks[0].add_clip(build_source_clip(
            missing_path,
            0,
            1_000_000_000,
            0,
            ClipKind::Video,
            None,
            AudioChannelMode::Stereo,
            None,
            None,
        ));
        let mut library = vec![MediaItem::new(missing_path, 1_000_000_000)];
        let summary = relink_missing_media_under_root(&mut project, library.as_mut_slice(), &root);

        assert_eq!(summary.updated_clip_count, 1);
        assert_eq!(summary.updated_library_count, 1);
        assert_eq!(summary.unresolved.len(), 0);
        assert_eq!(summary.remapped.len(), 1);
        let expected = target.to_string_lossy().to_string();
        assert_eq!(project.tracks[0].clips[0].source_path, expected);
        assert_eq!(library[0].source_path, expected);

        // Verify that refresh_media_availability_state clears is_missing
        // after relink (this is the chain the GUI follows).
        assert!(
            library[0].is_missing,
            "before refresh, is_missing should still be true (relink only updates path)"
        );
        let timeline_project = Rc::new(RefCell::new(project.clone()));
        let mut timeline_state = TimelineState::new(timeline_project);
        let missing =
            refresh_media_availability_state(&project, library.as_mut_slice(), &mut timeline_state);
        assert!(
            missing.is_empty(),
            "after refresh, no paths should be missing; got {:?}",
            missing
        );
        assert!(
            !library[0].is_missing,
            "library item.is_missing should be false after refresh"
        );
        assert!(
            timeline_state.missing_media_paths.is_empty(),
            "timeline missing_media_paths should be empty after refresh"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Simulate the full GUI relink sequence including the library sync that
    /// happens inside on_project_changed_impl.
    #[test]
    fn relink_full_gui_chain_clears_missing_state() {
        let root = std::env::temp_dir().join(format!(
            "ultimateslice-relink-chain-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let nested = root.join("footage/day1");
        std::fs::create_dir_all(&nested).expect("create test dirs");
        let target = nested.join("shot.mp4");
        std::fs::write(&target, b"test").expect("write target");

        let missing_path = "/old/footage/day1/shot.mp4";
        let mut project = Project::new("RelinkChain");
        project.tracks[0].clips.clear();
        project.tracks[1].clips.clear();
        project.tracks[0].add_clip(build_source_clip(
            missing_path,
            0,
            1_000_000_000,
            0,
            ClipKind::Video,
            None,
            AudioChannelMode::Stereo,
            None,
            None,
        ));
        let mut library = vec![MediaItem::new(missing_path, 1_000_000_000)];
        assert!(library[0].is_missing, "precondition: item is missing");

        // Step 1: relink (same as GUI callback)
        let summary = relink_missing_media_under_root(&mut project, library.as_mut_slice(), &root);
        assert_eq!(summary.remapped.len(), 1, "should remap 1 file");

        let expected = target.to_string_lossy().to_string();
        assert_eq!(project.tracks[0].clips[0].source_path, expected);
        assert_eq!(library[0].source_path, expected);

        // Step 2: refresh_media_availability_state (same as GUI callback)
        let timeline_project = Rc::new(RefCell::new(project.clone()));
        let mut st = TimelineState::new(timeline_project);
        let missing1 = refresh_media_availability_state(&project, library.as_mut_slice(), &mut st);
        assert!(
            missing1.is_empty(),
            "step 2: no missing; got {:?}",
            missing1
        );
        assert!(!library[0].is_missing, "step 2: is_missing should be false");
        assert!(st.missing_media_paths.is_empty(), "step 2: timeline clear");

        // Step 3: Simulate on_project_changed_impl library sync
        let media_from_project = collect_project_library_entries(&project);
        let mut wrapped_library = MediaLibrary {
            items: library,
            bins: Vec::new(),
            collections: Vec::new(),
        };
        sync_library_with_project_entries(&mut wrapped_library, &media_from_project);
        let mut library = wrapped_library.items;

        // Step 4: second refresh_media_availability_state (inside on_project_changed_impl)
        let missing2 = refresh_media_availability_state(&project, library.as_mut_slice(), &mut st);
        assert!(
            missing2.is_empty(),
            "step 4: no missing; got {:?}",
            missing2
        );
        assert!(!library[0].is_missing, "step 4: is_missing should be false");
        assert!(st.missing_media_paths.is_empty(), "step 4: timeline clear");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn apply_tracking_binding_selection_is_noop_for_same_clip_binding() {
        let mut clip = Clip::new("/tmp/overlay.png", 1_000_000_000, 0, ClipKind::Image);
        clip.tracking_binding = Some(crate::model::clip::TrackingBinding::new(
            "source-clip",
            "tracker-1",
        ));
        let reference = crate::model::project::MotionTrackerReference {
            source_clip_id: "source-clip".to_string(),
            clip_label: "Source".to_string(),
            tracker_id: "tracker-1".to_string(),
            tracker_label: "Tracker 1".to_string(),
            enabled: true,
            sample_count: 2,
        };

        let changed = apply_tracking_binding_selection(&mut clip, false, Some(&reference));

        assert!(!changed);
        assert_eq!(
            clip.tracking_binding,
            Some(crate::model::clip::TrackingBinding::new(
                "source-clip",
                "tracker-1"
            ))
        );
    }

    #[test]
    fn apply_tracking_binding_selection_is_noop_for_same_mask_binding() {
        let mut clip = Clip::new("/tmp/overlay.png", 1_000_000_000, 0, ClipKind::Image);
        let mut mask = crate::model::clip::ClipMask::new(crate::model::clip::MaskShape::Rectangle);
        mask.tracking_binding = Some(crate::model::clip::TrackingBinding::new(
            "source-clip",
            "tracker-1",
        ));
        clip.masks.push(mask);
        let reference = crate::model::project::MotionTrackerReference {
            source_clip_id: "source-clip".to_string(),
            clip_label: "Source".to_string(),
            tracker_id: "tracker-1".to_string(),
            tracker_label: "Tracker 1".to_string(),
            enabled: true,
            sample_count: 2,
        };

        let changed = apply_tracking_binding_selection(&mut clip, true, Some(&reference));

        assert!(!changed);
        assert!(clip.tracking_binding.is_none());
        assert_eq!(
            clip.masks[0].tracking_binding,
            Some(crate::model::clip::TrackingBinding::new(
                "source-clip",
                "tracker-1"
            ))
        );
    }

    #[test]
    fn selected_clip_is_static_image_excludes_animated_svg() {
        let mut project = Project::new("Test");

        let static_image = Clip::new("/tmp/overlay.png", 1_000_000_000, 0, ClipKind::Image);
        let static_image_id = static_image.id.clone();

        let mut animated_svg = Clip::new(
            "/tmp/overlay.svg",
            1_000_000_000,
            2_000_000_000,
            ClipKind::Image,
        );
        animated_svg.animated_svg = true;
        let animated_svg_id = animated_svg.id.clone();

        project.tracks[0].clips.push(static_image);
        project.tracks[0].clips.push(animated_svg);

        assert!(selected_clip_is_static_image(
            &project,
            Some(&static_image_id)
        ));
        assert!(!selected_clip_is_static_image(
            &project,
            Some(&animated_svg_id)
        ));
        assert!(!selected_clip_is_static_image(&project, None));
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
    results: &[(String, i64, f32, Option<f64>)],
    project: &Rc<RefCell<Project>>,
    timeline_state: &Rc<RefCell<crate::ui::timeline::TimelineState>>,
    on_project_changed: &Rc<dyn Fn()>,
    window: Option<&gtk::ApplicationWindow>,
    replace_audio: bool,
) {
    use crate::undo::SetTrackClipsCommand;

    const MIN_CONFIDENCE: f32 = 3.0;

    // Detect "no change" when all offsets are 0
    if results.iter().all(|(_, offset, _, _)| *offset == 0) {
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
    let low_confidence = results.iter().any(|(_, _, c, _)| *c < MIN_CONFIDENCE);
    if low_confidence {
        if let Some(win) = window {
            flash_window_status_title(
                win,
                project,
                &format!(
                    "Audio sync failed — confidence too low ({:.1})",
                    results
                        .iter()
                        .map(|(_, _, c, _)| *c)
                        .fold(f32::INFINITY, f32::min)
                ),
            );
        }
        return;
    }

    // Find the anchor clip's timeline_start (first selected clip that wasn't synced)
    let synced_ids: HashSet<&str> = results.iter().map(|(id, _, _, _)| id.as_str()).collect();
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

    // Build new clip positions and collect drift corrections.
    let mut assignments: HashMap<String, u64> = HashMap::new();
    let mut drift_corrections: HashMap<String, f64> = HashMap::new();
    for (clip_id, offset_ns, _, drift_speed) in results {
        let new_start = (anchor_timeline_start as i64 + offset_ns).max(0) as u64;
        assignments.insert(clip_id.clone(), new_start);
        if let Some(drift) = drift_speed {
            drift_corrections.insert(clip_id.clone(), *drift);
        }
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
                        // Apply drift correction (speed adjustment).
                        if let Some(&drift) = drift_corrections.get(&clip.id) {
                            if (drift - 1.0).abs() > 1e-9 {
                                clip.speed *= drift;
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
        let label = if replace_audio {
            "Sync & replace audio"
        } else {
            "Sync clips by audio"
        };
        for (track_id, old_clips, new_clips) in track_updates {
            let cmd = SetTrackClipsCommand {
                track_id,
                old_clips,
                new_clips,
                label: label.to_string(),
            };
            st.history.execute(Box::new(cmd), &mut proj);
        }

        // When replace_audio is set, link all involved clips and mute the
        // anchor's embedded audio so the external audio replaces it.
        if replace_audio {
            let link_id = uuid::Uuid::new_v4().to_string();
            let all_ids = st.selected_ids_or_primary();
            // Find the anchor clip ID (the selected clip NOT in the sync results).
            let anchor_id = all_ids
                .iter()
                .find(|id| !synced_ids.contains(id.as_str()))
                .cloned();
            for track in &mut proj.tracks {
                for clip in &mut track.clips {
                    if all_ids.contains(&clip.id) {
                        clip.link_group_id = Some(link_id.clone());
                    }
                    // Mute the anchor (camera) clip's embedded audio.
                    if Some(&clip.id) == anchor_id.as_ref()
                        && clip.kind == crate::model::clip::ClipKind::Video
                    {
                        clip.volume = 0.0;
                    }
                }
            }
        }

        proj.dirty = true;
    }

    on_project_changed();

    let status = if replace_audio {
        "Sync & replace audio complete"
    } else {
        "Audio sync complete"
    };
    if let Some(win) = window {
        flash_window_status_title(win, project, status);
    }
}

/// Apply silence removal results: split the original clip into non-silent sub-clips,
/// pack them back-to-back, and optionally shift subsequent clips in magnetic mode.
fn apply_remove_silent_parts_results(
    clip_id: &str,
    track_id: &str,
    silence_intervals: &[(f64, f64)],
    project: &Rc<RefCell<Project>>,
    timeline_state: &Rc<RefCell<crate::ui::timeline::TimelineState>>,
    on_project_changed: &Rc<dyn Fn()>,
    window: Option<&gtk::ApplicationWindow>,
) {
    use crate::undo::SetTrackClipsCommand;

    // No silence found → nothing to do
    if silence_intervals.is_empty() {
        if let Some(win) = window {
            flash_window_status_title(win, project, "No silence detected — clip unchanged");
        }
        return;
    }

    // Find the original clip and its track
    let (original_clip, old_clips, clip_duration_ns) = {
        let proj = project.borrow();
        let track = match proj.tracks.iter().find(|t| t.id == track_id) {
            Some(t) => t,
            None => {
                if let Some(win) = window {
                    flash_window_status_title(
                        win,
                        project,
                        "Silence removal failed — track not found",
                    );
                }
                return;
            }
        };
        let clip = match track.clips.iter().find(|c| c.id == clip_id) {
            Some(c) => c.clone(),
            None => {
                if let Some(win) = window {
                    flash_window_status_title(
                        win,
                        project,
                        "Silence removal failed — clip not found",
                    );
                }
                return;
            }
        };
        let dur = clip.source_duration();
        (clip, track.clips.clone(), dur)
    };

    let clip_duration_sec = clip_duration_ns as f64 / 1_000_000_000.0;

    // Invert silence intervals to get non-silent segments (in seconds, relative to source_in)
    let mut non_silent: Vec<(f64, f64)> = Vec::new();
    let mut cursor = 0.0_f64;
    for &(sil_start, sil_end) in silence_intervals {
        let sil_start = sil_start.max(0.0);
        let sil_end = sil_end.min(clip_duration_sec);
        if sil_start > cursor {
            non_silent.push((cursor, sil_start));
        }
        cursor = sil_end;
    }
    if cursor < clip_duration_sec {
        non_silent.push((cursor, clip_duration_sec));
    }

    // Filter out degenerate sub-segments shorter than 250ms (6 frames at 24fps)
    let min_segment_sec = 0.25;
    non_silent.retain(|&(s, e)| (e - s) >= min_segment_sec);

    if non_silent.is_empty() {
        if let Some(win) = window {
            flash_window_status_title(win, project, "Entire clip is silent — no segments to keep");
        }
        return;
    }

    // If non-silent covers the entire clip (no silence removed), nothing to do
    if non_silent.len() == 1 {
        let (s, e) = non_silent[0];
        if (s - 0.0).abs() < 0.001 && (e - clip_duration_sec).abs() < 0.001 {
            if let Some(win) = window {
                flash_window_status_title(win, project, "No silence detected — clip unchanged");
            }
            return;
        }
    }

    let speed = original_clip.speed;
    let original_timeline_start = original_clip.timeline_start;
    let original_source_in = original_clip.source_in;

    // Build sub-clips for each non-silent segment
    let mut sub_clips: Vec<Clip> = Vec::new();
    let mut timeline_cursor = original_timeline_start;
    for &(seg_start_sec, seg_end_sec) in &non_silent {
        let seg_start_ns = (seg_start_sec * 1_000_000_000.0).round() as u64;
        let seg_end_ns = (seg_end_sec * 1_000_000_000.0).round() as u64;
        let seg_duration_ns = seg_end_ns.saturating_sub(seg_start_ns);

        let mut sub = original_clip.clone();
        sub.id = uuid::Uuid::new_v4().to_string();
        sub.source_in = original_source_in + seg_start_ns;
        sub.source_out = original_source_in + seg_end_ns;
        sub.timeline_start = timeline_cursor;

        // Keyframes are in clip-local timeline time. Convert source-relative boundaries
        // to local-timeline coordinates (dividing by speed if != 1.0).
        let local_start = if speed != 0.0 && speed != 1.0 {
            (seg_start_ns as f64 / speed).round() as u64
        } else {
            seg_start_ns
        };
        let local_end = if speed != 0.0 && speed != 1.0 {
            (seg_end_ns as f64 / speed).round() as u64
        } else {
            seg_end_ns
        };
        sub.retain_keyframes_in_local_range(local_start, local_end);

        // Clear transition on all sub-clips except possibly the last
        sub.clear_outgoing_transition();

        // Timeline duration accounts for speed
        let timeline_duration = if speed != 0.0 {
            (seg_duration_ns as f64 / speed).round() as u64
        } else {
            seg_duration_ns
        };
        timeline_cursor += timeline_duration;
        sub_clips.push(sub);
    }

    let total_new_duration = timeline_cursor - original_timeline_start;
    let original_timeline_duration = if speed != 0.0 {
        (clip_duration_ns as f64 / speed).round() as u64
    } else {
        clip_duration_ns
    };
    let duration_removed = original_timeline_duration.saturating_sub(total_new_duration);

    // Build the new clip list for this track
    let magnetic_mode = timeline_state.borrow().magnetic_mode;
    let mut new_clips: Vec<Clip> = Vec::new();
    let mut found_original = false;
    for clip in &old_clips {
        if clip.id == clip_id {
            found_original = true;
            new_clips.extend(sub_clips.iter().cloned());
        } else {
            let mut c = clip.clone();
            // In magnetic mode, shift subsequent clips left to close the gap
            if found_original && magnetic_mode && duration_removed > 0 {
                c.timeline_start = c.timeline_start.saturating_sub(duration_removed);
            }
            new_clips.push(c);
        }
    }
    new_clips.sort_by_key(|c| c.timeline_start);

    // Execute via undo history
    {
        let mut st = timeline_state.borrow_mut();
        let proj_rc = st.project.clone();
        let mut proj = proj_rc.borrow_mut();
        let cmd = SetTrackClipsCommand {
            track_id: track_id.to_string(),
            old_clips,
            new_clips,
            label: "Remove silent parts".to_string(),
        };
        st.history.execute(Box::new(cmd), &mut proj);
        proj.dirty = true;
    }

    on_project_changed();

    if let Some(win) = window {
        let msg = format!(
            "Removed {} silent segment(s) — {} sub-clip(s) remain",
            silence_intervals.len(),
            sub_clips.len()
        );
        flash_window_status_title(win, project, &msg);
    }
}

/// Apply scene cut detection results: split the original clip at each detected cut point,
/// placing sub-clips back-to-back (preserving total duration).
fn apply_scene_cut_results(
    clip_id: &str,
    track_id: &str,
    cut_points: &[f64],
    project: &Rc<RefCell<Project>>,
    timeline_state: &Rc<RefCell<crate::ui::timeline::TimelineState>>,
    on_project_changed: &Rc<dyn Fn()>,
    window: Option<&gtk::ApplicationWindow>,
) {
    use crate::undo::SetTrackClipsCommand;

    if cut_points.is_empty() {
        if let Some(win) = window {
            flash_window_status_title(
                win,
                project,
                "No scene cuts detected \u{2014} clip unchanged",
            );
        }
        return;
    }

    let (original_clip, old_clips, clip_duration_ns) = {
        let proj = project.borrow();
        let track = match proj.tracks.iter().find(|t| t.id == track_id) {
            Some(t) => t,
            None => {
                if let Some(win) = window {
                    flash_window_status_title(
                        win,
                        project,
                        "Scene cut detection failed \u{2014} track not found",
                    );
                }
                return;
            }
        };
        let clip = match track.clips.iter().find(|c| c.id == clip_id) {
            Some(c) => c.clone(),
            None => {
                if let Some(win) = window {
                    flash_window_status_title(
                        win,
                        project,
                        "Scene cut detection failed \u{2014} clip not found",
                    );
                }
                return;
            }
        };
        let dur = clip.source_duration();
        (clip, track.clips.clone(), dur)
    };

    let clip_duration_sec = clip_duration_ns as f64 / 1_000_000_000.0;
    let speed = original_clip.speed;
    let original_timeline_start = original_clip.timeline_start;
    let original_source_in = original_clip.source_in;

    // Build segments from cut points: [0, cut0], [cut0, cut1], ..., [cutN, duration]
    let mut boundaries: Vec<f64> = Vec::with_capacity(cut_points.len() + 2);
    boundaries.push(0.0);
    for &cp in cut_points {
        let cp = cp.max(0.0).min(clip_duration_sec);
        if cp > *boundaries.last().unwrap() + 0.01 {
            boundaries.push(cp);
        }
    }
    if *boundaries.last().unwrap() < clip_duration_sec - 0.01 {
        boundaries.push(clip_duration_sec);
    }

    let mut sub_clips: Vec<Clip> = Vec::new();
    let mut timeline_cursor = original_timeline_start;
    for window_pair in boundaries.windows(2) {
        let seg_start_sec = window_pair[0];
        let seg_end_sec = window_pair[1];
        let seg_start_ns = (seg_start_sec * 1_000_000_000.0).round() as u64;
        let seg_end_ns = (seg_end_sec * 1_000_000_000.0).round() as u64;
        let seg_duration_ns = seg_end_ns.saturating_sub(seg_start_ns);

        let mut sub = original_clip.clone();
        sub.id = uuid::Uuid::new_v4().to_string();
        sub.source_in = original_source_in + seg_start_ns;
        sub.source_out = original_source_in + seg_end_ns;
        sub.timeline_start = timeline_cursor;

        let local_start = if speed != 0.0 && speed != 1.0 {
            (seg_start_ns as f64 / speed).round() as u64
        } else {
            seg_start_ns
        };
        let local_end = if speed != 0.0 && speed != 1.0 {
            (seg_end_ns as f64 / speed).round() as u64
        } else {
            seg_end_ns
        };
        sub.retain_keyframes_in_local_range(local_start, local_end);

        sub.clear_outgoing_transition();

        let timeline_duration = if speed != 0.0 {
            (seg_duration_ns as f64 / speed).round() as u64
        } else {
            seg_duration_ns
        };
        timeline_cursor += timeline_duration;
        sub_clips.push(sub);
    }

    let num_cuts = sub_clips.len().saturating_sub(1);

    // Build the new clip list for this track (total duration is preserved, no magnetic shift)
    let mut new_clips: Vec<Clip> = Vec::new();
    for clip in &old_clips {
        if clip.id == clip_id {
            new_clips.extend(sub_clips.iter().cloned());
        } else {
            new_clips.push(clip.clone());
        }
    }
    new_clips.sort_by_key(|c| c.timeline_start);

    {
        let mut st = timeline_state.borrow_mut();
        let proj_rc = st.project.clone();
        let mut proj = proj_rc.borrow_mut();
        let cmd = SetTrackClipsCommand {
            track_id: track_id.to_string(),
            old_clips,
            new_clips,
            label: "Detect scene cuts".to_string(),
        };
        st.history.execute(Box::new(cmd), &mut proj);
        proj.dirty = true;
    }

    on_project_changed();

    if let Some(win) = window {
        let msg = format!(
            "Detected {} scene cut(s) \u{2014} clip split into {} sub-clips",
            num_cuts,
            sub_clips.len()
        );
        flash_window_status_title(win, project, &msg);
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

pub(crate) fn proxy_scale_for_mode(
    mode: &crate::ui_state::ProxyMode,
) -> crate::media::proxy_cache::ProxyScale {
    match mode {
        crate::ui_state::ProxyMode::QuarterRes => crate::media::proxy_cache::ProxyScale::Quarter,
        _ => crate::media::proxy_cache::ProxyScale::Half,
    }
}

fn proxy_mode_label(mode: &crate::ui_state::ProxyMode) -> &'static str {
    match mode {
        crate::ui_state::ProxyMode::QuarterRes => "Quarter Resolution",
        _ => "Half Resolution",
    }
}

fn proxy_toggle_tooltip(
    current_proxy_mode: &crate::ui_state::ProxyMode,
    remembered_proxy_mode: &crate::ui_state::ProxyMode,
) -> String {
    if current_proxy_mode.is_enabled() {
        format!(
            "Proxy playback on ({}). Click to switch back to original media (Shift+P). Change Half/Quarter in Preferences.",
            proxy_mode_label(current_proxy_mode)
        )
    } else {
        format!(
            "Proxy playback off. Click to restore {} proxies (Shift+P). Change Half/Quarter in Preferences.",
            proxy_mode_label(remembered_proxy_mode)
        )
    }
}

fn proxy_toggle_label(mode: &crate::ui_state::ProxyMode) -> &'static str {
    if mode.is_enabled() {
        "Using Proxies"
    } else {
        "Original Media"
    }
}

fn background_render_label(enabled: bool) -> &'static str {
    if enabled {
        "Background Render"
    } else {
        "Live Rendering"
    }
}

fn ready_proxy_path_for_source(
    cache: &crate::media::proxy_cache::ProxyCache,
    source_path: &str,
    lut_key: Option<&str>,
) -> Option<String> {
    cache.get(source_path, lut_key).and_then(|proxy_path| {
        std::fs::metadata(proxy_path)
            .ok()
            .filter(|m| m.len() > 0)
            .map(|_| proxy_path.clone())
    })
}

fn reload_source_preview_selection(
    path: &str,
    duration_ns: u64,
    source_info: SourcePlacementInfo,
    player: &Rc<RefCell<Player>>,
    project: &Rc<RefCell<Project>>,
    proxy_cache: &Rc<RefCell<crate::media::proxy_cache::ProxyCache>>,
    proxy_mode: &crate::ui_state::ProxyMode,
    source_original_uri_for_proxy_fallback: &std::sync::Arc<std::sync::Mutex<Option<String>>>,
    set_audio_only: &Rc<dyn Fn(bool)>,
) {
    set_audio_only(source_info.is_audio_only);
    let original_uri = format!("file://{path}");
    let (fr_num, fr_den) = {
        let proj = project.borrow();
        (proj.frame_rate.numerator, proj.frame_rate.denominator)
    };
    if let Ok(mut fallback_uri) = source_original_uri_for_proxy_fallback.lock() {
        *fallback_uri = Some(original_uri.clone());
    }
    if proxy_mode.is_enabled() && !source_info.is_audio_only && !source_info.is_animated_svg {
        proxy_cache
            .borrow_mut()
            .request(path, proxy_scale_for_mode(proxy_mode), None);
    }
    let load_uri = if source_info.is_animated_svg {
        match crate::media::animated_svg::ensure_rendered_clip(
            path,
            0,
            duration_ns,
            Some(duration_ns),
            fr_num,
            fr_den,
        ) {
            Ok(render_path) => format!("file://{render_path}"),
            Err(err) => {
                log::warn!(
                    "source preview: failed to render animated SVG {}: {}",
                    path,
                    err
                );
                original_uri.clone()
            }
        }
    } else {
        let cache = proxy_cache.borrow();
        if proxy_mode.is_enabled() {
            if let Some(proxy_path) = ready_proxy_path_for_source(&cache, path, None) {
                log::info!("source preview: using proxy {}", proxy_path);
                format!("file://{proxy_path}")
            } else {
                log::info!("source preview: proxy not ready, loading original {}", path);
                original_uri.clone()
            }
        } else {
            original_uri.clone()
        }
    };
    let _ = player.borrow().load(&load_uri);
}

fn collect_unique_proxy_variants(
    project: &Project,
    scale: crate::media::proxy_cache::ProxyScale,
) -> Vec<crate::media::proxy_cache::ProxyVariantSpec> {
    let mut seen: HashSet<crate::media::proxy_cache::ProxyVariantSpec> = HashSet::new();
    project
        .tracks
        .iter()
        .filter(|t| t.is_video())
        .flat_map(|t| t.clips.iter())
        .filter_map(|c| {
            let spec = crate::media::proxy_cache::ProxyVariantSpec::new(
                c.source_path.clone(),
                scale,
                c.lut_key(),
                c.vidstab_enabled,
                c.vidstab_smoothing,
            );
            if seen.insert(spec.clone()) {
                Some(spec)
            } else {
                None
            }
        })
        .collect()
}

fn collect_unique_preview_lut_proxy_variants(
    project: &Project,
) -> Vec<crate::media::proxy_cache::ProxyVariantSpec> {
    let mut seen: HashSet<crate::media::proxy_cache::ProxyVariantSpec> = HashSet::new();
    let scale = crate::media::proxy_cache::ProxyScale::Project {
        width: project.width,
        height: project.height,
    };
    project
        .tracks
        .iter()
        .filter(|t| t.is_video())
        .flat_map(|t| t.clips.iter())
        .filter_map(|c| {
            let lut = c.lut_key()?;
            if lut.is_empty() {
                return None;
            }
            let spec = crate::media::proxy_cache::ProxyVariantSpec::new(
                c.source_path.clone(),
                scale,
                Some(lut),
                false,
                0.0,
            );
            if seen.insert(spec.clone()) {
                Some(spec)
            } else {
                None
            }
        })
        .collect()
}

fn collect_near_playhead_proxy_variants(
    project: &Project,
    playhead_ns: u64,
    window_ns: u64,
    max_items: usize,
    scale: crate::media::proxy_cache::ProxyScale,
) -> Vec<crate::media::proxy_cache::ProxyVariantSpec> {
    if max_items == 0 {
        return Vec::new();
    }
    let window_start = playhead_ns.saturating_sub(window_ns);
    let window_end = playhead_ns.saturating_add(window_ns);

    let mut candidates: Vec<(u64, u64, crate::media::proxy_cache::ProxyVariantSpec)> = project
        .tracks
        .iter()
        .filter(|t| t.is_video())
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
                crate::media::proxy_cache::ProxyVariantSpec::new(
                    c.source_path.clone(),
                    scale,
                    c.lut_key(),
                    c.vidstab_enabled,
                    c.vidstab_smoothing,
                ),
            )
        })
        .collect();

    candidates.sort_by_key(|(distance, timeline_start, _)| (*distance, *timeline_start));

    let mut out = Vec::new();
    let mut seen: HashSet<crate::media::proxy_cache::ProxyVariantSpec> = HashSet::new();
    for (_, _, spec) in candidates {
        if seen.insert(spec.clone()) {
            out.push(spec);
            if out.len() >= max_items {
                break;
            }
        }
    }
    out
}

fn request_proxy_variants(
    cache: &mut crate::media::proxy_cache::ProxyCache,
    variants: &[crate::media::proxy_cache::ProxyVariantSpec],
) {
    for variant in variants {
        cache.request_with_vidstab(
            &variant.source_path,
            variant.scale,
            variant.lut_key(),
            variant.vidstab_enabled,
            variant.vidstab_smoothing(),
        );
    }
}

/// Cancel any pending title-flush timer and schedule a new one.
/// The timer fires after ~32ms of idle, calling
/// `ProgramPlayer::flush_compositor_for_title_update()` once.
/// This coalesces rapid keystrokes / slider drags into a single flush.
fn schedule_title_flush(
    timer_raw: &Rc<Cell<u32>>,
    prog_player: &Rc<RefCell<crate::media::program_player::ProgramPlayer>>,
) {
    use glib::translate::FromGlib;
    // Cancel previous pending flush
    let old = timer_raw.get();
    if old != 0 {
        unsafe { glib::SourceId::from_glib(old) }.remove();
        timer_raw.set(0);
    }
    let pp = prog_player.clone();
    let timer = timer_raw.clone();
    let new_id = glib::timeout_add_local_once(std::time::Duration::from_millis(32), move || {
        timer.set(0);
        pp.borrow().flush_compositor_for_title_update();
    });
    timer_raw.set(new_id.as_raw());
}

/// Convert a single `Clip` into one or more `ProgramClip` entries.
/// For regular clips this returns a single-element Vec; for compound clips it
/// recursively flattens internal tracks into program clips with adjusted
/// timeline positions and track indices.
fn clip_to_program_clips(
    c: &crate::model::clip::Clip,
    audio_only: bool,
    duck: bool,
    duck_amount_db: f64,
    track_index: usize,
    suppress_embedded_audio_ids: &std::collections::HashSet<String>,
    timeline_offset: u64,
    depth: usize,
) -> Vec<ProgramClip> {
    use crate::model::clip::ClipKind;

    // Compound clips: recursively flatten internal clips
    if c.kind == ClipKind::Compound && depth < 16 {
        if let Some(ref internal_tracks) = c.compound_tracks {
            // Map internal clip positions into the parent timeline.
            // After windowing, each clip's timeline_start is >= source_in,
            // so subtracting source_in gives the offset from the compound's
            // visible start. Adding the compound's parent position gives the
            // absolute position without any u64 underflow risk.
            let compound_offset = timeline_offset.saturating_add(c.timeline_start);
            let window_start = c.source_in;
            let window_end = c.source_out;
            let mut result = Vec::new();
            for (inner_idx, inner_track) in internal_tracks.iter().enumerate() {
                let inner_audio = inner_track.is_audio();
                for inner_clip in &inner_track.clips {
                    // Window the clip to the compound's [source_in, source_out)
                    // range. Skip / trim / rebase keyframes & subtitles all
                    // happen inside the helper.
                    let Some(mut windowed) = inner_clip.rebase_to_window(window_start, window_end)
                    else {
                        continue;
                    };
                    let inner_track_idx = track_index + inner_idx;
                    // Rebase: offset from window start + compound parent pos
                    windowed.timeline_start = windowed.timeline_start.saturating_sub(window_start);
                    result.extend(clip_to_program_clips(
                        &windowed,
                        inner_audio,
                        inner_track.duck,
                        inner_track.duck_amount_db,
                        inner_track_idx,
                        suppress_embedded_audio_ids,
                        compound_offset,
                        depth + 1,
                    ));
                }
            }
            return result;
        }
        return Vec::new();
    }

    // Multicam clips: expand into sequential ProgramClips per angle segment
    if c.kind == ClipKind::Multicam {
        let clip_start = timeline_offset.saturating_add(c.timeline_start);
        let clip_dur = c.duration();
        let segments = c.multicam_segments();
        let mut result = Vec::new();

        // Video segments: one ProgramClip per contiguous angle segment (no embedded audio)
        // Segments are window-relative (0 = visible start); add source_in
        // to map back to the angle's internal timeline.
        for (seg_start, seg_end, angle_idx) in &segments {
            if let Some(angle) = c.multicam_angles.as_ref().and_then(|a| a.get(*angle_idx)) {
                let angle_source_in = angle
                    .source_in
                    .saturating_add(c.source_in)
                    .saturating_add(*seg_start);
                let angle_source_out = angle
                    .source_in
                    .saturating_add(c.source_in)
                    .saturating_add(*seg_end)
                    .min(angle.source_out);
                let mut seg_clip = crate::model::clip::Clip::new(
                    &angle.source_path,
                    angle_source_out,
                    0,
                    ClipKind::Video,
                );
                seg_clip.source_in = angle_source_in;
                seg_clip.source_out = angle_source_out;
                seg_clip.timeline_start = clip_start.saturating_add(*seg_start);
                let mut seg_results = clip_to_program_clips(
                    &seg_clip,
                    false, // not audio_only
                    duck,
                    duck_amount_db,
                    track_index,
                    suppress_embedded_audio_ids,
                    0,
                    depth + 1,
                );
                // Video segments have no embedded audio (audio comes from the mix below)
                for pc in &mut seg_results {
                    pc.has_audio = false;
                }
                result.extend(seg_results);
            }
        }

        // Audio mix: one continuous audio ProgramClip per unmuted angle spanning the full multicam duration
        // Add source_in to map into the correct part of the angle source.
        if let Some(ref angles) = c.multicam_angles {
            for (ai, angle) in angles.iter().enumerate() {
                if angle.muted {
                    continue;
                }
                let angle_source_in = angle.source_in.saturating_add(c.source_in);
                let angle_source_out = angle
                    .source_in
                    .saturating_add(c.source_in)
                    .saturating_add(clip_dur)
                    .min(angle.source_out);
                let mut audio_clip = crate::model::clip::Clip::new(
                    &angle.source_path,
                    angle_source_out,
                    clip_start,
                    ClipKind::Audio,
                );
                audio_clip.source_in = angle_source_in;
                audio_clip.source_out = angle_source_out;
                audio_clip.volume = angle.volume;
                audio_clip.id = format!("{}-audio-{}", c.id, ai);
                let audio_results = clip_to_program_clips(
                    &audio_clip,
                    true, // audio_only
                    duck,
                    duck_amount_db,
                    track_index + angles.len() + ai, // offset track index to avoid collisions
                    suppress_embedded_audio_ids,
                    0,
                    depth + 1,
                );
                result.extend(audio_results);
            }
        }

        return result;
    }

    let effective_timeline_start = timeline_offset.saturating_add(c.timeline_start);

    vec![ProgramClip {
        id: c.id.clone(),
        source_path: c.source_path.clone(),
        source_in_ns: c.source_in,
        source_out_ns: c.source_out,
        timeline_start_ns: effective_timeline_start,
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
        blur: c.blur as f64,
        blur_keyframes: c.blur_keyframes.clone(),
        vidstab_enabled: c.vidstab_enabled,
        vidstab_smoothing: c.vidstab_smoothing,
        volume: c.volume as f64,
        voice_isolation: c.voice_isolation as f64,
        voice_enhance: c.voice_enhance,
        voice_enhance_strength: c.voice_enhance_strength,
        voice_isolation_pad_ns: (c.voice_isolation_pad_ms as f64 * 1_000_000.0) as u64,
        voice_isolation_fade_ns: (c.voice_isolation_fade_ms as f64 * 1_000_000.0) as u64,
        voice_isolation_floor: c.voice_isolation_floor as f64,
        volume_keyframes: c.volume_keyframes.clone(),
        voice_isolation_merged_intervals_ns: c.voice_isolation_speech_intervals_ns(
            (c.voice_isolation_pad_ms as f64 * 1_000_000.0) as u64,
        ),
        pan: c.pan as f64,
        pan_keyframes: c.pan_keyframes.clone(),
        audio_channel_mode: c.audio_channel_mode,
        eq_bands: c.eq_bands,
        eq_low_gain_keyframes: c.eq_low_gain_keyframes.clone(),
        eq_mid_gain_keyframes: c.eq_mid_gain_keyframes.clone(),
        eq_high_gain_keyframes: c.eq_high_gain_keyframes.clone(),
        match_eq_bands: c.match_eq_bands.clone(),
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
        motion_blur_enabled: c.motion_blur_enabled,
        motion_blur_shutter_angle: c.motion_blur_shutter_angle,
        title_text: c.title_text.clone(),
        title_font: c.title_font.clone(),
        title_color: c.title_color,
        title_x: c.title_x,
        title_y: c.title_y,
        title_outline_color: c.title_outline_color,
        title_outline_width: c.title_outline_width,
        title_shadow: c.title_shadow,
        title_shadow_color: c.title_shadow_color,
        title_shadow_offset_x: c.title_shadow_offset_x,
        title_shadow_offset_y: c.title_shadow_offset_y,
        title_bg_box: c.title_bg_box,
        title_bg_box_color: c.title_bg_box_color,
        title_bg_box_padding: c.title_bg_box_padding,
        title_clip_bg_color: c.title_clip_bg_color,
        title_secondary_text: c.title_secondary_text.clone(),
        is_title: c.kind == ClipKind::Title,
        speed: c.speed,
        speed_keyframes: c.speed_keyframes.clone(),
        slow_motion_interp: c.slow_motion_interp,
        reverse: c.reverse,
        freeze_frame: c.freeze_frame,
        freeze_frame_source_ns: c.freeze_frame_source_ns,
        freeze_frame_hold_duration_ns: c.freeze_frame_hold_duration_ns,
        is_audio_only: audio_only,
        duck,
        duck_amount_db,
        ladspa_effects: c.ladspa_effects.clone(),
        pitch_shift_semitones: c.pitch_shift_semitones,
        pitch_preserve: c.pitch_preserve,
        anamorphic_desqueeze: c.anamorphic_desqueeze,
        track_index,
        transition_after: c.outgoing_transition.kind_trimmed().to_string(),
        transition_after_ns: c.outgoing_transition.duration_ns,
        transition_alignment: c.outgoing_transition.alignment,
        lut_paths: c.lut_paths.clone(),
        scale: c.scale,
        scale_keyframes: c.scale_keyframes.clone(),
        opacity: c.opacity,
        opacity_keyframes: c.opacity_keyframes.clone(),
        blend_mode: c.blend_mode,
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
            && c.kind != ClipKind::Title
            && c.kind != ClipKind::Adjustment
            && c.kind != ClipKind::Compound
            && c.kind != ClipKind::Multicam
            && !suppress_embedded_audio_ids.contains(&c.id),
        // Defensive: ClipKind::Image is the source of truth, but also fall
        // back to extension sniffing in case the kind drifted on a stale or
        // hand-edited project. A still that loses its `is_image` flag would
        // be sent down the time-based decoder path and disappear during
        // playback/reseeks. Only override for Video/Image kinds — Title /
        // Adjustment / Compound / Multicam each have their own pipeline
        // branches and must not be retagged based on a source extension.
        is_image: {
            let kind_says_image = c.kind == ClipKind::Image;
            let path_says_image = crate::model::clip::is_image_file(&c.source_path);
            let kind_is_videoish = matches!(c.kind, ClipKind::Image | ClipKind::Video);
            if kind_is_videoish && kind_says_image != path_says_image {
                log::warn!(
                    "clip_to_program_clips: is_image kind/extension mismatch for clip {} ({}): kind={:?} path_is_image={} — preferring extension",
                    c.id,
                    c.source_path,
                    c.kind,
                    path_says_image,
                );
            }
            kind_says_image || (kind_is_videoish && path_says_image)
        },
        animated_svg: c.animated_svg,
        media_duration_ns: c.media_duration_ns,
        is_adjustment: c.kind == ClipKind::Adjustment,
        chroma_key_enabled: c.chroma_key_enabled,
        chroma_key_color: c.chroma_key_color,
        chroma_key_tolerance: c.chroma_key_tolerance,
        chroma_key_softness: c.chroma_key_softness,
        bg_removal_enabled: c.bg_removal_enabled,
        bg_removal_threshold: c.bg_removal_threshold,
        frei0r_effects: c.frei0r_effects.clone(),
        tracking_binding: c.tracking_binding.clone(),
        masks: c.masks.clone(),
        hsl_qualifier: c.hsl_qualifier.clone(),
    }]
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
    let library: Rc<RefCell<MediaLibrary>> = Rc::new(RefCell::new(MediaLibrary::new()));
    let preferences_state = Rc::new(RefCell::new(crate::ui_state::load_preferences_state()));
    let workspace_layouts_state =
        Rc::new(RefCell::new(crate::ui_state::load_workspace_layouts_state()));

    // Apply the persisted AI backend preference to the process-wide
    // ai_providers singleton so every ONNX cache worker (bg removal,
    // frame interpolation, music gen, SAM in later phases) picks it
    // up on the next job without plumbing the preference through
    // every job struct. Feature-gated: when `ai-inference` is off
    // the module isn't compiled in and this block is skipped.
    #[cfg(feature = "ai-inference")]
    {
        let backend_id = preferences_state.borrow().ai_backend.clone();
        let backend = crate::media::ai_providers::AiBackend::from_id(&backend_id);
        crate::media::ai_providers::set_current_backend(backend);
        let report = crate::media::ai_providers::detect_backends();
        log::info!(
            "AI backend: preferred={} ({}) — {}",
            backend.label(),
            backend.as_id(),
            report.describe()
        );

        // If WebGPU is compiled in and the user's selected backend
        // is WebGpu or Auto, pre-trigger Dawn device creation with
        // stderr silenced so Dawn's "limits artificially reduced"
        // warnings don't interleave with user output during the
        // first real MusicGen / SAM / MODNet / RIFE inference call.
        // See `ai_providers::prewarm_webgpu_if_needed` for the full
        // rationale and mechanism.
        crate::media::ai_providers::prewarm_webgpu_if_needed();
    }

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
    let initial_prerender_preset = preferences_state.borrow().prerender_preset.clone();
    let initial_prerender_crf = preferences_state.borrow().prerender_crf;
    let initial_persist_proxies_next_to_original_media = preferences_state
        .borrow()
        .persist_proxies_next_to_original_media;
    let initial_persist_prerenders_next_to_project_file = preferences_state
        .borrow()
        .persist_prerenders_next_to_project_file;
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
                // Debounce: ignore repeated errors within 2 s of the last
                // handled error.  VA-API dmabuf errors can flood in at 30 fps;
                // without a cooldown the proxy-fallback code runs on every
                // already-queued error message after the first reload, causing
                // an infinite HW→SW→HW→… loop.
                let last_error_handled = std::rc::Rc::new(std::cell::Cell::new(
                    std::time::Instant::now()
                        .checked_sub(std::time::Duration::from_secs(10))
                        .unwrap_or(std::time::Instant::now()),
                ));
                let _watch = bus.add_watch_local(move |_bus, msg: &gstreamer::Message| {
                    use gstreamer::MessageView;
                    match msg.view() {
                        MessageView::Error(err) => {
                            // Debounce: only act on errors that are >2s apart.
                            let now = std::time::Instant::now();
                            let elapsed = now.duration_since(last_error_handled.get());
                            if elapsed < std::time::Duration::from_secs(2) {
                                return glib::ControlFlow::Continue;
                            }
                            last_error_handled.set(now);
                            log::error!(
                                "Source preview pipeline error: {} (debug: {:?})",
                                err.error(),
                                err.debug()
                            );
                            let mut hw_fallback_applied = false;
                            // Any source-preview pipeline error while in HW mode should trigger
                            // a software-decode retry. Restricting this to specific substrings
                            // misses some backend-specific error messages that still manifest
                            // as "audio plays, video black" in the source monitor.
                            match player_for_bus.borrow().fallback_to_software_after_error() {
                                Ok(true) => {
                                    hw_fallback_applied = true;
                                    log::warn!(
                                        "Source preview fallback: switched to software decode mode after HW-path error"
                                    );
                                }
                                Ok(false) => {}
                                Err(e) => log::error!("Source preview fallback failed: {e:#}"),
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
                // BusWatchGuard removes the watch when dropped.  Intentionally
                // leak it so the watch stays active for the entire app lifetime.
                std::mem::forget(_watch);
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
    prog_player_raw.set_prerender_quality(initial_prerender_preset, initial_prerender_crf);
    {
        let p = project.borrow();
        prog_player_raw.set_prerender_project_path(
            p.file_path.as_deref(),
            initial_persist_prerenders_next_to_project_file,
        );
    }
    prog_player_raw.set_audio_crossfade_preview(
        preferences_state.borrow().crossfade_enabled,
        preferences_state.borrow().crossfade_curve.clone(),
        preferences_state.borrow().crossfade_duration_ns,
    );
    prog_player_raw.set_duck_settings(
        preferences_state.borrow().duck_enabled,
        preferences_state.borrow().duck_amount_db,
    );
    let prog_player = Rc::new(RefCell::new(prog_player_raw));

    let proxy_cache = Rc::new(RefCell::new(crate::media::proxy_cache::ProxyCache::new()));
    proxy_cache.borrow_mut().set_sidecar_mirror_enabled(
        initial_proxy_mode.is_enabled() && initial_persist_proxies_next_to_original_media,
    );
    let bg_removal_cache = Rc::new(RefCell::new(
        crate::media::bg_removal_cache::BgRemovalCache::new(),
    ));
    let voice_enhance_cache = Rc::new(RefCell::new(
        crate::media::voice_enhance_cache::VoiceEnhanceCache::new(),
    ));
    // Apply the persisted preference cap so the LRU eviction respects
    // the user's chosen disk budget from the start of the session.
    {
        let cap_gib = preferences_state.borrow().voice_enhance_cache_cap_gib;
        let cap_bytes = (cap_gib.max(0.5) * 1024.0 * 1024.0 * 1024.0) as u64;
        voice_enhance_cache
            .borrow_mut()
            .set_cache_cap_bytes(cap_bytes);
    }
    let frame_interp_cache = Rc::new(RefCell::new(
        crate::media::frame_interp_cache::FrameInterpCache::new(),
    ));
    let stt_cache = Rc::new(RefCell::new(crate::media::stt_cache::SttCache::new()));
    let tracking_cache = Rc::new(RefCell::new(crate::media::tracking::TrackingCache::new()));
    let music_gen_cache = Rc::new(RefCell::new(crate::media::music_gen::MusicGenCache::new()));
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
    let sync_proxy_toggle_impl: Rc<
        RefCell<Option<Rc<dyn Fn(&crate::ui_state::PreferencesState)>>>,
    > = Rc::new(RefCell::new(None));
    let sync_background_render_toggle_impl: Rc<
        RefCell<Option<Rc<dyn Fn(&crate::ui_state::PreferencesState)>>>,
    > = Rc::new(RefCell::new(None));
    let refresh_source_preview_preferences_impl: Rc<
        RefCell<
            Option<
                Rc<dyn Fn(&crate::ui_state::PreferencesState, &crate::ui_state::PreferencesState)>,
            >,
        >,
    > = Rc::new(RefCell::new(None));
    let apply_preferences_state: Rc<dyn Fn(crate::ui_state::PreferencesState)> = {
        let preferences_state = preferences_state.clone();
        let player = player.clone();
        let prog_player = prog_player.clone();
        let proxy_cache = proxy_cache.clone();
        let voice_enhance_cache_apply = voice_enhance_cache.clone();
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let mcp_sender = mcp_sender.clone();
        let mcp_socket_stop = mcp_socket_stop.clone();
        let sync_proxy_toggle_impl = sync_proxy_toggle_impl.clone();
        let sync_background_render_toggle_impl = sync_background_render_toggle_impl.clone();
        let refresh_source_preview_preferences_impl =
            refresh_source_preview_preferences_impl.clone();
        Rc::new(move |mut new_state| {
            let old_state = preferences_state.borrow().clone();
            if !new_state.last_non_off_proxy_mode.is_enabled() {
                new_state.last_non_off_proxy_mode = old_state.remembered_proxy_mode();
            }
            *preferences_state.borrow_mut() = new_state.clone();
            crate::ui_state::save_preferences_state(&new_state);
            if let Err(e) = player
                .borrow()
                .set_hardware_acceleration(new_state.hardware_acceleration_enabled)
            {
                log::warn!("Failed to apply hardware acceleration setting: {e}");
            }
            player
                .borrow()
                .set_source_playback_priority(new_state.source_playback_priority.clone());
            prog_player
                .borrow_mut()
                .set_playback_priority(new_state.playback_priority.clone());
            prog_player
                .borrow_mut()
                .set_proxy_enabled(new_state.proxy_mode.is_enabled());
            proxy_cache.borrow_mut().set_sidecar_mirror_enabled(
                new_state.proxy_mode.is_enabled()
                    && new_state.persist_proxies_next_to_original_media,
            );
            prog_player
                .borrow_mut()
                .set_proxy_scale_divisor(match new_state.proxy_mode {
                    crate::ui_state::ProxyMode::QuarterRes => 4,
                    _ => 2,
                });
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
                .set_prerender_quality(new_state.prerender_preset.clone(), new_state.prerender_crf);
            let project_file_path = { project.borrow().file_path.clone() };
            prog_player.borrow_mut().set_prerender_project_path(
                project_file_path.as_deref(),
                new_state.persist_prerenders_next_to_project_file,
            );
            prog_player
                .borrow_mut()
                .set_preview_luts(new_state.preview_luts);
            prog_player.borrow_mut().set_audio_crossfade_preview(
                new_state.crossfade_enabled,
                new_state.crossfade_curve.clone(),
                new_state.crossfade_duration_ns,
            );
            prog_player
                .borrow_mut()
                .set_duck_settings(new_state.duck_enabled, new_state.duck_amount_db);
            // Push the cache cap to the voice enhance cache so future
            // request() calls evict against the new ceiling.
            {
                let cap_bytes = (new_state
                    .voice_enhance_cache_cap_gib
                    .max(0.5)
                    * 1024.0
                    * 1024.0
                    * 1024.0) as u64;
                voice_enhance_cache_apply
                    .borrow_mut()
                    .set_cache_cap_bytes(cap_bytes);
            }
            if new_state.proxy_mode.is_enabled() {
                // If the proxy scale changed, invalidate old entries so clips are
                // re-transcoded at the new resolution.
                if new_state.proxy_mode != old_state.proxy_mode
                    || new_state.preview_luts != old_state.preview_luts
                {
                    proxy_cache.borrow_mut().invalidate_all();
                }
                let scale = match new_state.proxy_mode {
                    crate::ui_state::ProxyMode::QuarterRes => {
                        crate::media::proxy_cache::ProxyScale::Quarter
                    }
                    _ => crate::media::proxy_cache::ProxyScale::Half,
                };
                let variants = {
                    let proj = project.borrow();
                    collect_unique_proxy_variants(&proj, scale)
                };
                {
                    let mut cache = proxy_cache.borrow_mut();
                    cache.cleanup_stale_variants(&variants);
                    request_proxy_variants(&mut cache, &variants);
                }
                let paths = proxy_cache.borrow().proxies.clone();
                prog_player.borrow_mut().update_proxy_paths(paths);
            } else if new_state.preview_luts {
                if new_state.proxy_mode != old_state.proxy_mode
                    || new_state.preview_luts != old_state.preview_luts
                {
                    proxy_cache.borrow_mut().invalidate_all();
                }
                let variants = {
                    let proj = project.borrow();
                    collect_unique_preview_lut_proxy_variants(&proj)
                };
                {
                    let mut cache = proxy_cache.borrow_mut();
                    cache.cleanup_stale_variants(&variants);
                    request_proxy_variants(&mut cache, &variants);
                }
                let paths = proxy_cache.borrow().proxies.clone();
                prog_player.borrow_mut().update_proxy_paths(paths);
            } else {
                prog_player.borrow_mut().update_proxy_paths(HashMap::new());
            }
            timeline_state.borrow_mut().show_waveform_on_video = new_state.show_waveform_on_video;
            timeline_state.borrow_mut().show_timeline_preview = new_state.show_timeline_preview;
            timeline_state.borrow_mut().show_track_audio_levels = new_state.show_track_audio_levels;
            // Start/stop MCP socket server based on preference change.
            if new_state.mcp_socket_enabled && mcp_socket_stop.borrow().is_none() {
                let stop = crate::mcp::start_mcp_socket_server((*mcp_sender).clone());
                *mcp_socket_stop.borrow_mut() = Some(stop);
            } else if !new_state.mcp_socket_enabled {
                if let Some(stop) = mcp_socket_stop.borrow_mut().take() {
                    stop.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            if let Some(sync_proxy_toggle) = sync_proxy_toggle_impl.borrow().as_ref().cloned() {
                sync_proxy_toggle(&new_state);
            }
            if let Some(sync_background_render_toggle) = sync_background_render_toggle_impl
                .borrow()
                .as_ref()
                .cloned()
            {
                sync_background_render_toggle(&new_state);
            }
            if let Some(refresh_source_preview) = refresh_source_preview_preferences_impl
                .borrow()
                .as_ref()
                .cloned()
            {
                refresh_source_preview(&old_state, &new_state);
            }
        })
    };
    let proxy_toggle_updating = Rc::new(Cell::new(false));
    let toggle_proxy_quick: Rc<dyn Fn(bool)> = {
        let preferences_state = preferences_state.clone();
        let apply_preferences_state = apply_preferences_state.clone();
        let proxy_toggle_updating = proxy_toggle_updating.clone();
        Rc::new(move |enabled| {
            if proxy_toggle_updating.get() {
                return;
            }
            let mut new_state = preferences_state.borrow().clone();
            new_state.set_proxy_enabled(enabled);
            apply_preferences_state(new_state);
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
        let bg_removal_cache = bg_removal_cache.clone();
        let apply_preferences_state = apply_preferences_state.clone();
        Rc::new(move || {
            if let Some(win) = window_weak.upgrade() {
                let current = preferences_state.borrow().clone();
                preferences::show_preferences_dialog(
                    win.upcast_ref(),
                    current,
                    apply_preferences_state.clone(),
                    bg_removal_cache.clone(),
                );
            }
        })
    });

    // ── Build inspector (after on_project_changed is defined so we can pass it) ──
    // timeline_panel_cell is shared between the inspector's on_audio_changed callback
    // and the program monitor poll timer. Declare it early (filled in after timeline build).
    let timeline_panel_cell: Rc<RefCell<Option<gtk4::Widget>>> = Rc::new(RefCell::new(None));
    // Shared flag for normalize-in-progress state (used by callback + button UI).
    let norm_in_progress: Rc<std::cell::Cell<bool>> = Rc::new(std::cell::Cell::new(false));
    let match_audio_in_progress: Rc<std::cell::Cell<bool>> = Rc::new(std::cell::Cell::new(false));
    let keyframe_editor_cell: Rc<RefCell<Option<Rc<keyframe_editor::KeyframeEditorView>>>> =
        Rc::new(RefCell::new(None));
    let transcript_panel_cell: Rc<RefCell<Option<Rc<crate::ui::transcript_panel::TranscriptPanelView>>>> =
        Rc::new(RefCell::new(None));
    // transform_overlay_cell holds the TransformOverlay after the program monitor is built.
    let transform_overlay_cell: Rc<
        RefCell<Option<Rc<crate::ui::transform_overlay::TransformOverlay>>>,
    > = Rc::new(RefCell::new(None));
    let tracking_job_owner_by_key: Rc<RefCell<HashMap<String, String>>> =
        Rc::new(RefCell::new(HashMap::new()));
    let tracking_job_key_by_clip: Rc<RefCell<HashMap<String, String>>> =
        Rc::new(RefCell::new(HashMap::new()));
    let tracking_status_by_clip: Rc<RefCell<HashMap<String, (String, bool)>>> =
        Rc::new(RefCell::new(HashMap::new()));
    let on_relink_media_impl: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_relink_media_gui: Rc<dyn Fn()> = {
        let cb = on_relink_media_impl.clone();
        Rc::new(move || {
            if let Some(f) = cb.borrow().as_ref() {
                f();
            }
        })
    };
    // Shared debounce timer for title property updates.  Both on_title_changed
    // and on_title_style_changed set GStreamer properties instantly, then
    // schedule a single compositor flush after a brief idle period so rapid
    // keystrokes / slider drags don't flood GStreamer with flush seeks.
    let title_flush_timer: Rc<Cell<u32>> = Rc::new(Cell::new(0));

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
            move |b, c, s, temp, tnt, d, sh, bl, shd, mid, hil, exp, bp, hw, ht, mw, mt, sw, st| {
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
                    bl as f64,
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
            move |clip_id: &str, vol: f32, pan: f32, voice_isolation: f32| {
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
                        if let Some(model_clip) = proj.clip_ref(&clip_id) {
                            // Sync to video clips (embedded audio)
                            for player_clip in pp.clips.iter_mut().filter(|c| c.id == clip_id) {
                                player_clip.volume_keyframes = model_clip.volume_keyframes.clone();
                                player_clip.pan_keyframes = model_clip.pan_keyframes.clone();
                                player_clip.voice_isolation = voice_isolation as f64;
                            }
                            // Sync to audio-only clips
                            for audio_clip in pp.audio_clips.iter_mut().filter(|c| c.id == clip_id)
                            {
                                audio_clip.volume_keyframes = model_clip.volume_keyframes.clone();
                                audio_clip.pan_keyframes = model_clip.pan_keyframes.clone();
                                audio_clip.voice_isolation = voice_isolation as f64;
                            }
                        }
                    }
                    pp.update_audio_for_clip(
                        clip_id,
                        vol as f64,
                        pan as f64,
                        voice_isolation as f64,
                    );
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
        // on_eq_changed: EQ slider → direct update, no pipeline reload
        {
            let prog_player = prog_player.clone();
            let window_weak = window_weak.clone();
            let project = project.clone();
            move |clip_id: &str, eq_bands: [crate::model::clip::EqBand; 3]| {
                {
                    let mut proj = project.borrow_mut();
                    proj.dirty = true;
                }
                {
                    let mut pp = prog_player.borrow_mut();
                    // Sync EQ keyframes from model to player
                    {
                        let proj = project.borrow();
                        if let Some(model_clip) = proj.clip_ref(&clip_id) {
                            if let Some(player_clip) = pp.clips.iter_mut().find(|c| c.id == clip_id)
                            {
                                player_clip.eq_bands = model_clip.eq_bands;
                                player_clip.eq_low_gain_keyframes =
                                    model_clip.eq_low_gain_keyframes.clone();
                                player_clip.eq_mid_gain_keyframes =
                                    model_clip.eq_mid_gain_keyframes.clone();
                                player_clip.eq_high_gain_keyframes =
                                    model_clip.eq_high_gain_keyframes.clone();
                            }
                            if let Some(audio_clip) =
                                pp.audio_clips.iter_mut().find(|c| c.id == clip_id)
                            {
                                audio_clip.eq_bands = model_clip.eq_bands;
                                audio_clip.eq_low_gain_keyframes =
                                    model_clip.eq_low_gain_keyframes.clone();
                                audio_clip.eq_mid_gain_keyframes =
                                    model_clip.eq_mid_gain_keyframes.clone();
                                audio_clip.eq_high_gain_keyframes =
                                    model_clip.eq_high_gain_keyframes.clone();
                            }
                        }
                    }
                    pp.update_eq_for_clip(clip_id, eq_bands);
                }
                if let Some(win) = window_weak.upgrade() {
                    let proj = project.borrow();
                    let title = format!("UltimateSlice — {} •", proj.title);
                    win.set_title(Some(&title));
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
                let selected = timeline_state.borrow().selected_clip_id.clone();
                let is_adjustment = {
                    let proj = project.borrow();
                    selected_clip_is_adjustment(&proj, selected.as_deref())
                };
                if !is_adjustment {
                    player.borrow().set_transform(cl, cr, ct, cb, rot, fh, fv);
                }
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
        // on_anamorphic_changed: changes pixel aspect ratio -> requires pipeline reload
        {
            let on_project_changed = on_project_changed.clone();
            let project = project.clone();
            let timeline_state = timeline_state.clone();
            move |factor| {
                let selected = timeline_state.borrow().selected_clip_id.clone();
                if let Some(ref clip_id) = selected {
                    let mut proj = project.borrow_mut();
                    let mut changed = false;
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        if (clip.anamorphic_desqueeze - factor).abs() > 0.001 {
                            clip.anamorphic_desqueeze = factor;
                            changed = true;
                        }
                    }
                    if changed {
                        proj.dirty = true;
                        drop(proj);
                        on_project_changed();
                    }
                }
            }
        },
        // on_title_changed: text/position → instant property set + debounced flush
        {
            let prog_player = prog_player.clone();
            let project = project.clone();
            let timeline_state = timeline_state.clone();
            let window_weak = window_weak.clone();
            let flush_timer = title_flush_timer.clone();
            move |text: String, x: f64, y: f64| {
                let selected = timeline_state.borrow().selected_clip_id.clone();
                // Get font/color from the selected clip
                let (font, color) = {
                    let proj = project.borrow();
                    if let Some(ref clip_id) = selected {
                        proj.tracks
                            .iter()
                            .flat_map(|t| t.clips.iter())
                            .find(|c| &c.id == clip_id)
                            .map(|c| (c.title_font.clone(), c.title_color))
                            .unwrap_or(("Sans Bold 36".to_string(), 0xFFFFFFFF))
                    } else {
                        ("Sans Bold 36".to_string(), 0xFFFFFFFF)
                    }
                };
                // Instant: set GStreamer textoverlay properties (non-blocking)
                let pp = prog_player.borrow();
                if let Some(ref clip_id) = selected {
                    pp.update_title_for_clip(clip_id, &text, &font, color, x, y);
                } else {
                    pp.update_current_title(&text, &font, color, x, y);
                }
                drop(pp);
                // Debounced: schedule a single compositor flush after 32ms idle
                schedule_title_flush(&flush_timer, &prog_player);
                if let Some(win) = window_weak.upgrade() {
                    let proj = project.borrow();
                    let title = format!("UltimateSlice — {} •", proj.title);
                    win.set_title(Some(&title));
                }
            }
        },
        // on_title_style_changed: outline/shadow/bg_box → instant property set + debounced flush
        {
            let prog_player = prog_player.clone();
            let window_weak = window_weak.clone();
            let project = project.clone();
            let timeline_state = timeline_state.clone();
            let flush_timer = title_flush_timer.clone();
            move || {
                let selected = timeline_state.borrow().selected_clip_id.clone();
                let (outline_width, outline_color, shadow, bg_box) = {
                    let proj = project.borrow();
                    if let Some(ref clip_id) = selected {
                        proj.tracks
                            .iter()
                            .flat_map(|t| t.clips.iter())
                            .find(|c| &c.id == clip_id)
                            .map(|c| {
                                (
                                    c.title_outline_width,
                                    c.title_outline_color,
                                    c.title_shadow,
                                    c.title_bg_box,
                                )
                            })
                            .unwrap_or((0.0, 0x000000FF, false, false))
                    } else {
                        (0.0, 0x000000FF, false, false)
                    }
                };
                // Instant: set GStreamer textoverlay properties (non-blocking)
                let pp = prog_player.borrow();
                if let Some(ref clip_id) = selected {
                    pp.update_title_style_for_clip(
                        clip_id,
                        outline_width,
                        outline_color,
                        shadow,
                        bg_box,
                    );
                } else {
                    pp.update_current_title_style(outline_width, outline_color, shadow, bg_box);
                }
                drop(pp);
                // Debounced: schedule a single compositor flush after 32ms idle
                schedule_title_flush(&flush_timer, &prog_player);
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
                    let variants = {
                        let proj = project.borrow();
                        if prefs.proxy_mode.is_enabled() {
                            collect_unique_proxy_variants(
                                &proj,
                                match prefs.proxy_mode {
                                    crate::ui_state::ProxyMode::QuarterRes => {
                                        crate::media::proxy_cache::ProxyScale::Quarter
                                    }
                                    _ => crate::media::proxy_cache::ProxyScale::Half,
                                },
                            )
                        } else {
                            collect_unique_preview_lut_proxy_variants(&proj)
                        }
                    };
                    {
                        let mut cache = proxy_cache.borrow_mut();
                        cache.invalidate_all();
                        cache.cleanup_stale_variants(&variants);
                        request_proxy_variants(&mut cache, &variants);
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
        // on_vidstab_changed: stabilization toggle/slider → project-changed + proxy re-request
        {
            let on_project_changed = on_project_changed.clone();
            let proxy_cache = proxy_cache.clone();
            let preferences_state = preferences_state.clone();
            let project = project.clone();
            let prog_player = prog_player.clone();
            move || {
                on_project_changed();
                let prefs = preferences_state.borrow();
                if prefs.proxy_mode.is_enabled() {
                    let scale = match prefs.proxy_mode {
                        crate::ui_state::ProxyMode::QuarterRes => {
                            crate::media::proxy_cache::ProxyScale::Quarter
                        }
                        _ => crate::media::proxy_cache::ProxyScale::Half,
                    };
                    let variants = {
                        let proj = project.borrow();
                        collect_unique_proxy_variants(&proj, scale)
                    };
                    {
                        let mut cache = proxy_cache.borrow_mut();
                        cache.cleanup_stale_variants(&variants);
                        request_proxy_variants(&mut cache, &variants);
                    }
                    let paths = proxy_cache.borrow().proxies.clone();
                    prog_player.borrow_mut().update_proxy_paths(paths);
                }
            }
        },
        // on_frei0r_changed: topology change (add/remove/reorder/toggle) → full pipeline rebuild
        {
            let on_project_changed = on_project_changed.clone();
            move || {
                on_project_changed();
            }
        },
        // on_frei0r_params_changed: slider change → live pipeline update, no rebuild
        {
            let prog_player = prog_player.clone();
            let project = project.clone();
            let timeline_state = timeline_state.clone();
            // Debounce timer for paused-frame reseek after frei0r param changes.
            // Avoids blocking the GTK main loop inside a slider callback and
            // prevents crash-prone flush-seeks in some frei0r plugins (cairogradient)
            // by coalescing rapid slider changes into a single deferred reseek.
            let frei0r_reseek_timer: Rc<Cell<u32>> = Rc::new(Cell::new(0));
            move || {
                let effects = {
                    let proj = project.borrow();
                    let selected = timeline_state.borrow().selected_clip_id.clone();
                    selected.and_then(|cid| {
                        proj.tracks
                            .iter()
                            .flat_map(|t| t.clips.iter())
                            .find(|c| c.id == cid)
                            .map(|c| c.frei0r_effects.clone())
                    })
                };
                // Also update mask shared state for live slider feedback.
                {
                    let proj = project.borrow();
                    let selected = timeline_state.borrow().selected_clip_id.clone();
                    if let Some(clip_id) = selected {
                        if let Some(masks) = proj
                            .tracks
                            .iter()
                            .flat_map(|t| t.clips.iter())
                            .find(|c| c.id == clip_id)
                            .map(|c| c.masks.clone())
                        {
                            prog_player
                                .borrow_mut()
                                .update_masks_for_clip(&clip_id, &masks);
                        }
                    }
                }
                // Also update HSL qualifier shared state for live slider feedback.
                // Uses recursive `clip_ref` so clips inside compound tracks work.
                {
                    let proj = project.borrow();
                    let selected = timeline_state.borrow().selected_clip_id.clone();
                    if let Some(clip_id) = selected {
                        let qualifier = proj
                            .clip_ref(&clip_id)
                            .and_then(|c| c.hsl_qualifier.clone());
                        drop(proj);
                        prog_player
                            .borrow_mut()
                            .update_hsl_qualifier_for_clip(&clip_id, qualifier);
                    }
                }
                if let Some(effects) = effects {
                    let needs_reseek = prog_player.borrow_mut().update_frei0r_effects(&effects);
                    if needs_reseek {
                        // Schedule a debounced reseek: cancel the previous timer
                        // (via ticket) and set a new one. The 32ms delay coalesces
                        // rapid slider changes into a single flush-seek, reducing
                        // crash risk and improving responsiveness.
                        let ticket = frei0r_reseek_timer.get().wrapping_add(1);
                        frei0r_reseek_timer.set(ticket);
                        let pp = prog_player.clone();
                        let timer_check = frei0r_reseek_timer.clone();
                        glib::timeout_add_local_once(
                            std::time::Duration::from_millis(32),
                            move || {
                                if timer_check.get() != ticket {
                                    return; // superseded by a newer change
                                }
                                let p = pp.borrow();
                                if !p.is_playing() {
                                    p.reseek_paused();
                                }
                            },
                        );
                    }
                }
            }
        },
        // on_speed_keyframe_changed: lightweight update without pipeline rebuild
        {
            let prog_player = prog_player.clone();
            let timeline_panel_cell = timeline_panel_cell.clone();
            let keyframe_editor_cell = keyframe_editor_cell.clone();
            move |clip_id: &str, speed: f64, keyframes: &[crate::model::clip::NumericKeyframe]| {
                prog_player.borrow_mut().update_speed_keyframes_for_clip(
                    clip_id,
                    speed,
                    keyframes.to_vec(),
                );
                // Redraw timeline and dopesheet to reflect new duration/keyframes
                if let Some(ref w) = *timeline_panel_cell.borrow() {
                    w.queue_draw();
                }
                if let Some(ref editor) = *keyframe_editor_cell.borrow() {
                    editor.queue_redraw();
                }
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
        // on_normalize_audio: analyze clip loudness and adjust volume
        {
            // Channel-based background analysis (same pattern as silence detection).
            // Result: Ok((clip_id, old_volume, old_measured, measured_lufs, target_lufs))
            //         Err(error_message)
            type NormResult = Result<(String, f32, Option<f64>, f64, f64), String>;
            let norm_rx: Rc<RefCell<Option<std::sync::mpsc::Receiver<NormResult>>>> =
                Rc::new(RefCell::new(None));

            // Poll timer — runs every 100ms, checks for completed analysis.
            {
                let norm_rx = norm_rx.clone();
                let norm_in_progress = norm_in_progress.clone();
                let project = project.clone();
                let timeline_state = timeline_state.clone();
                let on_project_changed = on_project_changed.clone();
                let window_weak = window_weak.clone();
                glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                    let rx_opt = norm_rx.borrow();
                    if let Some(ref rx) = *rx_opt {
                        if let Ok(result) = rx.try_recv() {
                            drop(rx_opt);
                            norm_rx.borrow_mut().take();
                            norm_in_progress.set(false);
                            match result {
                                Ok((
                                    clip_id,
                                    old_volume,
                                    old_measured,
                                    measured_lufs,
                                    target_lufs,
                                )) => {
                                    let gain = crate::media::export::compute_lufs_gain(
                                        measured_lufs,
                                        target_lufs,
                                    );
                                    let new_volume =
                                        (old_volume as f64 * gain).clamp(0.0, 4.0) as f32;
                                    {
                                        let mut proj = project.borrow_mut();
                                        let cmd = crate::undo::NormalizeClipAudioCommand {
                                            clip_id: clip_id.clone(),
                                            old_volume,
                                            new_volume,
                                            old_measured_loudness: old_measured,
                                            new_measured_loudness: Some(measured_lufs),
                                        };
                                        let mut ts = timeline_state.borrow_mut();
                                        ts.history.execute(Box::new(cmd), &mut proj);
                                    }
                                    on_project_changed();
                                    if let Some(win) = window_weak.upgrade() {
                                        let proj = project.borrow();
                                        let title = format!(
                                            "UltimateSlice \u{2014} {} \u{2022}",
                                            proj.title
                                        );
                                        win.set_title(Some(&title));
                                    }
                                    log::info!(
                                        "Normalize: clip={} measured={:.1} LUFS target={:.1} gain={:.3} vol {:.3} -> {:.3}",
                                        clip_id, measured_lufs, target_lufs, gain, old_volume, new_volume
                                    );
                                }
                                Err(e) => {
                                    log::warn!("Normalize analysis failed: {e}");
                                    if let Some(win) = window_weak.upgrade() {
                                        let proj = project.borrow();
                                        let title =
                                            format!("UltimateSlice \u{2014} {}", proj.title);
                                        win.set_title(Some(&title));
                                    }
                                }
                            }
                        }
                    }
                    glib::ControlFlow::Continue
                });
            }

            let project = project.clone();
            let window_weak = window_weak.clone();
            let norm_in_progress = norm_in_progress.clone();
            move |clip_id: &str| {
                // Don't start if one is already in progress.
                if norm_in_progress.get() {
                    return;
                }
                let clip_info = {
                    let proj = project.borrow();
                    proj.clip_ref(clip_id).map(|clip| {
                        (
                            clip.source_path.clone(),
                            clip.source_in,
                            clip.source_out,
                            clip.volume,
                            clip.measured_loudness_lufs,
                        )
                    })
                };
                let Some((source_path, source_in, source_out, old_volume, old_measured)) =
                    clip_info
                else {
                    return;
                };
                let target_lufs = -14.0_f64;
                let clip_id_owned = clip_id.to_string();
                norm_in_progress.set(true);
                if let Some(win) = window_weak.upgrade() {
                    let proj = project.borrow();
                    win.set_title(Some(&format!(
                        "UltimateSlice \u{2014} {} (Analyzing loudness\u{2026})",
                        proj.title
                    )));
                }
                let (tx, rx) = std::sync::mpsc::channel();
                *norm_rx.borrow_mut() = Some(rx);
                std::thread::spawn(move || {
                    let result = crate::media::export::analyze_loudness_lufs(
                        &source_path,
                        source_in,
                        source_out,
                    );
                    let _ = tx.send(match result {
                        Ok(measured_lufs) => Ok((
                            clip_id_owned,
                            old_volume,
                            old_measured,
                            measured_lufs,
                            target_lufs,
                        )),
                        Err(e) => Err(e.to_string()),
                    });
                });
            }
        },
        // on_analyze_voice_isolation_silence: run silencedetect, store inverted
        // speech intervals on the clip, push undo command. Synchronous (mirrors
        // the Normalize/silencedetect pattern; detect_silence is fast for typical clips).
        {
            let project = project.clone();
            let timeline_state = timeline_state.clone();
            let on_project_changed = on_project_changed.clone();
            let window_weak = window_weak.clone();
            move |clip_id: &str| {
                let clip_info = {
                    let proj = project.borrow();
                    proj.clip_ref(clip_id).map(|c| {
                        (
                            c.source_path.clone(),
                            c.source_in,
                            c.source_out,
                            c.voice_isolation_silence_threshold_db,
                            c.voice_isolation_silence_min_ms,
                            c.voice_isolation_speech_intervals.clone(),
                        )
                    })
                };
                let Some((source_path, source_in, source_out, threshold_db, min_ms, old_intervals)) =
                    clip_info
                else {
                    return;
                };
                let min_duration = (min_ms as f64) / 1000.0;
                let result = crate::media::export::detect_silence(
                    &source_path,
                    source_in,
                    source_out,
                    threshold_db as f64,
                    min_duration,
                );
                match result {
                    Ok(silences) => {
                        let clip_duration_ns = source_out.saturating_sub(source_in);
                        let new_intervals = crate::media::export::invert_silences_to_speech(
                            &silences,
                            clip_duration_ns,
                        );
                        log::info!(
                            "voice_iso analyze: clip={} silences={} speech_intervals={}",
                            clip_id,
                            silences.len(),
                            new_intervals.len()
                        );
                        {
                            let mut proj = project.borrow_mut();
                            let cmd = crate::undo::AnalyzeVoiceIsolationSilenceCommand {
                                clip_id: clip_id.to_string(),
                                track_id: String::new(),
                                old_intervals,
                                new_intervals,
                            };
                            let mut ts = timeline_state.borrow_mut();
                            ts.history.execute(Box::new(cmd), &mut proj);
                        }
                        on_project_changed();
                    }
                    Err(e) => {
                        log::warn!("voice_iso analyze failed for {clip_id}: {e}");
                        if let Some(win) = window_weak.upgrade() {
                            flash_window_status_title(
                                &win,
                                &project,
                                &format!("Voice isolation analysis failed: {e}"),
                            );
                        }
                    }
                }
            }
        },
        // on_suggest_voice_isolation_threshold: run astats on the clip and
        // return a suggested silence-detect threshold (dB) based on the noise
        // floor. Returns None on failure (logged); the inspector leaves the
        // slider unchanged.
        {
            let project = project.clone();
            let window_weak = window_weak.clone();
            move |clip_id: &str| -> Option<f32> {
                let info = {
                    let proj = project.borrow();
                    proj.clip_ref(clip_id)
                        .map(|c| (c.source_path.clone(), c.source_in, c.source_out))
                };
                let (source_path, source_in, source_out) = info?;
                match crate::media::export::suggest_silence_threshold_db(
                    &source_path,
                    source_in,
                    source_out,
                ) {
                    Ok(db) => {
                        log::info!("voice_iso suggest: clip={clip_id} threshold={db:.1} dB");
                        Some(db)
                    }
                    Err(e) => {
                        log::warn!("voice_iso suggest failed for {clip_id}: {e}");
                        if let Some(win) = window_weak.upgrade() {
                            flash_window_status_title(
                                &win,
                                &project,
                                &format!("Voice isolation suggest failed: {e}"),
                            );
                        }
                        None
                    }
                }
            }
        },
        // on_match_audio: analyse source/reference audio and apply volume + EQ together
        {
            type MatchAudioResult = Result<PreparedAudioMatch, String>;
            let match_rx: Rc<RefCell<Option<std::sync::mpsc::Receiver<MatchAudioResult>>>> =
                Rc::new(RefCell::new(None));
            let match_in_progress = match_audio_in_progress.clone();

            {
                let match_rx = match_rx.clone();
                let match_in_progress = match_in_progress.clone();
                let project = project.clone();
                let timeline_state = timeline_state.clone();
                let on_project_changed = on_project_changed.clone();
                let window_weak = window_weak.clone();
                let prog_player = prog_player.clone();
                glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                    let rx_opt = match_rx.borrow();
                    if let Some(ref rx) = *rx_opt {
                        if let Ok(result) = rx.try_recv() {
                            drop(rx_opt);
                            match_rx.borrow_mut().take();
                            match_in_progress.set(false);
                            match result {
                                Ok(prepared) => {
                                    {
                                        let mut proj = project.borrow_mut();
                                        let cmd = crate::undo::MatchClipAudioCommand {
                                            clip_id: prepared.clip_id.clone(),
                                            old_volume: prepared.old_volume,
                                            new_volume: prepared.new_volume,
                                            old_measured_loudness: prepared.old_measured_loudness,
                                            new_measured_loudness: prepared.new_measured_loudness,
                                            old_eq_bands: prepared.old_eq_bands,
                                            new_eq_bands: prepared.new_eq_bands,
                                            old_match_eq_bands: prepared.old_match_eq_bands.clone(),
                                            new_match_eq_bands: prepared.new_match_eq_bands.clone(),
                                        };
                                        let mut ts = timeline_state.borrow_mut();
                                        ts.history.execute(Box::new(cmd), &mut proj);
                                    }
                                    {
                                        let mut pp = prog_player.borrow_mut();
                                        pp.update_match_eq_for_clip(
                                            &prepared.clip_id,
                                            prepared.new_match_eq_bands.clone(),
                                        );
                                    }
                                    on_project_changed();
                                    if let Some(win) = window_weak.upgrade() {
                                        flash_window_status_title(
                                            &win,
                                            &project,
                                            "Audio match applied",
                                        );
                                    }
                                    log::info!(
                                        "audio_match: clip={} gain={:.3} lufs {:.1}->{:.1} channels src={} ref={} profile src({:.1},{:.1},{:.1}) ref({:.1},{:.1},{:.1})",
                                        prepared.clip_id,
                                        prepared.volume_gain,
                                        prepared.source_loudness_lufs,
                                        prepared.reference_loudness_lufs,
                                        prepared.source_channel_mode.as_str(),
                                        prepared.reference_channel_mode.as_str(),
                                        prepared.source_profile.low_db,
                                        prepared.source_profile.mid_db,
                                        prepared.source_profile.high_db,
                                        prepared.reference_profile.low_db,
                                        prepared.reference_profile.mid_db,
                                        prepared.reference_profile.high_db
                                    );
                                }
                                Err(e) => {
                                    log::warn!("Audio match failed: {e}");
                                    if let Some(win) = window_weak.upgrade() {
                                        flash_window_status_title(
                                            &win,
                                            &project,
                                            &format!("Audio match failed: {e}"),
                                        );
                                    }
                                }
                            }
                        }
                    }
                    glib::ControlFlow::Continue
                });
            }

            let project = project.clone();
            let window_weak = window_weak.clone();
            let match_in_progress = match_in_progress.clone();
            move |source_clip_id: &str,
                  source_region: Option<crate::media::audio_match::AnalysisRegionNs>,
                  source_channel_mode: crate::media::audio_match::AudioMatchChannelMode,
                  reference_clip_id: &str,
                  reference_region: Option<crate::media::audio_match::AnalysisRegionNs>,
                  reference_channel_mode: crate::media::audio_match::AudioMatchChannelMode| {
                if match_in_progress.get() {
                    return;
                }
                let clip_info = {
                    let proj = project.borrow();
                    let source = collect_audio_match_clip_info(&proj, source_clip_id)
                        .ok_or_else(|| "Source clip not found.".to_string());
                    let reference = collect_audio_match_clip_info(&proj, reference_clip_id)
                        .ok_or_else(|| "Reference clip not found.".to_string());
                    match (source, reference) {
                        (Ok(source), Ok(reference)) => Ok((source, reference)),
                        (Err(e), _) | (_, Err(e)) => Err(e),
                    }
                };
                let (source, reference) = match clip_info {
                    Ok(pair) => pair,
                    Err(message) => {
                        if let Some(win) = window_weak.upgrade() {
                            flash_window_status_title(
                                &win,
                                &project,
                                &format!("Audio match failed: {message}"),
                            );
                        }
                        return;
                    }
                };

                match_in_progress.set(true);
                if let Some(win) = window_weak.upgrade() {
                    let proj = project.borrow();
                    win.set_title(Some(&format!(
                        "UltimateSlice — {} (Matching audio…)",
                        proj.title
                    )));
                }
                let source_clip_id = source_clip_id.to_string();
                let reference_clip_id = reference_clip_id.to_string();
                let (tx, rx) = std::sync::mpsc::channel();
                *match_rx.borrow_mut() = Some(rx);
                std::thread::spawn(move || {
                    let _ = tx.send(run_audio_match_for_clips(
                        &source_clip_id,
                        &source,
                        source_region,
                        source_channel_mode,
                        &reference_clip_id,
                        &reference,
                        reference_region,
                        reference_channel_mode,
                    ));
                });
            }
        },
        // on_duck_changed: update track duck settings from inspector
        {
            let project = project.clone();
            let on_project_changed = on_project_changed.clone();
            move |clip_id: &str, duck: bool, amount_db: f64| {
                let mut proj = project.borrow_mut();
                for track in &mut proj.tracks {
                    if track.clips.iter().any(|c| c.id == clip_id) {
                        track.duck = duck;
                        track.duck_amount_db = amount_db.clamp(-24.0, 0.0);
                        proj.dirty = true;
                        break;
                    }
                }
                drop(proj);
                on_project_changed();
            }
        },
        // on_role_changed: update track audio role from inspector
        {
            let project = project.clone();
            let on_project_changed = on_project_changed.clone();
            move |clip_id: &str, role_str: &str| {
                let mut proj = project.borrow_mut();
                for track in &mut proj.tracks {
                    if track.clips.iter().any(|c| c.id == clip_id) {
                        track.audio_role = crate::model::track::AudioRole::from_str(role_str);
                        proj.dirty = true;
                        break;
                    }
                }
                drop(proj);
                on_project_changed();
            }
        },
        // on_surround_position_changed: update track surround position override
        // from inspector. Affects only surround exports — the value is read by
        // `resolve_stem_position` in `media/export.rs` when the export channel
        // layout is 5.1 or 7.1. Stereo exports ignore the field.
        {
            let project = project.clone();
            let on_project_changed = on_project_changed.clone();
            move |clip_id: &str, position_str: &str| {
                let mut proj = project.borrow_mut();
                for track in &mut proj.tracks {
                    if track.clips.iter().any(|c| c.id == clip_id) {
                        track.surround_position =
                            crate::model::track::SurroundPositionOverride::from_str(
                                position_str,
                            );
                        proj.dirty = true;
                        break;
                    }
                }
                drop(proj);
                on_project_changed();
            }
        },
        // on_execute_command: inspector pushes undo-tracked commands through here
        {
            let timeline_state = timeline_state.clone();
            let project = project.clone();
            let on_project_changed = on_project_changed.clone();
            move |cmd: Box<dyn crate::undo::EditCommand>| {
                {
                    let mut st = timeline_state.borrow_mut();
                    let mut proj = project.borrow_mut();
                    st.history.execute(cmd, &mut proj);
                }
                on_project_changed();
            }
        },
        // on_clear_match_eq: inspector clears the 7-band match EQ on a clip
        {
            let timeline_state = timeline_state.clone();
            let project = project.clone();
            let on_project_changed = on_project_changed.clone();
            let prog_player = prog_player.clone();
            move |clip_id: &str| {
                let old_match_eq_bands = {
                    let proj = project.borrow();
                    proj.clip_ref(clip_id).map(|c| c.match_eq_bands.clone())
                };
                let Some(old_bands) = old_match_eq_bands else {
                    return;
                };
                if old_bands.is_empty() {
                    return;
                }
                {
                    let mut st = timeline_state.borrow_mut();
                    let mut proj = project.borrow_mut();
                    let cmd = crate::undo::ClearMatchEqCommand {
                        clip_id: clip_id.to_string(),
                        old_match_eq_bands: old_bands,
                    };
                    st.history.execute(Box::new(cmd), &mut proj);
                }
                {
                    let mut pp = prog_player.borrow_mut();
                    pp.update_match_eq_for_clip(clip_id, Vec::new());
                }
                on_project_changed();
            }
        },
    );

    let sync_tracking_controls: Rc<dyn Fn()> = {
        let inspector_view = inspector_view.clone();
        let project = project.clone();
        let tracking_cache = tracking_cache.clone();
        let tracking_job_key_by_clip = tracking_job_key_by_clip.clone();
        let tracking_status_by_clip = tracking_status_by_clip.clone();
        let transform_overlay_cell = transform_overlay_cell.clone();
        Rc::new(move || {
            let selected_clip_id = inspector_view.selected_clip_id.borrow().clone();
            let current_tracker_id = inspector_view.current_motion_tracker_id();
            let proj = project.borrow();

            let clip = selected_clip_id
                .as_deref()
                .and_then(|clip_id| proj.clip_ref(clip_id));
            let has_tracker = clip
                .and_then(|clip| {
                    current_tracker_id
                        .as_deref()
                        .and_then(|tracker_id| clip.motion_tracker_ref(tracker_id))
                })
                .is_some();
            let analysis_error = clip.and_then(|clip| clip_supports_tracking_analysis(clip).err());
            let can_analyze = clip.is_some() && analysis_error.is_none();

            if (!can_analyze || !has_tracker) && inspector_view.tracking_edit_region_btn.is_active()
            {
                inspector_view.tracking_edit_region_btn.set_active(false);
            }

            if let Some(ref to) = *transform_overlay_cell.borrow() {
                sync_transform_overlay_tracking_region(
                    to,
                    &proj,
                    selected_clip_id.as_deref(),
                    current_tracker_id.as_deref(),
                    inspector_view.tracking_edit_region_btn.is_active(),
                );
            }

            let status_message = if let Some(clip) = clip {
                if let Some(cache_key) = tracking_job_key_by_clip.borrow().get(&clip.id).cloned() {
                    if let Some(progress) = tracking_cache.borrow().job_progress(&cache_key) {
                        inspector_view.tracking_run_btn.set_sensitive(false);
                        inspector_view.tracking_cancel_btn.set_sensitive(true);
                        inspector_view.tracking_edit_region_btn.set_sensitive(false);
                        format!(
                            "Tracking… {}/{} samples",
                            progress.processed_samples, progress.total_samples
                        )
                    } else {
                        inspector_view.tracking_run_btn.set_sensitive(false);
                        inspector_view.tracking_cancel_btn.set_sensitive(true);
                        inspector_view.tracking_edit_region_btn.set_sensitive(false);
                        "Tracking…".to_string()
                    }
                } else if let Some((message, is_error)) =
                    tracking_status_by_clip.borrow().get(&clip.id).cloned()
                {
                    inspector_view
                        .tracking_run_btn
                        .set_sensitive(can_analyze && has_tracker);
                    inspector_view.tracking_cancel_btn.set_sensitive(false);
                    inspector_view
                        .tracking_edit_region_btn
                        .set_sensitive(can_analyze && has_tracker);
                    if is_error {
                        inspector_view.tracking_status_label.add_css_class("error");
                    } else {
                        inspector_view
                            .tracking_status_label
                            .remove_css_class("error");
                    }
                    message
                } else if let Some(tracker) = clip
                    .motion_trackers
                    .iter()
                    .find(|tracker| Some(tracker.id.as_str()) == current_tracker_id.as_deref())
                {
                    inspector_view
                        .tracking_run_btn
                        .set_sensitive(can_analyze && has_tracker);
                    inspector_view.tracking_cancel_btn.set_sensitive(false);
                    inspector_view
                        .tracking_edit_region_btn
                        .set_sensitive(can_analyze && has_tracker);
                    inspector_view
                        .tracking_status_label
                        .remove_css_class("error");
                    if tracker.samples.is_empty() {
                        analysis_error.map(str::to_string).unwrap_or_else(|| {
                            if tracker.analysis_end_ns.is_some() {
                                "Tracking data is stale. Run tracking again.".to_string()
                            } else {
                                "Choose a region and run tracking.".to_string()
                            }
                        })
                    } else {
                        format!(
                            "Tracker ready: {} samples cached on this clip.",
                            tracker.samples.len()
                        )
                    }
                } else {
                    inspector_view.tracking_run_btn.set_sensitive(false);
                    inspector_view.tracking_cancel_btn.set_sensitive(false);
                    inspector_view.tracking_edit_region_btn.set_sensitive(false);
                    inspector_view
                        .tracking_status_label
                        .remove_css_class("error");
                    analysis_error
                        .map(str::to_string)
                        .unwrap_or_else(|| "Add a tracker to start motion analysis.".to_string())
                }
            } else {
                inspector_view
                    .tracking_status_label
                    .remove_css_class("error");
                "Select a visual clip to create or attach motion tracking.".to_string()
            };

            if !tracking_status_by_clip
                .borrow()
                .get(selected_clip_id.as_deref().unwrap_or_default())
                .map(|(_, is_error)| *is_error)
                .unwrap_or(false)
                && tracking_job_key_by_clip
                    .borrow()
                    .get(selected_clip_id.as_deref().unwrap_or_default())
                    .is_none()
            {
                inspector_view
                    .tracking_status_label
                    .remove_css_class("error");
            }
            inspector_view
                .tracking_status_label
                .set_text(&status_message);
        })
    };
    let schedule_tracking_binding_refresh: Rc<dyn Fn()> = {
        let pending = Rc::new(Cell::new(false));
        let on_project_changed = on_project_changed.clone();
        let sync_tracking_controls = sync_tracking_controls.clone();
        Rc::new(move || {
            if pending.replace(true) {
                return;
            }
            let pending = pending.clone();
            let on_project_changed = on_project_changed.clone();
            let sync_tracking_controls = sync_tracking_controls.clone();
            // Rebuilding the tracking dropdown models during their own
            // selected-notify signal can leave GTK touching stale row objects.
            glib::idle_add_local_once(move || {
                pending.set(false);
                on_project_changed();
                sync_tracking_controls();
            });
        })
    };

    {
        let inspector_view = inspector_view.clone();
        let sync_tracking_controls = sync_tracking_controls.clone();
        let tracking_tracker_dropdown = inspector_view.tracking_tracker_dropdown.clone();
        tracking_tracker_dropdown.connect_selected_notify(move |dropdown| {
            if *inspector_view.updating.borrow() {
                return;
            }
            let tracker_id = inspector_view
                .tracking_tracker_ids
                .borrow()
                .get(dropdown.selected() as usize)
                .cloned()
                .flatten();
            *inspector_view.selected_motion_tracker_id.borrow_mut() = tracker_id;
            sync_tracking_controls();
        });
    }
    {
        let inspector_view = inspector_view.clone();
        let project = project.clone();
        let on_project_changed = on_project_changed.clone();
        let tracking_status_by_clip = tracking_status_by_clip.clone();
        let sync_tracking_controls = sync_tracking_controls.clone();
        let tracking_add_btn = inspector_view.tracking_add_btn.clone();
        tracking_add_btn.connect_clicked(move |_| {
            let clip_id = inspector_view.selected_clip_id.borrow().clone();
            let Some(clip_id) = clip_id else {
                return;
            };
            let new_tracker_id = {
                let mut proj = project.borrow_mut();
                let Some(clip) = proj.clip_mut(&clip_id) else {
                    return;
                };
                if clip_supports_tracking_analysis(clip).is_err() {
                    return;
                }
                let mut tracker = crate::model::clip::MotionTracker::new(format!(
                    "Tracker {}",
                    clip.motion_trackers.len() + 1
                ));
                tracker.analysis_region = crate::model::clip::TrackingRegion::default();
                let tracker_id = tracker.id.clone();
                clip.motion_trackers.push(tracker);
                proj.dirty = true;
                tracker_id
            };
            tracking_status_by_clip.borrow_mut().remove(&clip_id);
            *inspector_view.selected_motion_tracker_id.borrow_mut() = Some(new_tracker_id);
            on_project_changed();
            sync_tracking_controls();
        });
    }
    {
        let inspector_view = inspector_view.clone();
        let project = project.clone();
        let tracking_cache = tracking_cache.clone();
        let tracking_job_owner_by_key = tracking_job_owner_by_key.clone();
        let tracking_job_key_by_clip = tracking_job_key_by_clip.clone();
        let tracking_status_by_clip = tracking_status_by_clip.clone();
        let on_project_changed = on_project_changed.clone();
        let sync_tracking_controls = sync_tracking_controls.clone();
        let tracking_remove_btn = inspector_view.tracking_remove_btn.clone();
        tracking_remove_btn.connect_clicked(move |_| {
            let clip_id = inspector_view.selected_clip_id.borrow().clone();
            let tracker_id = inspector_view.current_motion_tracker_id();
            let (Some(clip_id), Some(tracker_id)) = (clip_id, tracker_id) else {
                return;
            };

            if let Some(cache_key) = tracking_job_key_by_clip.borrow_mut().remove(&clip_id) {
                tracking_job_owner_by_key.borrow_mut().remove(&cache_key);
                tracking_cache.borrow_mut().cancel(&cache_key);
            }

            let next_tracker_id = {
                let mut proj = project.borrow_mut();
                let next_tracker_id = {
                    let Some(clip) = proj.clip_mut(&clip_id) else {
                        return;
                    };
                    clip.motion_trackers
                        .retain(|tracker| tracker.id != tracker_id);
                    clip.motion_trackers
                        .first()
                        .map(|tracker| tracker.id.clone())
                };
                proj.clear_tracking_bindings_for_tracker(&clip_id, &tracker_id);
                proj.dirty = true;
                next_tracker_id
            };
            *inspector_view.selected_motion_tracker_id.borrow_mut() = next_tracker_id;
            tracking_status_by_clip
                .borrow_mut()
                .insert(clip_id.clone(), ("Tracker removed.".to_string(), false));
            on_project_changed();
            sync_tracking_controls();
        });
    }
    {
        let inspector_view = inspector_view.clone();
        let project = project.clone();
        let window_weak = window_weak.clone();
        let tracking_label_entry = inspector_view.tracking_label_entry.clone();
        tracking_label_entry.connect_changed(move |entry| {
            if *inspector_view.updating.borrow() {
                return;
            }
            let clip_id = inspector_view.selected_clip_id.borrow().clone();
            let tracker_id = inspector_view.current_motion_tracker_id();
            let (Some(clip_id), Some(tracker_id)) = (clip_id, tracker_id) else {
                return;
            };
            let mut proj = project.borrow_mut();
            if let Some(tracker) = proj
                .clip_mut(&clip_id)
                .and_then(|clip| clip.motion_tracker_mut(&tracker_id))
            {
                let trimmed = entry.text().trim().to_string();
                tracker.label = if trimmed.is_empty() {
                    "Tracker".to_string()
                } else {
                    trimmed
                };
                proj.dirty = true;
            }
            if let Some(win) = window_weak.upgrade() {
                win.set_title(Some(&format!("UltimateSlice — {} •", proj.title)));
            }
        });
    }
    {
        let sync_tracking_controls = sync_tracking_controls.clone();
        let inspector_view = inspector_view.clone();
        inspector_view
            .tracking_edit_region_btn
            .connect_toggled(move |_| sync_tracking_controls());
    }
    macro_rules! wire_tracking_region_slider {
        ($widget:expr, $field:ident) => {{
            let inspector_view = inspector_view.clone();
            let project = project.clone();
            let library = library.clone();
            let tracking_status_by_clip = tracking_status_by_clip.clone();
            let sync_tracking_controls = sync_tracking_controls.clone();
            let on_project_changed = on_project_changed.clone();
            let window_weak = window_weak.clone();
            let widget = $widget.clone();
            widget.connect_value_changed(move |widget| {
                if *inspector_view.updating.borrow() {
                    return;
                }
                let clip_id = inspector_view.selected_clip_id.borrow().clone();
                let tracker_id = inspector_view.current_motion_tracker_id();
                let (Some(clip_id), Some(tracker_id)) = (clip_id, tracker_id) else {
                    return;
                };
                let mut auto_crop_active = false;
                {
                    let mut proj = project.borrow_mut();
                    if let Some(tracker) = proj
                        .clip_mut(&clip_id)
                        .and_then(|clip| clip.motion_tracker_mut(&tracker_id))
                    {
                        tracker.analysis_region.$field = widget.value();
                        tracker.samples.clear();
                        proj.dirty = true;
                    }
                    // Peek: does this clip currently have an auto-crop
                    // binding we should keep in sync with the new region?
                    auto_crop_active = proj
                        .clip_ref(&clip_id)
                        .and_then(|c| c.tracking_binding.as_ref())
                        .map(|b| b.source_clip_id == clip_id)
                        .unwrap_or(false);
                    if let Some(win) = window_weak.upgrade() {
                        win.set_title(Some(&format!("UltimateSlice — {} •", proj.title)));
                    }
                }
                if auto_crop_active {
                    let padding = inspector_view.tracking_auto_crop_padding_slider.value();
                    reapply_auto_crop_in_place(&project, &library, &clip_id, padding);
                    tracking_status_by_clip.borrow_mut().insert(
                        clip_id.clone(),
                        (
                            "Auto-crop updated. Click Auto-Crop to Project Aspect to re-track motion."
                                .to_string(),
                            false,
                        ),
                    );
                    on_project_changed();
                } else {
                    tracking_status_by_clip.borrow_mut().insert(
                        clip_id.clone(),
                        ("Region changed. Run tracking again.".to_string(), false),
                    );
                }
                sync_tracking_controls();
            });
        }};
    }
    wire_tracking_region_slider!(inspector_view.tracking_center_x_slider, center_x);
    wire_tracking_region_slider!(inspector_view.tracking_center_y_slider, center_y);
    wire_tracking_region_slider!(inspector_view.tracking_width_slider, width);
    wire_tracking_region_slider!(inspector_view.tracking_height_slider, height);
    {
        // Crop Padding slider: when an auto-crop is already active on the
        // selected clip, drag → live re-compute the binding in place so
        // the user can dial in the headroom without re-running the full
        // button flow. If no auto-crop is active, the slider value is
        // simply stored (consumed on the next button click).
        let inspector_view = inspector_view.clone();
        let project = project.clone();
        let library = library.clone();
        let tracking_status_by_clip = tracking_status_by_clip.clone();
        let on_project_changed = on_project_changed.clone();
        let sync_tracking_controls = sync_tracking_controls.clone();
        let padding_slider = inspector_view.tracking_auto_crop_padding_slider.clone();
        padding_slider.connect_value_changed(move |slider| {
            if *inspector_view.updating.borrow() {
                return;
            }
            let clip_id = inspector_view.selected_clip_id.borrow().clone();
            let Some(clip_id) = clip_id else {
                return;
            };
            let padding = slider.value();
            if reapply_auto_crop_in_place(&project, &library, &clip_id, padding) {
                tracking_status_by_clip.borrow_mut().insert(
                    clip_id.clone(),
                    (format!("Auto-crop padding: {:.0}%", padding * 100.0), false),
                );
                on_project_changed();
            }
            sync_tracking_controls();
        });
    }
    {
        let inspector_view = inspector_view.clone();
        let project = project.clone();
        let tracking_status_by_clip = tracking_status_by_clip.clone();
        let sync_tracking_controls = sync_tracking_controls.clone();
        let window_weak = window_weak.clone();
        let tracking_rotation_spin = inspector_view.tracking_rotation_spin.clone();
        tracking_rotation_spin.connect_value_changed(move |spin| {
            if *inspector_view.updating.borrow() {
                return;
            }
            let clip_id = inspector_view.selected_clip_id.borrow().clone();
            let tracker_id = inspector_view.current_motion_tracker_id();
            let (Some(clip_id), Some(tracker_id)) = (clip_id, tracker_id) else {
                return;
            };
            let mut proj = project.borrow_mut();
            if let Some(tracker) = proj
                .clip_mut(&clip_id)
                .and_then(|clip| clip.motion_tracker_mut(&tracker_id))
            {
                tracker.analysis_region.rotation_deg = spin.value();
                tracker.samples.clear();
                proj.dirty = true;
                tracking_status_by_clip.borrow_mut().insert(
                    clip_id.clone(),
                    ("Region changed. Run tracking again.".to_string(), false),
                );
            }
            if let Some(win) = window_weak.upgrade() {
                win.set_title(Some(&format!("UltimateSlice — {} •", proj.title)));
            }
            drop(proj);
            sync_tracking_controls();
        });
    }
    {
        let inspector_view = inspector_view.clone();
        let project = project.clone();
        let schedule_tracking_binding_refresh = schedule_tracking_binding_refresh.clone();
        let tracking_target_dropdown = inspector_view.tracking_target_dropdown.clone();
        tracking_target_dropdown.connect_selected_notify(move |_| {
            if *inspector_view.updating.borrow() {
                return;
            }
            let clip_id = inspector_view.selected_clip_id.borrow().clone();
            let Some(clip_id) = clip_id else {
                return;
            };
            let reference = inspector_view.selected_tracking_reference_choice();
            {
                let mut proj = project.borrow_mut();
                if let Some(clip) = proj.clip_mut(&clip_id) {
                    if apply_tracking_binding_selection(
                        clip,
                        inspector_view.tracking_target_is_mask(),
                        reference.as_ref(),
                    ) {
                        proj.dirty = true;
                    } else {
                        return;
                    }
                }
            }
            schedule_tracking_binding_refresh();
        });
    }
    {
        let inspector_view = inspector_view.clone();
        let project = project.clone();
        let schedule_tracking_binding_refresh = schedule_tracking_binding_refresh.clone();
        let tracking_reference_dropdown = inspector_view.tracking_reference_dropdown.clone();
        tracking_reference_dropdown.connect_selected_notify(move |_| {
            if *inspector_view.updating.borrow() {
                return;
            }
            let clip_id = inspector_view.selected_clip_id.borrow().clone();
            let Some(clip_id) = clip_id else {
                return;
            };
            let reference = inspector_view.selected_tracking_reference_choice();
            {
                let mut proj = project.borrow_mut();
                if let Some(clip) = proj.clip_mut(&clip_id) {
                    if apply_tracking_binding_selection(
                        clip,
                        inspector_view.tracking_target_is_mask(),
                        reference.as_ref(),
                    ) {
                        proj.dirty = true;
                    } else {
                        return;
                    }
                }
            }
            schedule_tracking_binding_refresh();
        });
    }
    {
        let inspector_view = inspector_view.clone();
        let project = project.clone();
        let schedule_tracking_binding_refresh = schedule_tracking_binding_refresh.clone();
        let tracking_clear_binding_btn = inspector_view.tracking_clear_binding_btn.clone();
        tracking_clear_binding_btn.connect_clicked(move |_| {
            let clip_id = inspector_view.selected_clip_id.borrow().clone();
            let Some(clip_id) = clip_id else {
                return;
            };
            {
                let mut proj = project.borrow_mut();
                if let Some(clip) = proj.clip_mut(&clip_id) {
                    if apply_tracking_binding_selection(clip, false, None) {
                        proj.dirty = true;
                    } else {
                        return;
                    }
                }
            }
            schedule_tracking_binding_refresh();
        });
    }
    {
        let inspector_view = inspector_view.clone();
        let project = project.clone();
        let tracking_cache = tracking_cache.clone();
        let tracking_job_owner_by_key = tracking_job_owner_by_key.clone();
        let tracking_job_key_by_clip = tracking_job_key_by_clip.clone();
        let tracking_status_by_clip = tracking_status_by_clip.clone();
        let on_project_changed = on_project_changed.clone();
        let sync_tracking_controls = sync_tracking_controls.clone();
        let tracking_run_btn = inspector_view.tracking_run_btn.clone();
        tracking_run_btn.connect_clicked(move |_| {
            let clip_id = inspector_view.selected_clip_id.borrow().clone();
            let tracker_id = inspector_view.current_motion_tracker_id();
            let (Some(clip_id), Some(tracker_id)) = (clip_id, tracker_id) else {
                return;
            };

            let job = {
                let proj = project.borrow();
                let Some(clip) = proj.clip_ref(&clip_id) else {
                    return;
                };
                if let Err(message) = clip_supports_tracking_analysis(clip) {
                    tracking_status_by_clip
                        .borrow_mut()
                        .insert(clip_id.clone(), (message.to_string(), true));
                    sync_tracking_controls();
                    return;
                }
                let Some(tracker) = clip.motion_tracker_ref(&tracker_id) else {
                    tracking_status_by_clip.borrow_mut().insert(
                        clip_id.clone(),
                        (
                            "Select a tracker before running motion analysis.".to_string(),
                            true,
                        ),
                    );
                    sync_tracking_controls();
                    return;
                };
                let analysis_start_ns = tracker.analysis_start_ns.min(clip.source_duration() - 1);
                let mut analysis_end_ns = tracker
                    .analysis_end_ns
                    .unwrap_or_else(|| clip.source_duration())
                    .min(clip.source_duration());
                if analysis_end_ns <= analysis_start_ns {
                    analysis_end_ns = clip.source_duration();
                }
                if analysis_end_ns <= analysis_start_ns {
                    tracking_status_by_clip.borrow_mut().insert(
                        clip_id.clone(),
                        (
                            "Tracking analysis needs a non-empty source range.".to_string(),
                            true,
                        ),
                    );
                    sync_tracking_controls();
                    return;
                }
                let mut job = crate::media::tracking::TrackingJob::new(
                    tracker.id.clone(),
                    tracker.label.clone(),
                    clip.source_path.clone(),
                    clip.source_in,
                    analysis_start_ns,
                    analysis_end_ns,
                    tracker.analysis_region,
                );
                // "Every source frame" step, resolved at enqueue time
                // so the cache key is deterministic for the same
                // source + region.
                job.frame_step_ns =
                    crate::media::tracking::source_frame_step_ns(&clip.source_path);
                job
            };

            let request_and_apply = |job: &crate::media::tracking::TrackingJob| {
                let cache_key = tracking_cache.borrow_mut().request(job.clone());
                let cached_tracker = tracking_cache.borrow().get_for_job(job);
                let pending = tracking_cache.borrow().job_progress(&cache_key).is_some();
                (cache_key, cached_tracker, pending)
            };

            let (mut cache_key, mut cached_tracker, mut pending) = request_and_apply(&job);
            if cached_tracker.is_none() && !pending {
                tracking_cache.borrow_mut().invalidate(&cache_key);
                (cache_key, cached_tracker, pending) = request_and_apply(&job);
            }

            if let Some(tracker) = cached_tracker {
                let mut proj = project.borrow_mut();
                if upsert_motion_tracker_on_clip(&mut proj, &clip_id, tracker.clone()) {
                    tracking_status_by_clip.borrow_mut().insert(
                        clip_id.clone(),
                        (
                            format!("Tracking ready: {} samples loaded.", tracker.samples.len()),
                            false,
                        ),
                    );
                } else {
                    tracking_status_by_clip.borrow_mut().insert(
                        clip_id.clone(),
                        ("Tracked clip no longer exists.".to_string(), true),
                    );
                }
                drop(proj);
                on_project_changed();
            } else if pending {
                tracking_job_owner_by_key
                    .borrow_mut()
                    .insert(cache_key.clone(), clip_id.clone());
                tracking_job_key_by_clip
                    .borrow_mut()
                    .insert(clip_id.clone(), cache_key);
                tracking_status_by_clip
                    .borrow_mut()
                    .insert(clip_id.clone(), ("Tracking…".to_string(), false));
            } else {
                tracking_status_by_clip.borrow_mut().insert(
                    clip_id.clone(),
                    ("Failed to queue tracking analysis.".to_string(), true),
                );
            }
            sync_tracking_controls();
        });
    }
    {
        let inspector_view = inspector_view.clone();
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let tracking_cache = tracking_cache.clone();
        let tracking_job_owner_by_key = tracking_job_owner_by_key.clone();
        let tracking_job_key_by_clip = tracking_job_key_by_clip.clone();
        let tracking_status_by_clip = tracking_status_by_clip.clone();
        let on_project_changed = on_project_changed.clone();
        let sync_tracking_controls = sync_tracking_controls.clone();
        let tracking_auto_crop_btn = inspector_view.tracking_auto_crop_btn.clone();
        tracking_auto_crop_btn.connect_clicked(move |_| {
            let clip_id = inspector_view.selected_clip_id.borrow().clone();
            let tracker_id = inspector_view.current_motion_tracker_id();
            let (Some(clip_id), Some(tracker_id)) = (clip_id, tracker_id) else {
                return;
            };
            let padding = inspector_view.tracking_auto_crop_padding_slider.value();
            let (outcome, command) = run_auto_crop_track_for_clip(
                &project,
                &tracking_cache,
                &tracking_job_owner_by_key,
                &tracking_job_key_by_clip,
                &clip_id,
                &tracker_id,
                padding,
            );
            if let Some(cmd) = command {
                let mut st = timeline_state.borrow_mut();
                let mut proj = project.borrow_mut();
                st.history.execute(cmd, &mut proj);
            }
            match outcome {
                AutoCropOutcome::Ok { ref message, .. }
                | AutoCropOutcome::Queued { ref message } => {
                    tracking_status_by_clip
                        .borrow_mut()
                        .insert(clip_id.clone(), (message.clone(), false));
                }
                AutoCropOutcome::Err { ref message } => {
                    tracking_status_by_clip
                        .borrow_mut()
                        .insert(clip_id.clone(), (message.clone(), true));
                }
            }
            on_project_changed();
            sync_tracking_controls();
        });
    }
    {
        let inspector_view = inspector_view.clone();
        let tracking_cache = tracking_cache.clone();
        let tracking_job_owner_by_key = tracking_job_owner_by_key.clone();
        let tracking_job_key_by_clip = tracking_job_key_by_clip.clone();
        let tracking_status_by_clip = tracking_status_by_clip.clone();
        let sync_tracking_controls = sync_tracking_controls.clone();
        let tracking_cancel_btn = inspector_view.tracking_cancel_btn.clone();
        tracking_cancel_btn.connect_clicked(move |_| {
            let clip_id = inspector_view.selected_clip_id.borrow().clone();
            let Some(clip_id) = clip_id else {
                return;
            };
            if let Some(cache_key) = tracking_job_key_by_clip.borrow_mut().remove(&clip_id) {
                tracking_job_owner_by_key.borrow_mut().remove(&cache_key);
                tracking_cache.borrow_mut().cancel(&cache_key);
                tracking_status_by_clip
                    .borrow_mut()
                    .insert(clip_id.clone(), ("Tracking canceled.".to_string(), false));
            }
            sync_tracking_controls();
        });
    }

    // Sync normalize button state with in-progress flag.
    {
        let btn = inspector_view.normalize_btn.clone();
        let label = inspector_view.measured_loudness_label.clone();
        let in_progress = norm_in_progress.clone();
        let mut was_in_progress = false;
        glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
            let now = in_progress.get();
            if now != was_in_progress {
                was_in_progress = now;
                if now {
                    btn.set_sensitive(false);
                    btn.set_label("Analyzing\u{2026}");
                    label.set_text("Measuring loudness\u{2026}");
                } else {
                    btn.set_sensitive(true);
                    btn.set_label("Normalize\u{2026}");
                    // The measured loudness label will be updated by on_project_changed
                    // via the inspector's update() method.
                }
            }
            glib::ControlFlow::Continue
        });
    }
    // Sync Match Audio button state with in-progress flag.
    {
        let btn = inspector_view.match_audio_btn.clone();
        let in_progress = match_audio_in_progress.clone();
        let mut was_in_progress = false;
        glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
            let now = in_progress.get();
            if now != was_in_progress {
                was_in_progress = now;
                if now {
                    btn.set_sensitive(false);
                    btn.set_label("Matching\u{2026}");
                } else {
                    btn.set_sensitive(true);
                    btn.set_label("Match Audio\u{2026}");
                }
            }
            glib::ControlFlow::Continue
        });
    }

    // Set initial model availability on the inspector so the bg-removal
    // section is hidden when no ONNX model is present.
    inspector_view
        .bg_removal_model_available
        .set(bg_removal_cache.borrow().is_available());

    // Wire inspector "Relink…" button to the shared relink callback.
    {
        let cb = on_relink_media_gui.clone();
        inspector_view.relink_btn.connect_clicked(move |_| cb());
    }

    // Wire inspector "Retry" button (under the Voice Enhance status row)
    // to clear the failed marker for the currently-selected clip's
    // (source, strength) cache key and trigger a project-changed cycle.
    // The next on_project_changed reload walks the clip list and
    // re-requests the prerender; the cache will see the failed entry is
    // gone and re-queue the ffmpeg job.
    {
        let project = project.clone();
        let inspector_view_retry = inspector_view.clone();
        let voice_enhance_cache_retry = voice_enhance_cache.clone();
        let on_project_changed_retry = on_project_changed.clone();
        inspector_view
            .voice_enhance_retry_btn
            .connect_clicked(move |_| {
                let selected = inspector_view_retry.selected_clip_id.borrow().clone();
                if let Some(clip_id) = selected {
                    let snapshot = {
                        let proj = project.borrow();
                        proj.clip_ref(&clip_id)
                            .map(|c| (c.source_path.clone(), c.voice_enhance_strength))
                    };
                    if let Some((src, strength)) = snapshot {
                        let cleared = voice_enhance_cache_retry.borrow_mut().retry(&src, strength);
                        if cleared {
                            log::info!(
                                "voice_enhance: retry requested for clip={} src={} strength={:.2}",
                                clip_id,
                                src,
                                strength
                            );
                            on_project_changed_retry();
                        }
                    }
                }
            });
    }

    // Wire inspector "Generate Subtitles" button to STT cache.
    {
        let stt_cache = stt_cache.clone();
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let lang_dropdown = inspector_view.subtitle_language_dropdown.clone();
        let inspector_view_gen = inspector_view.clone();
        inspector_view
            .subtitle_generate_btn
            .connect_clicked(move |_btn| {
                let selected = timeline_state.borrow().selected_clip_id.clone();
                if let Some(ref clip_id) = selected {
                    let proj = project.borrow();
                    let languages = [
                        "auto", "en", "es", "fr", "de", "it", "pt", "ja", "zh", "ko", "ru", "ar",
                        "hi",
                    ];
                    let lang_idx = lang_dropdown.selected() as usize;
                    let language = languages.get(lang_idx).unwrap_or(&"auto");
                    if let Some(clip) = proj.clip_ref(clip_id) {
                        stt_cache.borrow_mut().request(
                            &clip.source_path,
                            clip.source_in,
                            clip.source_out,
                            language,
                        );
                        inspector_view_gen.stt_generating.set(true);
                    }
                }
            });
    }

    // Wire inspector "Clear Subtitles" button.
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        inspector_view
            .subtitle_clear_btn
            .connect_clicked(move |_btn| {
                let selected = timeline_state.borrow().selected_clip_id.clone();
                if let Some(ref clip_id) = selected {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.subtitle_segments.clear();
                    }
                    proj.dirty = true;
                    drop(proj);
                    on_project_changed();
                }
            });
    }

    // Wire subtitle font button — opens GTK FontDialog.
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let font_btn = inspector_view.subtitle_font_btn.clone();
        inspector_view
            .subtitle_font_btn
            .connect_clicked(move |btn| {
                let dialog = gtk4::FontDialog::new();
                let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
                let project_c = project.clone();
                let ts_c = timeline_state.clone();
                let opc = on_project_changed.clone();
                let font_btn_c = font_btn.clone();
                dialog.choose_font(
                    window.as_ref(),
                    None::<&pango::FontDescription>,
                    None::<&gio::Cancellable>,
                    move |result| {
                        if let Ok(font_desc) = result {
                            let desc_str = font_desc.to_string();
                            let normalized =
                                crate::media::title_font::normalize_subtitle_font_label(&desc_str);
                            let tooltip = crate::media::title_font::build_subtitle_font_tooltip(
                                &desc_str,
                                "Click to choose a subtitle font",
                            );
                            font_btn_c.set_label(&normalized);
                            font_btn_c.set_tooltip_text(Some(&tooltip));
                            let selected = ts_c.borrow().selected_clip_id.clone();
                            if let Some(ref clip_id) = selected {
                                let mut proj = project_c.borrow_mut();
                                if let Some(clip) = proj.clip_mut(clip_id) {
                                    clip.subtitle_font = desc_str.clone();
                                }
                                proj.dirty = true;
                                drop(proj);
                                opc();
                            }
                        }
                    },
                );
            });
    }

    // Wire subtitle color button.
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let updating = inspector_view.updating.clone();
        inspector_view
            .subtitle_color_btn
            .connect_notify_local(Some("rgba"), move |btn, _| {
                if *updating.borrow() {
                    return;
                }
                let rgba = btn.rgba();
                let r = (rgba.red() * 255.0) as u32;
                let g = (rgba.green() * 255.0) as u32;
                let b = (rgba.blue() * 255.0) as u32;
                let a = (rgba.alpha() * 255.0) as u32;
                let color = (r << 24) | (g << 16) | (b << 8) | a;
                let selected = timeline_state.borrow().selected_clip_id.clone();
                if let Some(ref clip_id) = selected {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.subtitle_color = color;
                    }
                    proj.dirty = true;
                    drop(proj);
                    on_project_changed();
                }
            });
    }

    // Wire subtitle highlight mode dropdown.
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let hl_color_row = inspector_view.subtitle_highlight_color_row.clone();
        let updating = inspector_view.updating.clone();
        inspector_view
            .subtitle_highlight_dropdown
            .connect_notify_local(Some("selected"), move |dd, _| {
                if *updating.borrow() {
                    return;
                }
                let idx = dd.selected();
                let mode = match idx {
                    1 => crate::model::clip::SubtitleHighlightMode::Bold,
                    2 => crate::model::clip::SubtitleHighlightMode::Color,
                    3 => crate::model::clip::SubtitleHighlightMode::Underline,
                    4 => crate::model::clip::SubtitleHighlightMode::Stroke,
                    _ => crate::model::clip::SubtitleHighlightMode::None,
                };
                hl_color_row.set_visible(idx == 2 || idx == 4);
                let selected = timeline_state.borrow().selected_clip_id.clone();
                if let Some(ref clip_id) = selected {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.subtitle_highlight_mode = mode;
                    }
                    proj.dirty = true;
                    drop(proj);
                    on_project_changed();
                }
            });
    }

    // Wire subtitle highlight color button.
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let updating = inspector_view.updating.clone();
        inspector_view
            .subtitle_highlight_color_btn
            .connect_notify_local(Some("rgba"), move |btn, _| {
                if *updating.borrow() {
                    return;
                }
                let rgba = btn.rgba();
                let r = (rgba.red() * 255.0) as u32;
                let g = (rgba.green() * 255.0) as u32;
                let b = (rgba.blue() * 255.0) as u32;
                let a = (rgba.alpha() * 255.0) as u32;
                let color = (r << 24) | (g << 16) | (b << 8) | a;
                let selected = timeline_state.borrow().selected_clip_id.clone();
                if let Some(ref clip_id) = selected {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.subtitle_highlight_color = color;
                    }
                    proj.dirty = true;
                    drop(proj);
                    on_project_changed();
                }
            });
    }

    // Wire subtitle highlight stroke color button (independent from the
    // text-fill highlight colour so users can pick e.g. yellow text + black
    // stroke).
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let updating = inspector_view.updating.clone();
        inspector_view
            .subtitle_highlight_stroke_color_btn
            .connect_notify_local(Some("rgba"), move |btn, _| {
                if *updating.borrow() {
                    return;
                }
                let rgba = btn.rgba();
                let r = (rgba.red() * 255.0) as u32;
                let g = (rgba.green() * 255.0) as u32;
                let b = (rgba.blue() * 255.0) as u32;
                let a = (rgba.alpha() * 255.0) as u32;
                let color = (r << 24) | (g << 16) | (b << 8) | a;
                let selected = timeline_state.borrow().selected_clip_id.clone();
                if let Some(ref clip_id) = selected {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.subtitle_highlight_stroke_color = color;
                    }
                    proj.dirty = true;
                    drop(proj);
                    on_project_changed();
                }
            });
    }

    // Wire subtitle base style toggle buttons.
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let updating = inspector_view.updating.clone();
        inspector_view.sub_bold_btn.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            let active = btn.is_active();
            let selected = timeline_state.borrow().selected_clip_id.clone();
            if let Some(ref clip_id) = selected {
                let mut proj = project.borrow_mut();
                if let Some(clip) = proj.clip_mut(clip_id) {
                    clip.subtitle_bold = active;
                }
                proj.dirty = true;
                drop(proj);
                on_project_changed();
            }
        });
    }
    // Wire "Render subtitles" visibility toggle. Hides this clip's
    // subtitles from the preview overlay, export burn-in, and SRT
    // sidecar without removing the segment data, so the transcript
    // editor and voice isolation continue to work.
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let updating = inspector_view.updating.clone();
        inspector_view
            .sub_visible_check
            .connect_toggled(move |btn| {
                if *updating.borrow() {
                    return;
                }
                let active = btn.is_active();
                let selected = timeline_state.borrow().selected_clip_id.clone();
                if let Some(ref clip_id) = selected {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.subtitle_visible = active;
                    }
                    proj.dirty = true;
                    drop(proj);
                    on_project_changed();
                }
            });
    }
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let updating = inspector_view.updating.clone();
        inspector_view.sub_italic_btn.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            let active = btn.is_active();
            let selected = timeline_state.borrow().selected_clip_id.clone();
            if let Some(ref clip_id) = selected {
                let mut proj = project.borrow_mut();
                if let Some(clip) = proj.clip_mut(clip_id) {
                    clip.subtitle_italic = active;
                }
                proj.dirty = true;
                drop(proj);
                on_project_changed();
            }
        });
    }
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let updating = inspector_view.updating.clone();
        inspector_view
            .sub_underline_btn
            .connect_toggled(move |btn| {
                if *updating.borrow() {
                    return;
                }
                let active = btn.is_active();
                let selected = timeline_state.borrow().selected_clip_id.clone();
                if let Some(ref clip_id) = selected {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.subtitle_underline = active;
                    }
                    proj.dirty = true;
                    drop(proj);
                    on_project_changed();
                }
            });
    }
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let updating = inspector_view.updating.clone();
        inspector_view.sub_shadow_btn.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            let active = btn.is_active();
            let selected = timeline_state.borrow().selected_clip_id.clone();
            if let Some(ref clip_id) = selected {
                let mut proj = project.borrow_mut();
                if let Some(clip) = proj.clip_mut(clip_id) {
                    clip.subtitle_shadow = active;
                }
                proj.dirty = true;
                drop(proj);
                on_project_changed();
            }
        });
    }

    // Wire subtitle highlight flag checkboxes.
    // Helper macro-style: each checkbox sets one flag in subtitle_highlight_flags.
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let updating = inspector_view.updating.clone();
        let hl_color_row = inspector_view.subtitle_highlight_color_row.clone();
        let bg_hl_color_row = inspector_view.subtitle_bg_highlight_color_row.clone();
        inspector_view.hl_bold_check.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            let active = btn.is_active();
            let selected = timeline_state.borrow().selected_clip_id.clone();
            if let Some(ref clip_id) = selected {
                let mut proj = project.borrow_mut();
                if let Some(clip) = proj.clip_mut(clip_id) {
                    clip.subtitle_highlight_flags.bold = active;
                }
                let _ = &hl_color_row;
                let _ = &bg_hl_color_row;
                proj.dirty = true;
                drop(proj);
                on_project_changed();
            }
        });
    }
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let updating = inspector_view.updating.clone();
        inspector_view.hl_color_check.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            let active = btn.is_active();
            let selected = timeline_state.borrow().selected_clip_id.clone();
            if let Some(ref clip_id) = selected {
                let mut proj = project.borrow_mut();
                if let Some(clip) = proj.clip_mut(clip_id) {
                    clip.subtitle_highlight_flags.color = active;
                }
                proj.dirty = true;
                drop(proj);
                on_project_changed();
            }
        });
    }
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let updating = inspector_view.updating.clone();
        inspector_view
            .hl_underline_check
            .connect_toggled(move |btn| {
                if *updating.borrow() {
                    return;
                }
                let active = btn.is_active();
                let selected = timeline_state.borrow().selected_clip_id.clone();
                if let Some(ref clip_id) = selected {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.subtitle_highlight_flags.underline = active;
                    }
                    proj.dirty = true;
                    drop(proj);
                    on_project_changed();
                }
            });
    }
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let updating = inspector_view.updating.clone();
        inspector_view.hl_stroke_check.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            let active = btn.is_active();
            let selected = timeline_state.borrow().selected_clip_id.clone();
            if let Some(ref clip_id) = selected {
                let mut proj = project.borrow_mut();
                if let Some(clip) = proj.clip_mut(clip_id) {
                    clip.subtitle_highlight_flags.stroke = active;
                }
                proj.dirty = true;
                drop(proj);
                on_project_changed();
            }
        });
    }
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let updating = inspector_view.updating.clone();
        inspector_view.hl_italic_check.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            let active = btn.is_active();
            let selected = timeline_state.borrow().selected_clip_id.clone();
            if let Some(ref clip_id) = selected {
                let mut proj = project.borrow_mut();
                if let Some(clip) = proj.clip_mut(clip_id) {
                    clip.subtitle_highlight_flags.italic = active;
                }
                proj.dirty = true;
                drop(proj);
                on_project_changed();
            }
        });
    }
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let updating = inspector_view.updating.clone();
        inspector_view.hl_bg_check.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            let active = btn.is_active();
            let selected = timeline_state.borrow().selected_clip_id.clone();
            if let Some(ref clip_id) = selected {
                let mut proj = project.borrow_mut();
                if let Some(clip) = proj.clip_mut(clip_id) {
                    clip.subtitle_highlight_flags.background = active;
                }
                proj.dirty = true;
                drop(proj);
                on_project_changed();
            }
        });
    }
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let updating = inspector_view.updating.clone();
        inspector_view.hl_shadow_check.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            let active = btn.is_active();
            let selected = timeline_state.borrow().selected_clip_id.clone();
            if let Some(ref clip_id) = selected {
                let mut proj = project.borrow_mut();
                if let Some(clip) = proj.clip_mut(clip_id) {
                    clip.subtitle_highlight_flags.shadow = active;
                }
                proj.dirty = true;
                drop(proj);
                on_project_changed();
            }
        });
    }

    // Wire subtitle background highlight color button.
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let updating = inspector_view.updating.clone();
        inspector_view
            .subtitle_bg_highlight_color_btn
            .connect_notify_local(Some("rgba"), move |btn, _| {
                if *updating.borrow() {
                    return;
                }
                let rgba = btn.rgba();
                let r = (rgba.red() * 255.0) as u32;
                let g = (rgba.green() * 255.0) as u32;
                let b = (rgba.blue() * 255.0) as u32;
                let a = (rgba.alpha() * 255.0) as u32;
                let color = (r << 24) | (g << 16) | (b << 8) | a;
                let selected = timeline_state.borrow().selected_clip_id.clone();
                if let Some(ref clip_id) = selected {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.subtitle_bg_highlight_color = color;
                    }
                    proj.dirty = true;
                    drop(proj);
                    on_project_changed();
                }
            });
    }

    // Wire subtitle word window slider.
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let updating = inspector_view.updating.clone();
        inspector_view
            .subtitle_word_window_slider
            .connect_value_changed(move |s| {
                if *updating.borrow() {
                    return;
                }
                let val = s.value();
                let selected = timeline_state.borrow().selected_clip_id.clone();
                if let Some(ref clip_id) = selected {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.subtitle_word_window_secs = val;
                    }
                    proj.dirty = true;
                }
            });
    }

    // Wire subtitle position slider.
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let updating = inspector_view.updating.clone();
        inspector_view
            .subtitle_position_slider
            .connect_value_changed(move |s| {
                if *updating.borrow() {
                    return;
                }
                let val = s.value();
                let selected = timeline_state.borrow().selected_clip_id.clone();
                if let Some(ref clip_id) = selected {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.subtitle_position_y = val;
                    }
                    proj.dirty = true;
                }
            });
    }

    // Wire subtitle background box toggle.
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let updating = inspector_view.updating.clone();
        inspector_view
            .subtitle_bg_box_check
            .connect_toggled(move |btn| {
                if *updating.borrow() {
                    return;
                }
                let enabled = btn.is_active();
                let selected = timeline_state.borrow().selected_clip_id.clone();
                if let Some(ref clip_id) = selected {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.subtitle_bg_box = enabled;
                    }
                    proj.dirty = true;
                }
            });
    }

    // Wire subtitle outline color button.
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let updating = inspector_view.updating.clone();
        inspector_view
            .subtitle_outline_color_btn
            .connect_notify_local(Some("rgba"), move |btn, _| {
                if *updating.borrow() {
                    return;
                }
                let rgba = btn.rgba();
                let r = (rgba.red() * 255.0) as u32;
                let g = (rgba.green() * 255.0) as u32;
                let b = (rgba.blue() * 255.0) as u32;
                let a = (rgba.alpha() * 255.0) as u32;
                let color = (r << 24) | (g << 16) | (b << 8) | a;
                let selected = timeline_state.borrow().selected_clip_id.clone();
                if let Some(ref clip_id) = selected {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.subtitle_outline_color = color;
                    }
                    proj.dirty = true;
                }
            });
    }

    // Wire subtitle background color button.
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let updating = inspector_view.updating.clone();
        inspector_view
            .subtitle_bg_color_btn
            .connect_notify_local(Some("rgba"), move |btn, _| {
                if *updating.borrow() {
                    return;
                }
                let rgba = btn.rgba();
                let r = (rgba.red() * 255.0) as u32;
                let g = (rgba.green() * 255.0) as u32;
                let b = (rgba.blue() * 255.0) as u32;
                let a = (rgba.alpha() * 255.0) as u32;
                let color = (r << 24) | (g << 16) | (b << 8) | a;
                let selected = timeline_state.borrow().selected_clip_id.clone();
                if let Some(ref clip_id) = selected {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.subtitle_bg_box_color = color;
                    }
                    proj.dirty = true;
                }
            });
    }

    // Wire Export SRT button.
    {
        let project = project.clone();
        let window_weak = window.downgrade();
        inspector_view
            .subtitle_export_srt_btn
            .connect_clicked(move |_btn| {
                let Some(win) = window_weak.upgrade() else {
                    return;
                };
                let dialog = gtk4::FileDialog::new();
                dialog.set_title("Export Subtitles as SRT");
                let filter = gtk4::FileFilter::new();
                filter.add_pattern("*.srt");
                filter.set_name(Some("SRT Subtitle Files"));
                let filters = gio::ListStore::new::<gtk4::FileFilter>();
                filters.append(&filter);
                dialog.set_filters(Some(&filters));
                dialog.set_initial_name(Some("subtitles.srt"));
                let project_c = project.clone();
                dialog.save(Some(&win), None::<&gio::Cancellable>, move |result| {
                    if let Ok(file) = result {
                        if let Some(path) = file.path() {
                            let proj = project_c.borrow();
                            if let Err(e) =
                                crate::media::export::export_srt(&proj, &path.to_string_lossy())
                            {
                                log::error!("SRT export failed: {e}");
                            }
                        }
                    }
                });
            });
    }

    // Wire Import SRT button.
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let inspector_view_imp = inspector_view.clone();
        let window_weak = window.downgrade();
        inspector_view
            .subtitle_import_srt_btn
            .connect_clicked(move |_btn| {
                let Some(win) = window_weak.upgrade() else {
                    return;
                };
                let dialog = gtk4::FileDialog::new();
                dialog.set_title("Import SRT Subtitles");
                let filter = gtk4::FileFilter::new();
                filter.add_pattern("*.srt");
                filter.set_name(Some("SRT Subtitle Files"));
                let filters = gio::ListStore::new::<gtk4::FileFilter>();
                filters.append(&filter);
                dialog.set_filters(Some(&filters));
                let project_c = project.clone();
                let ts_c = timeline_state.clone();
                let opc = on_project_changed.clone();
                let iv = inspector_view_imp.clone();
                dialog.open(Some(&win), None::<&gio::Cancellable>, move |result| {
                    if let Ok(file) = result {
                        if let Some(path) = file.path() {
                            let selected = ts_c.borrow().selected_clip_id.clone();
                            if let Some(ref clip_id) = selected {
                                // Get clip's source_in for timestamp offset.
                                let source_in = {
                                    let proj = project_c.borrow();
                                    proj.tracks
                                        .iter()
                                        .flat_map(|t| t.clips.iter())
                                        .find(|c| &c.id == clip_id)
                                        .map(|c| c.source_in)
                                        .unwrap_or(0)
                                };
                                match crate::media::export::import_srt(
                                    &path.to_string_lossy(),
                                    source_in,
                                ) {
                                    Ok(segments) if !segments.is_empty() => {
                                        // Find track_id and push undo command.
                                        let (track_id, old_segments) = {
                                            let proj = project_c.borrow();
                                            proj.tracks
                                                .iter()
                                                .find(|t| t.clips.iter().any(|c| &c.id == clip_id))
                                                .map(|t| {
                                                    let old = t
                                                        .clips
                                                        .iter()
                                                        .find(|c| &c.id == clip_id)
                                                        .map(|c| c.subtitle_segments.clone())
                                                        .unwrap_or_default();
                                                    (t.id.clone(), old)
                                                })
                                                .unwrap_or_default()
                                        };
                                        let cmd = crate::undo::GenerateSubtitlesCommand {
                                            clip_id: clip_id.clone(),
                                            track_id,
                                            old_segments,
                                            new_segments: segments,
                                        };
                                        use crate::undo::EditCommand;
                                        cmd.execute(&mut project_c.borrow_mut());
                                        ts_c.borrow_mut().history.undo_stack.push(Box::new(cmd));
                                        ts_c.borrow_mut().history.redo_stack.clear();
                                        iv.subtitle_segments_snapshot.borrow_mut().clear();
                                        opc();
                                    }
                                    Ok(_) => {
                                        log::warn!("SRT import: no segments found in file");
                                    }
                                    Err(e) => {
                                        log::error!("SRT import failed: {e}");
                                    }
                                }
                            }
                        }
                    }
                });
            });
    }

    // Wire subtitle Copy Style button.
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let clipboard = inspector_view.subtitle_style_clipboard.clone();
        let paste_btn = inspector_view.subtitle_paste_style_btn.clone();
        inspector_view
            .subtitle_copy_style_btn
            .connect_clicked(move |_| {
                let selected = timeline_state.borrow().selected_clip_id.clone();
                if let Some(ref clip_id) = selected {
                    let proj = project.borrow();
                    if let Some(clip) = proj.clip_ref(clip_id) {
                        *clipboard.borrow_mut() =
                            Some(crate::ui::inspector::SubtitleStyleClipboard {
                                font: clip.subtitle_font.clone(),
                                color: clip.subtitle_color,
                                outline_color: clip.subtitle_outline_color,
                                outline_width: clip.subtitle_outline_width,
                                bg_box: clip.subtitle_bg_box,
                                bg_box_color: clip.subtitle_bg_box_color,
                                highlight_mode: clip.subtitle_highlight_mode,
                                highlight_color: clip.subtitle_highlight_color,
                                position_y: clip.subtitle_position_y,
                                word_window_secs: clip.subtitle_word_window_secs,
                                subtitle_bold: clip.subtitle_bold,
                                subtitle_italic: clip.subtitle_italic,
                                subtitle_underline: clip.subtitle_underline,
                                subtitle_shadow: clip.subtitle_shadow,
                                subtitle_shadow_color: clip.subtitle_shadow_color,
                                subtitle_shadow_offset_x: clip.subtitle_shadow_offset_x,
                                subtitle_shadow_offset_y: clip.subtitle_shadow_offset_y,
                                highlight_flags: clip.subtitle_highlight_flags,
                                bg_highlight_color: clip.subtitle_bg_highlight_color,
                                highlight_stroke_color: clip.subtitle_highlight_stroke_color,
                            });
                        paste_btn.set_sensitive(true);
                    }
                }
            });
    }

    // Wire subtitle Paste Style button.
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let clipboard = inspector_view.subtitle_style_clipboard.clone();
        inspector_view
            .subtitle_paste_style_btn
            .connect_clicked(move |_| {
                let style = clipboard.borrow().clone();
                let selected = timeline_state.borrow().selected_clip_id.clone();
                if let (Some(style), Some(ref clip_id)) = (style, selected) {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.subtitle_font = style.font;
                        clip.subtitle_color = style.color;
                        clip.subtitle_outline_color = style.outline_color;
                        clip.subtitle_outline_width = style.outline_width;
                        clip.subtitle_bg_box = style.bg_box;
                        clip.subtitle_bg_box_color = style.bg_box_color;
                        clip.subtitle_highlight_mode = style.highlight_mode;
                        clip.subtitle_highlight_color = style.highlight_color;
                        clip.subtitle_position_y = style.position_y;
                        clip.subtitle_word_window_secs = style.word_window_secs;
                        clip.subtitle_bold = style.subtitle_bold;
                        clip.subtitle_italic = style.subtitle_italic;
                        clip.subtitle_underline = style.subtitle_underline;
                        clip.subtitle_shadow = style.subtitle_shadow;
                        clip.subtitle_shadow_color = style.subtitle_shadow_color;
                        clip.subtitle_shadow_offset_x = style.subtitle_shadow_offset_x;
                        clip.subtitle_shadow_offset_y = style.subtitle_shadow_offset_y;
                        clip.subtitle_highlight_flags = style.highlight_flags;
                        clip.subtitle_bg_highlight_color = style.bg_highlight_color;
                        clip.subtitle_highlight_stroke_color = style.highlight_stroke_color;
                    }
                    proj.dirty = true;
                    drop(proj);
                    on_project_changed();
                }
            });
    }

    // Wire timeline's on_project_changed + on_seek + on_play_pause
    {
        let cb = on_project_changed.clone();
        timeline_state.borrow_mut().on_project_changed = Some(Rc::new(move || cb()));
    }
    // Multicam angle viewer panel widgets (created early so closures can capture them;
    // appended to the sidebar layout later in the function).
    let multicam_panel = gtk::Box::new(Orientation::Vertical, 4);
    multicam_panel.set_margin_start(6);
    multicam_panel.set_margin_end(6);
    multicam_panel.set_margin_top(4);
    multicam_panel.set_margin_bottom(4);
    multicam_panel.set_visible(false);
    {
        let header = gtk::Label::new(Some("Multicam Angles"));
        header.add_css_class("heading");
        header.set_halign(gtk::Align::Start);
        multicam_panel.append(&header);
        let hint = gtk::Label::new(Some("Press 1-9 to switch angles"));
        hint.set_halign(gtk::Align::Start);
        hint.add_css_class("dim-label");
        multicam_panel.append(&hint);
    }
    let multicam_angles_box = gtk::Box::new(Orientation::Vertical, 2);
    multicam_panel.append(&multicam_angles_box);

    // Wire on_clip_selected: lightweight inspector sync without pipeline rebuild.
    {
        let inspector_view = inspector_view.clone();
        let project = project.clone();
        let prog_player_for_sel = prog_player.clone();
        let transform_overlay_cell = transform_overlay_cell.clone();
        let keyframe_editor_cell = keyframe_editor_cell.clone();
        let timeline_state_for_sel = timeline_state.clone();
        let multicam_panel_for_sel = multicam_panel.clone();
        let multicam_angles_box_for_sel = multicam_angles_box.clone();
        let timeline_state_for_multicam_btn = timeline_state.clone();
        let on_project_changed_for_multicam = on_project_changed.clone();
        let sync_tracking_controls = sync_tracking_controls.clone();
        timeline_state.borrow_mut().on_clip_selected =
            Some(Rc::new(move |clip_id: Option<String>| {
                let proj = project.borrow();
                let (playhead_ns, missing_paths) = {
                    let st = timeline_state_for_sel.borrow();
                    (st.playhead_ns, st.missing_media_paths.clone())
                };
                inspector_view.update(&proj, clip_id.as_deref(), playhead_ns, Some(&missing_paths));
                sync_tracking_controls();
                inspector_view.update_keyframe_indicator(&proj, playhead_ns);
                // Sync transform overlay handles with selection state,
                // using keyframe-interpolated values at the current playhead.
                // Also refresh the content-inset so a freshly selected still
                // image gets its own preview framing immediately, instead of
                // inheriting whatever inset the previous selection had until
                // the next 100 ms playhead poll lands.
                if let Some(ref to) = *transform_overlay_cell.borrow() {
                    let pp = prog_player_for_sel.borrow();
                    sync_transform_overlay_to_playhead_resolved(
                        to,
                        &proj,
                        &pp,
                        clip_id.as_deref(),
                        playhead_ns,
                    );
                    let (ix, iy) = pp.content_inset_for_clip(clip_id.as_deref());
                    to.set_content_inset(ix, iy);
                }
                // When the user selects a static image while paused, force a
                // paused frame refresh so the Program Monitor actually shows
                // the still instead of leaving whatever frame was previously
                // on screen. (For video clips the existing playhead-driven
                // refresh path already covers this.)
                if selected_clip_is_static_image(&proj, clip_id.as_deref()) {
                    let pp = prog_player_for_sel.borrow();
                    if !matches!(pp.state(), crate::media::player::PlayerState::Playing) {
                        pp.reseek_paused();
                    }
                }
                if let Some(ref editor) = *keyframe_editor_cell.borrow() {
                    editor.clear_selection();
                    editor.queue_redraw();
                }
                // Update multicam angle panel visibility and contents
                let is_multicam = clip_id
                    .as_deref()
                    .and_then(|id| proj.clip_ref(id))
                    .map(|c| c.is_multicam())
                    .unwrap_or(false);
                multicam_panel_for_sel.set_visible(is_multicam);
                if is_multicam {
                    // Clear old buttons
                    while let Some(child) = multicam_angles_box_for_sel.first_child() {
                        multicam_angles_box_for_sel.remove(&child);
                    }
                    if let Some(clip) = clip_id.as_deref().and_then(|id| proj.clip_ref(id)) {
                        let active =
                            clip.active_angle_at(playhead_ns.saturating_sub(clip.timeline_start));
                        if let Some(ref angles) = clip.multicam_angles {
                            for (i, angle) in angles.iter().enumerate() {
                                let row = gtk::Box::new(Orientation::Horizontal, 4);
                                // Angle switch button
                                let btn = gtk::Button::with_label(&format!(
                                    "[{}] {}",
                                    i + 1,
                                    angle.label
                                ));
                                btn.add_css_class("flat");
                                btn.set_hexpand(true);
                                if i == active {
                                    btn.add_css_class("suggested-action");
                                }
                                let ts = timeline_state_for_multicam_btn.clone();
                                btn.connect_clicked(move |_| {
                                    let mut st = ts.borrow_mut();
                                    let changed = st.insert_multicam_angle_switch(i);
                                    let proj_cb = st.on_project_changed.clone();
                                    drop(st);
                                    if changed {
                                        if let Some(cb) = proj_cb {
                                            cb();
                                        }
                                    }
                                });
                                row.append(&btn);
                                // Audio mute indicator
                                let audio_label = if angle.muted {
                                    "🔇"
                                } else if angle.volume < 0.01 {
                                    "🔈"
                                } else {
                                    "🔊"
                                };
                                let audio_btn = gtk::Label::new(Some(audio_label));
                                let vol_str = if angle.muted {
                                    "Audio: muted".to_string()
                                } else {
                                    format!("Audio: {:.0}%", angle.volume * 100.0)
                                };
                                audio_btn.set_tooltip_text(Some(&vol_str));
                                row.append(&audio_btn);
                                multicam_angles_box_for_sel.append(&row);
                            }
                        }
                    }
                }
            }));
    }
    {
        let prog_player = prog_player.clone();
        let pending_program_seek_ticket = pending_program_seek_ticket.clone();
        let keyframe_editor_cell = keyframe_editor_cell.clone();
        timeline_state.borrow_mut().on_seek = Some(Rc::new(move |ns| {
            let ticket = pending_program_seek_ticket.get().wrapping_add(1);
            pending_program_seek_ticket.set(ticket);
            let prog_player_seek = prog_player.clone();
            let pending_program_seek_ticket_check = pending_program_seek_ticket.clone();
            let keyframe_editor_cell = keyframe_editor_cell.clone();
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
                if let Some(ref editor) = *keyframe_editor_cell.borrow() {
                    editor.queue_redraw();
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
                                log::error!("{e}");
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
    // ── Voiceover recorder ────────────────────────────────────────────────
    let voiceover_recorder: Rc<RefCell<crate::media::voiceover::VoiceoverRecorder>> = Rc::new(
        RefCell::new(crate::media::voiceover::VoiceoverRecorder::new()),
    );
    // Shared countdown counter for the program monitor overlay (0 = hidden).
    let voiceover_countdown: Rc<Cell<u32>> = Rc::new(Cell::new(0));
    let voiceover_recording: Rc<std::cell::Cell<bool>> = Rc::new(std::cell::Cell::new(false));
    let on_apply_collected_files_impl: Rc<
        RefCell<Option<Rc<dyn Fn(crate::fcpxml::writer::CollectFilesManifest)>>>,
    > = Rc::new(RefCell::new(None));
    let on_apply_collected_files_gui: Rc<dyn Fn(crate::fcpxml::writer::CollectFilesManifest)> = {
        let cb = on_apply_collected_files_impl.clone();
        Rc::new(move |manifest| {
            let callback = cb.borrow().as_ref().cloned();
            if let Some(f) = callback {
                f(manifest);
            }
        })
    };
    let on_show_editor_impl: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_show_editor_gui: Rc<dyn Fn()> = {
        let cb = on_show_editor_impl.clone();
        Rc::new(move || {
            let callback = cb.borrow().as_ref().cloned();
            if let Some(f) = callback {
                f();
            }
        })
    };

    let (header, btn_record) = toolbar::build_toolbar(
        project.clone(),
        library.clone(),
        timeline_state.clone(),
        bg_removal_cache.clone(),
        frame_interp_cache.clone(),
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
            let cb = on_show_editor_gui.clone();
            move || cb()
        },
        {
            let cb = on_apply_collected_files_gui.clone();
            move |manifest| cb(manifest)
        },
        {
            let cb = on_export_frame_gui.clone();
            move || cb()
        },
        // on_record_voiceover — opens the voiceover recording dialog.
        {
            let recorder = voiceover_recorder.clone();
            let recording = voiceover_recording.clone();
            let project = project.clone();
            let timeline_state = timeline_state.clone();
            let prog_player = prog_player.clone();
            let on_project_changed = on_project_changed.clone();
            let window_weak = window.downgrade();
            let voiceover_countdown = voiceover_countdown.clone();
            move || {
                // If already recording, stop.
                if recording.get() {
                    recording.set(false);
                    prog_player.borrow_mut().pause();
                    // Unmute playback audio.
                    prog_player.borrow_mut().set_master_mute(false);
                    let result = recorder.borrow_mut().stop_recording();
                    if let Ok((file_path, duration_ns, start_position_ns)) = result {
                        // Find target audio track (selected or first audio track).
                        let track_id = {
                            let proj = project.borrow();
                            let ts = timeline_state.borrow();
                            let selected_tid = ts.selected_track_id.clone();
                            // Use selected track if it's an audio track.
                            selected_tid
                                .and_then(|tid| {
                                    proj.tracks
                                        .iter()
                                        .find(|t| t.id == tid && t.is_audio())
                                        .map(|t| t.id.clone())
                                })
                                .or_else(|| {
                                    proj.tracks
                                        .iter()
                                        .find(|t| t.is_audio())
                                        .map(|t| t.id.clone())
                                })
                        };
                        let track_id = track_id.unwrap_or_else(|| {
                            let mut proj = project.borrow_mut();
                            let new_track = crate::model::track::Track::new_audio("Audio 1");
                            let id = new_track.id.clone();
                            proj.tracks.push(new_track);
                            id
                        });
                        // Resolve non-overlapping start position.
                        let placement_ns = {
                            let proj = project.borrow();
                            if let Some(track) = proj.tracks.iter().find(|t| t.id == track_id) {
                                crate::media::voiceover::find_non_overlapping_start(
                                    &track.clips,
                                    start_position_ns,
                                    duration_ns,
                                )
                            } else {
                                start_position_ns
                            }
                        };
                        let clip = crate::model::clip::Clip::new(
                            &file_path,
                            duration_ns,
                            placement_ns,
                            crate::model::clip::ClipKind::Audio,
                        );
                        {
                            let mut proj = project.borrow_mut();
                            let mut ts = timeline_state.borrow_mut();
                            if let Some(track) = proj.tracks.iter().find(|t| t.id == track_id) {
                                let old_clips = track.clips.clone();
                                let mut new_clips = old_clips.clone();
                                new_clips.push(clip);
                                new_clips.sort_by_key(|c| c.timeline_start);
                                let cmd = crate::undo::SetTrackClipsCommand {
                                    track_id,
                                    old_clips,
                                    new_clips,
                                    label: "Record voiceover".to_string(),
                                };
                                ts.history.execute(Box::new(cmd), &mut proj);
                            }
                        }
                        on_project_changed();
                    }
                    recorder.borrow_mut().reset();
                    if let Some(win) = window_weak.upgrade() {
                        let proj = project.borrow();
                        let title = format!("UltimateSlice \u{2014} {} \u{2022}", proj.title);
                        win.set_title(Some(&title));
                    }
                    return;
                }

                // ── Open voiceover recording dialog ──────────────
                let Some(win) = window_weak.upgrade() else {
                    return;
                };

                #[allow(deprecated)]
                let dialog = gtk4::Dialog::builder()
                    .title("Record Voiceover")
                    .transient_for(&win)
                    .modal(true)
                    .default_width(380)
                    .build();

                let body = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
                body.set_margin_start(16);
                body.set_margin_end(16);
                body.set_margin_top(16);
                body.set_margin_bottom(16);

                // Microphone selector
                let mic_label = gtk4::Label::new(Some("Microphone"));
                mic_label.set_halign(gtk4::Align::Start);
                body.append(&mic_label);
                let mic_dropdown = gtk4::ComboBoxText::new();
                #[allow(deprecated)]
                mic_dropdown.append(Some("default"), "System Default");
                let devices = crate::media::voiceover::list_audio_input_devices();
                for (i, dev) in devices.iter().enumerate() {
                    #[allow(deprecated)]
                    mic_dropdown.append(Some(&format!("dev_{i}")), &dev.display_name);
                }
                #[allow(deprecated)]
                mic_dropdown.set_active_id(Some("default"));
                body.append(&mic_dropdown);

                // Mute playback checkbox
                let mute_check =
                    gtk4::CheckButton::with_label("Mute playback audio during recording");
                mute_check.set_active(true);
                body.append(&mute_check);

                let mono_check =
                    gtk4::CheckButton::with_label("Record as mono (recommended for single mic)");
                mono_check.set_active(true);
                body.append(&mono_check);

                // Target track info
                let track_hint = {
                    let ts = timeline_state.borrow();
                    let proj = project.borrow();
                    let selected_tid = ts.selected_track_id.clone();
                    let target = selected_tid
                        .and_then(|tid| proj.tracks.iter().find(|t| t.id == tid && t.is_audio()))
                        .or_else(|| proj.tracks.iter().find(|t| t.is_audio()));
                    if let Some(t) = target {
                        format!("Target track: {}", t.label)
                    } else {
                        "A new audio track will be created".to_string()
                    }
                };
                let track_label = gtk4::Label::new(Some(&track_hint));
                track_label.set_halign(gtk4::Align::Start);
                track_label.add_css_class("dim-label");
                body.append(&track_label);

                // Playhead position
                let playhead_ns = timeline_state.borrow().playhead_ns;
                let pos_label = gtk4::Label::new(Some(&format!(
                    "Recording starts at: {:.2}s",
                    playhead_ns as f64 / 1e9
                )));
                pos_label.set_halign(gtk4::Align::Start);
                pos_label.add_css_class("dim-label");
                body.append(&pos_label);

                // Countdown info
                let countdown_label =
                    gtk4::Label::new(Some("3-second countdown before recording begins"));
                countdown_label.set_halign(gtk4::Align::Start);
                countdown_label.add_css_class("dim-label");
                body.append(&countdown_label);

                #[allow(deprecated)]
                dialog.content_area().append(&body);

                #[allow(deprecated)]
                dialog.add_button("Cancel", gtk4::ResponseType::Cancel);
                #[allow(deprecated)]
                dialog.add_button("Start Recording", gtk4::ResponseType::Accept);

                // Wire dialog response
                let recorder = recorder.clone();
                let recording = recording.clone();
                let prog_player = prog_player.clone();
                let window_weak = window_weak.clone();
                let project = project.clone();
                let voiceover_countdown_cb = voiceover_countdown.clone();
                #[allow(deprecated)]
                dialog.connect_response(move |d, resp| {
                    if resp != gtk4::ResponseType::Accept {
                        d.close();
                        return;
                    }
                    d.close();

                    // Read settings from dialog
                    #[allow(deprecated)]
                    let mic_id = mic_dropdown.active_id().map(|s| s.to_string());
                    let mute_playback = mute_check.is_active();
                    let record_mono = mono_check.is_active();

                    // Find selected device
                    let selected_device: Option<gstreamer::Device> =
                        mic_id.as_deref().and_then(|id| {
                            if id == "default" {
                                None
                            } else if let Some(idx_str) = id.strip_prefix("dev_") {
                                idx_str
                                    .parse::<usize>()
                                    .ok()
                                    .and_then(|idx| devices.get(idx).map(|d| d.device.clone()))
                            } else {
                                None
                            }
                        });

                    // Start countdown with a visible dialog.
                    recording.set(true);
                    // NOTE: mute is applied AFTER play() inside the countdown timer,
                    // because play() rebuilds the pipeline and resets the audio sink.

                    // Show countdown overlay on the program monitor.
                    voiceover_countdown_cb.set(3);

                    let countdown: Rc<std::cell::Cell<u32>> = Rc::new(std::cell::Cell::new(3));
                    let recorder = recorder.clone();
                    let recording = recording.clone();
                    let prog_player = prog_player.clone();
                    let window_weak = window_weak.clone();
                    let project = project.clone();
                    let mute_after_play = mute_playback;
                    let record_mono = record_mono;
                    let vo_countdown = voiceover_countdown_cb.clone();
                    glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
                        if !recording.get() {
                            vo_countdown.set(0);
                            return glib::ControlFlow::Break;
                        }
                        let remaining = countdown.get().saturating_sub(1);
                        countdown.set(remaining);
                        vo_countdown.set(remaining);
                        if remaining > 0 {
                            glib::ControlFlow::Continue
                        } else {
                            match recorder.borrow_mut().start_recording(
                                playhead_ns,
                                selected_device.as_ref(),
                                record_mono,
                            ) {
                                Ok(_) => {
                                    {
                                        let mut pp = prog_player.borrow_mut();
                                        pp.play();
                                        if mute_after_play {
                                            pp.set_master_mute(true);
                                        }
                                    }
                                    if let Some(win) = window_weak.upgrade() {
                                        let proj = project.borrow();
                                        win.set_title(Some(&format!(
                                            "UltimateSlice \u{2014} {} (Recording\u{2026})",
                                            proj.title
                                        )));
                                    }
                                }
                                Err(e) => {
                                    log::warn!("Voiceover start failed: {e}");
                                    recording.set(false);
                                }
                            }
                            glib::ControlFlow::Break
                        }
                    });
                });

                dialog.present();
            }
        },
    );
    window.set_titlebar(Some(&header));

    // Sync Record button state with voiceover_recording flag.
    {
        let recording = voiceover_recording.clone();
        let btn = btn_record.clone();
        let mut was_recording = false;
        glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
            let now = recording.get();
            if now != was_recording {
                was_recording = now;
                if now {
                    btn.set_label("Stop Recording");
                    btn.add_css_class("destructive-action");
                } else {
                    btn.set_label("Record");
                    btn.remove_css_class("destructive-action");
                }
            }
            glib::ControlFlow::Continue
        });
    }

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
    let workspace_layouts_applying = Rc::new(Cell::new(false));
    let workspace_layout_pending_name: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let workspace_layout_apply_generation = Rc::new(Cell::new(0u64));
    let workspace_layout_controls_updating = Rc::new(Cell::new(false));
    let sync_workspace_layout_controls_impl: Rc<RefCell<Option<Rc<dyn Fn()>>>> =
        Rc::new(RefCell::new(None));
    let sync_workspace_layout_controls: Rc<dyn Fn()> = {
        let cb = sync_workspace_layout_controls_impl.clone();
        Rc::new(move || {
            if let Some(f) = cb.borrow().as_ref() {
                f();
            }
        })
    };
    let sync_workspace_layout_state_impl: Rc<RefCell<Option<Rc<dyn Fn()>>>> =
        Rc::new(RefCell::new(None));
    let sync_workspace_layout_state: Rc<dyn Fn()> = {
        let cb = sync_workspace_layout_state_impl.clone();
        Rc::new(move || {
            if let Some(f) = cb.borrow().as_ref() {
                f();
            }
        })
    };
    let apply_workspace_arrangement_impl: Rc<
        RefCell<Option<Rc<dyn Fn(crate::ui_state::WorkspaceArrangement)>>>,
    > = Rc::new(RefCell::new(None));
    let apply_workspace_arrangement: Rc<dyn Fn(crate::ui_state::WorkspaceArrangement)> = {
        let cb = apply_workspace_arrangement_impl.clone();
        Rc::new(move |arrangement| {
            if let Some(f) = cb.borrow().as_ref() {
                f(arrangement);
            }
        })
    };

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
    let (preview_widget, source_marks, clip_name_label, set_audio_only) = preview::build_preview(
        player.clone(),
        paintable,
        on_append.clone(),
        on_insert.clone(),
        on_overwrite.clone(),
        on_close_preview.clone(),
    );
    let source_monitor_panel = gtk::Box::new(Orientation::Vertical, 4);
    source_monitor_panel.append(&preview_widget);

    let source_keyword_controls = gtk::Box::new(Orientation::Vertical, 4);
    source_keyword_controls.set_margin_start(8);
    source_keyword_controls.set_margin_end(8);
    source_keyword_controls.set_margin_bottom(8);

    let source_keyword_row = gtk::Box::new(Orientation::Horizontal, 4);
    let source_keyword_combo = gtk::ComboBoxText::new();
    source_keyword_combo.append(Some(SOURCE_KEYWORD_NEW_ID), "New keyword range");
    source_keyword_combo.set_active_id(Some(SOURCE_KEYWORD_NEW_ID));
    source_keyword_combo.set_hexpand(true);
    source_keyword_row.append(&source_keyword_combo);

    let source_keyword_entry = gtk::Entry::new();
    source_keyword_entry.set_hexpand(true);
    source_keyword_entry.set_placeholder_text(Some("Keyword label"));
    source_keyword_row.append(&source_keyword_entry);

    let add_source_keyword_btn = gtk::Button::with_label("Add");
    source_keyword_row.append(&add_source_keyword_btn);

    let update_source_keyword_btn = gtk::Button::with_label("Update");
    source_keyword_row.append(&update_source_keyword_btn);

    let remove_source_keyword_btn = gtk::Button::with_label("Remove");
    source_keyword_row.append(&remove_source_keyword_btn);

    source_keyword_controls.append(&source_keyword_row);

    let source_keyword_status_label =
        gtk::Label::new(Some("Use source In/Out marks to define a keyword range."));
    source_keyword_status_label.set_halign(gtk::Align::Start);
    source_keyword_status_label.set_xalign(0.0);
    source_keyword_status_label.set_wrap(true);
    source_keyword_status_label.add_css_class("media-meta-secondary");
    source_keyword_controls.append(&source_keyword_status_label);

    source_monitor_panel.append(&source_keyword_controls);

    let selected_source_keyword_id: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let refresh_source_keyword_actions: Rc<dyn Fn()> = {
        let source_marks = source_marks.clone();
        let library = library.clone();
        let selected_source_keyword_id = selected_source_keyword_id.clone();
        let source_keyword_entry = source_keyword_entry.clone();
        let add_source_keyword_btn = add_source_keyword_btn.clone();
        let update_source_keyword_btn = update_source_keyword_btn.clone();
        let remove_source_keyword_btn = remove_source_keyword_btn.clone();
        let source_keyword_status_label = source_keyword_status_label.clone();
        Rc::new(move || {
            let (path, in_ns, out_ns) = {
                let marks = source_marks.borrow();
                (marks.path.clone(), marks.in_ns, marks.out_ns)
            };
            let entry_text = source_keyword_entry.text().trim().to_string();
            let selected_id = selected_source_keyword_id.borrow().clone();
            let (has_item, selected_range_text, summary_text) = {
                let lib = library.borrow();
                let item = lib.items.iter().find(|item| item.source_path == path);
                let selected_range_text = item.and_then(|item| {
                    selected_id.as_deref().and_then(|range_id| {
                        item.keyword_ranges
                            .iter()
                            .find(|range| range.id == range_id)
                            .map(format_source_keyword_range)
                    })
                });
                let summary_text = item.and_then(|item| media_keyword_summary(item, 3));
                (item.is_some(), selected_range_text, summary_text)
            };
            let has_valid_range = !path.is_empty() && out_ns > in_ns;
            let has_label = !entry_text.is_empty();
            let has_selected_range = selected_range_text.is_some();

            add_source_keyword_btn.set_sensitive(has_item && has_valid_range && has_label);
            update_source_keyword_btn
                .set_sensitive(has_item && has_valid_range && has_selected_range && has_label);
            remove_source_keyword_btn.set_sensitive(has_item && has_selected_range);

            let status = if path.is_empty() {
                "Load a source clip to add keyword ranges.".to_string()
            } else if !has_item {
                "This source is not available in the media library.".to_string()
            } else if !has_valid_range {
                "Set source In and Out to define a keyword range.".to_string()
            } else if let Some(selected_range_text) = selected_range_text {
                format!("Selected: {selected_range_text}")
            } else if let Some(summary_text) = summary_text {
                format!("Current: {summary_text}")
            } else {
                "Use source In/Out marks to define a keyword range.".to_string()
            };
            source_keyword_status_label.set_text(&status);
        })
    };
    let refresh_source_keyword_picker: Rc<dyn Fn()> = {
        let source_marks = source_marks.clone();
        let library = library.clone();
        let selected_source_keyword_id = selected_source_keyword_id.clone();
        let source_keyword_combo = source_keyword_combo.clone();
        let refresh_source_keyword_actions = refresh_source_keyword_actions.clone();
        Rc::new(move || {
            let path = source_marks.borrow().path.clone();
            let selected_id = selected_source_keyword_id.borrow().clone();
            let ranges = {
                let lib = library.borrow();
                lib.items
                    .iter()
                    .find(|item| item.source_path == path)
                    .map(|item| item.keyword_ranges.clone())
                    .unwrap_or_default()
            };
            source_keyword_combo.remove_all();
            source_keyword_combo.append(Some(SOURCE_KEYWORD_NEW_ID), "New keyword range");
            let mut active_id = SOURCE_KEYWORD_NEW_ID.to_string();
            for range in &ranges {
                source_keyword_combo.append(
                    Some(range.id.as_str()),
                    format_source_keyword_range(range).as_str(),
                );
                if selected_id.as_deref() == Some(range.id.as_str()) {
                    active_id = range.id.clone();
                }
            }
            if active_id == SOURCE_KEYWORD_NEW_ID {
                *selected_source_keyword_id.borrow_mut() = None;
            }
            source_keyword_combo.set_active_id(Some(&active_id));
            refresh_source_keyword_actions();
        })
    };
    {
        let source_marks = source_marks.clone();
        let library = library.clone();
        let selected_source_keyword_id = selected_source_keyword_id.clone();
        let source_keyword_entry = source_keyword_entry.clone();
        let refresh_source_keyword_actions = refresh_source_keyword_actions.clone();
        source_keyword_combo.connect_changed(move |combo| {
            let path = source_marks.borrow().path.clone();
            let selected_id = combo.active_id().and_then(|id| {
                let id = id.to_string();
                (id != SOURCE_KEYWORD_NEW_ID).then_some(id)
            });
            *selected_source_keyword_id.borrow_mut() = selected_id.clone();
            let selected_label = {
                let lib = library.borrow();
                lib.items
                    .iter()
                    .find(|item| item.source_path == path)
                    .and_then(|item| {
                        selected_id.as_deref().and_then(|range_id| {
                            item.keyword_ranges
                                .iter()
                                .find(|range| range.id == range_id)
                                .map(|range| range.label.clone())
                        })
                    })
            };
            if let Some(selected_label) = selected_label {
                source_keyword_entry.set_text(&selected_label);
            } else {
                source_keyword_entry.set_text("");
            }
            refresh_source_keyword_actions();
        });
    }
    {
        let refresh_source_keyword_actions = refresh_source_keyword_actions.clone();
        source_keyword_entry.connect_changed(move |_| {
            refresh_source_keyword_actions();
        });
    }
    refresh_source_keyword_picker();
    *refresh_source_preview_preferences_impl.borrow_mut() = Some({
        let player = player.clone();
        let source_marks = source_marks.clone();
        let library = library.clone();
        let project = project.clone();
        let proxy_cache = proxy_cache.clone();
        let source_original_uri_for_proxy_fallback = source_original_uri_for_proxy_fallback.clone();
        let set_audio_only = set_audio_only.clone();
        Rc::new(move |old_state, new_state| {
            if old_state.proxy_mode == new_state.proxy_mode {
                return;
            }
            let (path, duration_ns) = {
                let marks = source_marks.borrow();
                (marks.path.clone(), marks.duration_ns)
            };
            if path.is_empty() {
                return;
            }
            let source_info = {
                let lib = library.borrow();
                let proj = project.borrow();
                lookup_source_placement_info(&lib.items, &proj, &path)
            };
            reload_source_preview_selection(
                &path,
                duration_ns,
                source_info,
                &player,
                &project,
                &proxy_cache,
                &new_state.proxy_mode,
                &source_original_uri_for_proxy_fallback,
                &set_audio_only,
            );
        })
    });

    // Wire on_drop_clip — placed here so it can read source_marks to honour
    // the in/out selection set in the source monitor.
    {
        let project = project.clone();
        let library = library.clone();
        let on_project_changed = on_project_changed.clone();
        let preferences_state = preferences_state.clone();
        let source_marks = source_marks.clone();
        let timeline_state_for_drop = timeline_state.clone();
        timeline_state.borrow_mut().on_drop_clip = Some(Rc::new(
            move |source_path, duration_ns, track_idx, timeline_start_ns| {
                let magnetic_mode = timeline_state_for_drop.borrow().magnetic_mode;
                let source_monitor_auto_link_av =
                    preferences_state.borrow().source_monitor_auto_link_av;
                let source_info = {
                    let marks = source_marks.borrow();
                    if marks.path == source_path {
                        SourcePlacementInfo {
                            is_audio_only: marks.is_audio_only,
                            has_audio: marks.has_audio,
                            is_image: marks.is_image,
                            is_animated_svg: marks.is_animated_svg,
                            source_timecode_base_ns: marks.source_timecode_base_ns,
                            audio_channel_mode: marks.audio_channel_mode,
                        }
                    } else {
                        let lib = library.borrow();
                        let proj = project.borrow();
                        lookup_source_placement_info(&lib.items, &proj, &source_path)
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
                let placement_plan = build_source_placement_plan_by_track_index(
                    &proj,
                    Some(track_idx),
                    source_info,
                    source_monitor_auto_link_av,
                );
                let magnetic_mode_for_placement =
                    magnetic_mode && !placement_plan.uses_linked_pair();
                let media_dur_opt = if source_info.is_image {
                    if source_info.is_animated_svg {
                        Some(duration_ns)
                    } else {
                        None
                    }
                } else {
                    Some(duration_ns)
                };
                let mut track_changes: Vec<TrackClipsChange> = Vec::new();
                for (target_track_idx, clip) in build_source_clips_for_plan(
                    &placement_plan,
                    &source_path,
                    src_in,
                    src_out,
                    timeline_start_ns,
                    source_info.source_timecode_base_ns,
                    source_info.audio_channel_mode,
                    media_dur_opt,
                    source_info.is_animated_svg,
                ) {
                    track_changes.push(add_clip_to_track(
                        &mut proj.tracks[target_track_idx],
                        clip,
                        magnetic_mode_for_placement,
                    ));
                }
                if track_changes.is_empty() {
                    return;
                }
                proj.dirty = true;
                drop(proj);
                let cmd: Box<dyn crate::undo::EditCommand> = if track_changes.len() == 1 {
                    let change = track_changes.pop().unwrap();
                    Box::new(crate::undo::SetTrackClipsCommand {
                        track_id: change.track_id,
                        old_clips: change.old_clips,
                        new_clips: change.new_clips,
                        label: "Drop clip".to_string(),
                    })
                } else {
                    Box::new(crate::undo::SetMultipleTracksClipsCommand {
                        changes: track_changes,
                        label: "Drop clip".to_string(),
                    })
                };
                timeline_state_for_drop
                    .borrow_mut()
                    .history
                    .undo_stack
                    .push(cmd);
                timeline_state_for_drop
                    .borrow_mut()
                    .history
                    .redo_stack
                    .clear();
                on_project_changed();
            },
        ));
    }

    // Wire on_drop_external_files — handles file manager drops onto the timeline.
    // Imports each file into the library (synchronous probe) and places clips sequentially.
    {
        let project = project.clone();
        let library = library.clone();
        let on_project_changed = on_project_changed.clone();
        let preferences_state = preferences_state.clone();
        let timeline_state_for_ext = timeline_state.clone();
        timeline_state.borrow_mut().on_drop_external_files = Some(Rc::new(
            move |file_paths: Vec<String>, track_idx: usize, timeline_start_ns: u64| {
                let magnetic_mode = timeline_state_for_ext.borrow().magnetic_mode;
                let source_monitor_auto_link_av =
                    preferences_state.borrow().source_monitor_auto_link_av;
                let mut cursor_ns = timeline_start_ns;
                let mut track_changes: Vec<crate::undo::TrackClipsChange> = Vec::new();

                for path in &file_paths {
                    let is_image = crate::model::clip::is_image_file(path);
                    let animated_svg_analysis = if crate::model::clip::is_svg_file(path) {
                        crate::media::animated_svg::analyze_svg_path(path).ok()
                    } else {
                        None
                    };
                    let is_animated_svg = animated_svg_analysis
                        .as_ref()
                        .is_some_and(|analysis| analysis.is_animated);

                    // Import into library if not already present (synchronous probe).
                    let already_in_library = library
                        .borrow()
                        .items
                        .iter()
                        .any(|item| item.source_path == *path);
                    if !already_in_library {
                        let metadata = crate::media::probe_cache::probe_media_metadata(path);
                        let duration_ns = if is_animated_svg {
                            animated_svg_analysis
                                .as_ref()
                                .and_then(|analysis| analysis.duration_ns)
                                .unwrap_or(4_000_000_000u64)
                        } else if is_image {
                            4_000_000_000u64
                        } else {
                            metadata.duration_ns.unwrap_or(10_000_000_000)
                        };
                        let mut item = MediaItem::new(path.clone(), duration_ns);
                        item.is_audio_only = metadata.is_audio_only;
                        item.has_audio = metadata.has_audio;
                        item.is_image = is_image;
                        item.is_animated_svg = is_animated_svg;
                        item.source_timecode_base_ns =
                            metadata.source_timecode_base_ns.or_else(|| {
                                lookup_source_timecode_base_ns(
                                    &library.borrow().items,
                                    &project.borrow(),
                                    path,
                                )
                            });
                        item.video_width = metadata.video_width;
                        item.video_height = metadata.video_height;
                        item.frame_rate_num = metadata.frame_rate_num;
                        item.frame_rate_den = metadata.frame_rate_den;
                        item.codec_summary = metadata.codec_summary.clone();
                        item.file_size_bytes = metadata.file_size_bytes;
                        library.borrow_mut().items.push(item);
                    }

                    // Look up placement info (may re-probe if needed).
                    let source_info = {
                        let lib = library.borrow();
                        let proj = project.borrow();
                        lookup_source_placement_info(&lib.items, &proj, path)
                    };

                    let duration_ns = {
                        let lib = library.borrow();
                        lib.items
                            .iter()
                            .find(|item| item.source_path == *path)
                            .map(|item| item.duration_ns)
                            .unwrap_or(if is_image {
                                4_000_000_000
                            } else {
                                10_000_000_000
                            })
                    };

                    let src_in = 0u64;
                    let src_out = duration_ns;

                    let mut proj = project.borrow_mut();
                    let placement_plan = build_source_placement_plan_by_track_index(
                        &proj,
                        Some(track_idx),
                        source_info,
                        source_monitor_auto_link_av,
                    );
                    let magnetic_mode_for_placement =
                        magnetic_mode && !placement_plan.uses_linked_pair();
                    let media_dur_opt = if source_info.is_image {
                        if source_info.is_animated_svg {
                            Some(duration_ns)
                        } else {
                            None
                        }
                    } else {
                        Some(duration_ns)
                    };
                    for (target_track_idx, clip) in build_source_clips_for_plan(
                        &placement_plan,
                        path,
                        src_in,
                        src_out,
                        cursor_ns,
                        source_info.source_timecode_base_ns,
                        source_info.audio_channel_mode,
                        media_dur_opt,
                        source_info.is_animated_svg,
                    ) {
                        track_changes.push(add_clip_to_track(
                            &mut proj.tracks[target_track_idx],
                            clip,
                            magnetic_mode_for_placement,
                        ));
                    }

                    proj.dirty = true;
                    drop(proj);
                    cursor_ns += src_out.saturating_sub(src_in);
                }

                // Single undo entry for the entire multi-file drop.
                if !track_changes.is_empty() {
                    let cmd: Box<dyn crate::undo::EditCommand> = if track_changes.len() == 1 {
                        let change = track_changes.pop().unwrap();
                        Box::new(crate::undo::SetTrackClipsCommand {
                            track_id: change.track_id,
                            old_clips: change.old_clips,
                            new_clips: change.new_clips,
                            label: "Drop files from file manager".to_string(),
                        })
                    } else {
                        Box::new(crate::undo::SetMultipleTracksClipsCommand {
                            changes: track_changes,
                            label: "Drop files from file manager".to_string(),
                        })
                    };
                    timeline_state_for_ext
                        .borrow_mut()
                        .history
                        .undo_stack
                        .push(cmd);
                    timeline_state_for_ext
                        .borrow_mut()
                        .history
                        .redo_stack
                        .clear();
                    on_project_changed();
                }
            },
        ));
    }

    // Shared flag: true while audio sync is running (read by status bar timer).
    let audio_sync_in_progress: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    // Whether the current sync operation should also replace audio (link + mute anchor).
    let sync_replace_mode: Rc<Cell<bool>> = Rc::new(Cell::new(false));

    // Wire on_sync_audio — spawns a background thread for FFT cross-correlation.
    {
        let project = project.clone();
        let on_project_changed = on_project_changed.clone();
        let window_weak = window.downgrade();
        let sync_rx: Rc<
            RefCell<Option<std::sync::mpsc::Receiver<Vec<(String, i64, f32, Option<f64>)>>>>,
        > = Rc::new(RefCell::new(None));
        let sync_rx_for_timer = sync_rx.clone();
        let audio_sync_in_progress_timer = audio_sync_in_progress.clone();
        // Poll timer for sync results
        {
            let project = project.clone();
            let on_project_changed = on_project_changed.clone();
            let timeline_state = timeline_state.clone();
            let window_weak = window_weak.clone();
            let sync_replace_mode_timer = sync_replace_mode.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                let rx_opt = sync_rx_for_timer.borrow();
                if let Some(ref rx) = *rx_opt {
                    if let Ok(results) = rx.try_recv() {
                        drop(rx_opt);
                        sync_rx_for_timer.borrow_mut().take();
                        audio_sync_in_progress_timer.set(false);
                        let replace = sync_replace_mode_timer.get();
                        sync_replace_mode_timer.set(false);
                        apply_audio_sync_results(
                            &results,
                            &project,
                            &timeline_state,
                            &on_project_changed,
                            window_weak.upgrade().as_ref(),
                            replace,
                        );
                    }
                }
                glib::ControlFlow::Continue
            });
        }
        let audio_sync_in_progress_cb = audio_sync_in_progress.clone();
        let sync_rx_for_replace = sync_rx.clone();
        let project_for_replace = project.clone();
        let window_weak_for_replace = window_weak.clone();
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
                    let results: Vec<(String, i64, f32, Option<f64>)> = sync_results
                        .into_iter()
                        .map(|r| (r.clip_id, r.offset_ns, r.confidence, r.drift_speed))
                        .collect();
                    let _ = tx.send(results);
                });
            },
        ));

        // Wire on_sync_replace_audio — same sync flow but sets replace_audio flag.
        {
            let sync_rx2 = sync_rx_for_replace;
            let audio_sync_in_progress_cb2 = audio_sync_in_progress.clone();
            let sync_replace_mode_cb = sync_replace_mode.clone();
            let project = project_for_replace;
            let window_weak = window_weak_for_replace;
            timeline_state.borrow_mut().on_sync_replace_audio = Some(Rc::new(
                move |clip_infos: Vec<(String, String, u64, u64, u64, String)>| {
                    if sync_rx2.borrow().is_some() {
                        return; // Sync already in progress
                    }
                    audio_sync_in_progress_cb2.set(true);
                    sync_replace_mode_cb.set(true);
                    if let Some(win) = window_weak.upgrade() {
                        let proj = project.borrow();
                        win.set_title(Some(&format!(
                            "UltimateSlice \u{2014} {} (Syncing & replacing audio\u{2026})",
                            proj.title
                        )));
                    }
                    let (tx, rx) = std::sync::mpsc::channel();
                    *sync_rx2.borrow_mut() = Some(rx);
                    std::thread::spawn(move || {
                        let _ = gstreamer::init();
                        let clips: Vec<(String, String, u64, u64)> = clip_infos
                            .iter()
                            .map(|(id, path, src_in, src_out, _tl_start, _track_id)| {
                                (id.clone(), path.clone(), *src_in, *src_out)
                            })
                            .collect();
                        let sync_results = crate::media::audio_sync::sync_clips_by_audio(&clips);
                        let results: Vec<(String, i64, f32, Option<f64>)> = sync_results
                            .into_iter()
                            .map(|r| (r.clip_id, r.offset_ns, r.confidence, r.drift_speed))
                            .collect();
                        let _ = tx.send(results);
                    });
                },
            ));
        }
    }

    // Wire on_create_multicam — spawns audio sync in background, then creates multicam clip.
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let window_weak = window_weak.clone();
        let multicam_sync_rx: Rc<
            RefCell<
                Option<
                    std::sync::mpsc::Receiver<(
                        Vec<(String, String, u64, u64, u64, String)>,
                        Vec<(String, i64, f32, Option<f64>)>,
                    )>,
                >,
            >,
        > = Rc::new(RefCell::new(None));
        // Poll timer for multicam sync results
        {
            let multicam_sync_rx = multicam_sync_rx.clone();
            let project = project.clone();
            let timeline_state = timeline_state.clone();
            let on_project_changed = on_project_changed.clone();
            let window_weak = window_weak.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                let result = {
                    let rx_opt = multicam_sync_rx.borrow();
                    rx_opt.as_ref().and_then(|rx| rx.try_recv().ok())
                };
                if let Some((clip_infos, sync_results)) = result {
                    *multicam_sync_rx.borrow_mut() = None;
                    // Build multicam angles from sync results
                    let anchor_id = clip_infos
                        .first()
                        .map(|(id, ..)| id.clone())
                        .unwrap_or_default();
                    let anchor_start = clip_infos
                        .first()
                        .map(|(_, _, _, _, tl, _)| *tl)
                        .unwrap_or(0);
                    let mut angles: Vec<crate::model::clip::MulticamAngle> = Vec::new();
                    for (i, (id, path, src_in, src_out, _tl_start, _track_id)) in
                        clip_infos.iter().enumerate()
                    {
                        let offset_ns = if *id == anchor_id {
                            0i64
                        } else {
                            sync_results
                                .iter()
                                .find(|(rid, ..)| rid == id)
                                .map(|(_, o, _, _)| *o)
                                .unwrap_or(0)
                        };
                        let label = format!("Angle {}", i + 1);
                        // Compute synced source_in: anchor's source_in + offset
                        let synced_in = (*src_in as i64 + offset_ns).max(0) as u64;
                        let synced_out = *src_out;
                        angles.push(crate::model::clip::MulticamAngle {
                            id: uuid::Uuid::new_v4().to_string(),
                            label,
                            source_path: path.clone(),
                            source_in: synced_in,
                            source_out: synced_out,
                            sync_offset_ns: offset_ns,
                            source_timecode_base_ns: None,
                            media_duration_ns: None,
                            volume: if i == 0 { 1.0 } else { 0.0 },
                            muted: i != 0,
                        });
                    }
                    if angles.len() >= 2 {
                        let multicam = crate::model::clip::Clip::new_multicam(anchor_start, angles);
                        let multicam_id = multicam.id.clone();
                        // Remove original clips and add multicam clip
                        let selected_ids: std::collections::HashSet<String> =
                            clip_infos.iter().map(|(id, ..)| id.clone()).collect();
                        let mut proj = project.borrow_mut();
                        let mut changes = Vec::new();
                        let mut placement_track_id: Option<String> = None;
                        for track in &proj.tracks {
                            if track.clips.iter().any(|c| selected_ids.contains(&c.id)) {
                                let old_clips = track.clips.clone();
                                let mut new_clips: Vec<crate::model::clip::Clip> = track
                                    .clips
                                    .iter()
                                    .filter(|c| !selected_ids.contains(&c.id))
                                    .cloned()
                                    .collect();
                                if placement_track_id.is_none() {
                                    new_clips.push(multicam.clone());
                                    new_clips.sort_by_key(|c| c.timeline_start);
                                    placement_track_id = Some(track.id.clone());
                                }
                                changes.push(crate::undo::TrackClipsChange {
                                    track_id: track.id.clone(),
                                    old_clips,
                                    new_clips,
                                });
                            }
                        }
                        let cmd = Box::new(crate::undo::SetMultipleTracksClipsCommand {
                            changes,
                            label: "Create Multicam Clip".to_string(),
                        });
                        {
                            let mut st = timeline_state.borrow_mut();
                            st.history.execute(cmd, &mut proj);
                        }
                        drop(proj);
                        on_project_changed();
                    }
                    if let Some(win) = window_weak.upgrade() {
                        let proj = project.borrow();
                        let title = &proj.title;
                        win.set_title(Some(&format!("UltimateSlice — {title}")));
                    }
                }
                glib::ControlFlow::Continue
            });
        }
        timeline_state.borrow_mut().on_create_multicam = Some(Rc::new(
            move |clip_infos: Vec<(String, String, u64, u64, u64, String)>| {
                if multicam_sync_rx.borrow().is_some() {
                    return; // sync already in progress
                }
                if let Some(win) = window_weak.upgrade() {
                    let proj = project.borrow();
                    win.set_title(Some(&format!(
                        "UltimateSlice — {} (Syncing multicam...)",
                        proj.title
                    )));
                }
                let (tx, rx) = std::sync::mpsc::channel();
                *multicam_sync_rx.borrow_mut() = Some(rx);
                let clip_infos_clone = clip_infos.clone();
                std::thread::spawn(move || {
                    let _ = gstreamer::init();
                    let clips: Vec<(String, String, u64, u64)> = clip_infos_clone
                        .iter()
                        .map(|(id, path, src_in, src_out, _, _)| {
                            (id.clone(), path.clone(), *src_in, *src_out)
                        })
                        .collect();
                    let sync_results = crate::media::audio_sync::sync_clips_by_audio(&clips);
                    let results: Vec<(String, i64, f32, Option<f64>)> = sync_results
                        .into_iter()
                        .map(|r| (r.clip_id, r.offset_ns, r.confidence, r.drift_speed))
                        .collect();
                    let _ = tx.send((clip_infos, results));
                });
            },
        ));
    }

    // Shared flag: true while silence detection is running (read by status bar timer).
    let silence_detect_in_progress: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let scene_detect_in_progress: Rc<Cell<bool>> = Rc::new(Cell::new(false));

    // Wire on_remove_silent_parts — spawns a background thread for ffmpeg silencedetect.
    {
        let project = project.clone();
        let on_project_changed = on_project_changed.clone();
        let window_weak = window.downgrade();
        // Result: (clip_id, track_id, silence_intervals)
        let silence_rx: Rc<
            RefCell<Option<std::sync::mpsc::Receiver<(String, String, Vec<(f64, f64)>)>>>,
        > = Rc::new(RefCell::new(None));
        let silence_rx_for_timer = silence_rx.clone();
        let silence_detect_in_progress_timer = silence_detect_in_progress.clone();
        // Poll timer for silence detection results
        {
            let project = project.clone();
            let on_project_changed = on_project_changed.clone();
            let timeline_state = timeline_state.clone();
            let window_weak = window_weak.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                let rx_opt = silence_rx_for_timer.borrow();
                if let Some(ref rx) = *rx_opt {
                    if let Ok((clip_id, track_id, silence_intervals)) = rx.try_recv() {
                        drop(rx_opt);
                        silence_rx_for_timer.borrow_mut().take();
                        silence_detect_in_progress_timer.set(false);
                        apply_remove_silent_parts_results(
                            &clip_id,
                            &track_id,
                            &silence_intervals,
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
        let silence_detect_in_progress_cb = silence_detect_in_progress.clone();
        timeline_state.borrow_mut().on_remove_silent_parts = Some(Rc::new(
            move |clip_id: String,
                  track_id: String,
                  source_path: String,
                  source_in: u64,
                  source_out: u64,
                  noise_db: f64,
                  min_duration: f64| {
                if silence_rx.borrow().is_some() {
                    return; // Already in progress
                }
                silence_detect_in_progress_cb.set(true);
                if let Some(win) = window_weak.upgrade() {
                    let proj = project.borrow();
                    let title = proj.title.clone();
                    drop(proj);
                    win.set_title(Some(&format!(
                        "UltimateSlice — {title} (Detecting silence...)"
                    )));
                }
                let (tx, rx) = std::sync::mpsc::channel();
                *silence_rx.borrow_mut() = Some(rx);
                std::thread::spawn(move || {
                    let result = crate::media::export::detect_silence(
                        &source_path,
                        source_in,
                        source_out,
                        noise_db,
                        min_duration,
                    );
                    let intervals = result.unwrap_or_default();
                    let _ = tx.send((clip_id, track_id, intervals));
                });
            },
        ));
    }

    // Wire on_detect_scene_cuts — spawns a background thread for ffmpeg scdet.
    {
        let project = project.clone();
        let on_project_changed = on_project_changed.clone();
        let window_weak = window.downgrade();
        let scene_rx: Rc<RefCell<Option<std::sync::mpsc::Receiver<(String, String, Vec<f64>)>>>> =
            Rc::new(RefCell::new(None));
        let scene_rx_for_timer = scene_rx.clone();
        let scene_detect_in_progress_timer = scene_detect_in_progress.clone();
        {
            let project = project.clone();
            let on_project_changed = on_project_changed.clone();
            let timeline_state = timeline_state.clone();
            let window_weak = window_weak.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                let rx_opt = scene_rx_for_timer.borrow();
                if let Some(ref rx) = *rx_opt {
                    if let Ok((clip_id, track_id, cut_points)) = rx.try_recv() {
                        drop(rx_opt);
                        scene_rx_for_timer.borrow_mut().take();
                        scene_detect_in_progress_timer.set(false);
                        apply_scene_cut_results(
                            &clip_id,
                            &track_id,
                            &cut_points,
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
        let scene_detect_in_progress_cb = scene_detect_in_progress.clone();
        timeline_state.borrow_mut().on_detect_scene_cuts = Some(Rc::new(
            move |clip_id: String,
                  track_id: String,
                  source_path: String,
                  source_in: u64,
                  source_out: u64,
                  threshold: f64| {
                if scene_rx.borrow().is_some() {
                    return;
                }
                scene_detect_in_progress_cb.set(true);
                if let Some(win) = window_weak.upgrade() {
                    let proj = project.borrow();
                    let title = proj.title.clone();
                    drop(proj);
                    win.set_title(Some(&format!(
                        "UltimateSlice \u{2014} {title} (Detecting scene cuts...)"
                    )));
                }
                let (tx, rx) = std::sync::mpsc::channel();
                *scene_rx.borrow_mut() = Some(rx);
                std::thread::spawn(move || {
                    let result = crate::media::export::detect_scene_cuts(
                        &source_path,
                        source_in,
                        source_out,
                        threshold,
                    );
                    let cuts = result.unwrap_or_default();
                    let _ = tx.send((clip_id, track_id, cuts));
                });
            },
        ));
    }

    // Wire on_convert_ltc_to_timecode — decode LTC in the background and apply
    // source timecode + channel routing on the main thread.
    {
        type LtcConversionThreadResult = Result<PreparedLtcConversion, String>;
        let project = project.clone();
        let library = library.clone();
        let source_marks = source_marks.clone();
        let on_project_changed = on_project_changed.clone();
        let window_weak = window.downgrade();
        let ltc_rx: Rc<RefCell<Option<std::sync::mpsc::Receiver<LtcConversionThreadResult>>>> =
            Rc::new(RefCell::new(None));
        let ltc_rx_for_timer = ltc_rx.clone();
        {
            let project = project.clone();
            let library = library.clone();
            let source_marks = source_marks.clone();
            let on_project_changed = on_project_changed.clone();
            let window_weak = window_weak.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                let rx_opt = ltc_rx_for_timer.borrow();
                if let Some(ref rx) = *rx_opt {
                    if let Ok(result) = rx.try_recv() {
                        drop(rx_opt);
                        ltc_rx_for_timer.borrow_mut().take();
                        match result {
                            Ok(prepared) => {
                                let status = {
                                    let mut proj = project.borrow_mut();
                                    let mut lib = library.borrow_mut();
                                    let mut marks = source_marks.borrow_mut();
                                    let applied = apply_prepared_ltc_conversion(
                                        &mut proj,
                                        &mut lib,
                                        Some(&mut *marks),
                                        prepared,
                                    );
                                    format_ltc_conversion_status(&applied)
                                };
                                on_project_changed();
                                if let Some(win) = window_weak.upgrade() {
                                    flash_window_status_title(&win, &project, &status);
                                }
                            }
                            Err(error) => {
                                log::warn!("ltc conversion failed: {error}");
                                if let Some(win) = window_weak.upgrade() {
                                    flash_window_status_title(
                                        &win,
                                        &project,
                                        &format!("LTC conversion failed: {error}"),
                                    );
                                }
                            }
                        }
                    }
                }
                glib::ControlFlow::Continue
            });
        }
        timeline_state.borrow_mut().on_convert_ltc_to_timecode = Some(Rc::new(
            move |clip_id: String,
                  selection: crate::media::ltc::LtcChannelSelection,
                  frame_rate_override: Option<FrameRate>| {
                if ltc_rx.borrow().is_some() {
                    return;
                }
                let context = {
                    let proj = project.borrow();
                    let lib = library.borrow();
                    resolve_ltc_conversion_context(&proj, &lib, &clip_id, frame_rate_override)
                };
                let context = match context {
                    Ok(context) => context,
                    Err(message) => {
                        if let Some(win) = window_weak.upgrade() {
                            flash_window_status_title(
                                &win,
                                &project,
                                &format!("LTC conversion failed: {message}"),
                            );
                        }
                        return;
                    }
                };
                if let Some(win) = window_weak.upgrade() {
                    let title = project.borrow().title.clone();
                    win.set_title(Some(&format!(
                        "UltimateSlice — {title} (Converting LTC to timecode...)"
                    )));
                }
                let (tx, rx) = std::sync::mpsc::channel();
                *ltc_rx.borrow_mut() = Some(rx);
                std::thread::spawn(move || {
                    let result = crate::media::ltc::decode_ltc_from_clip(
                        &context.source_path,
                        context.source_in,
                        context.source_out,
                        selection,
                        &context.frame_rate,
                    )
                    .map(|decode| PreparedLtcConversion { context, decode });
                    let _ = tx.send(result);
                });
            },
        ));
    }

    {
        let project = project.clone();
        let timeline_panel_cell = timeline_panel_cell.clone();
        let window_weak = window.downgrade();
        timeline_state.borrow_mut().on_music_generation_status =
            Some(Rc::new(move |message: String| {
                if let Some(win) = window_weak.upgrade() {
                    flash_window_status_title(&win, &project, &message);
                }
                if let Some(ref w) = *timeline_panel_cell.borrow() {
                    w.queue_draw();
                }
            }));
    }

    // Wire on_generate_music — opens a dialog to generate music via MusicGen AI.
    {
        let music_gen_cache = music_gen_cache.clone();
        let project = project.clone();
        let timeline_state_for_music = timeline_state.clone();
        let timeline_panel_cell = timeline_panel_cell.clone();
        let window_weak = window.downgrade();
        timeline_state.borrow_mut().on_generate_music = Some(Rc::new(
            move |target: MusicGenerationTarget| {
                let win = match window_weak.upgrade() {
                    Some(w) => w,
                    None => return,
                };
                if !music_gen_cache.borrow().is_available() {
                    let dialog = gtk::Dialog::builder()
                        .title("Generate Music")
                        .default_width(360)
                        .modal(true)
                        .transient_for(&win)
                        .build();
                    dialog.add_button("OK", gtk::ResponseType::Accept);
                    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
                    body.set_margin_start(16);
                    body.set_margin_end(16);
                    body.set_margin_top(16);
                    body.set_margin_bottom(16);
                    let label = gtk::Label::new(Some(
                        "MusicGen ONNX models not found.\n\n\
                         Download musicgen-small models from Hugging Face\n\
                         (Xenova/musicgen-small) and place them in:\n\n\
                         ~/.local/share/ultimateslice/models/musicgen-small/\n\n\
                         Required files: text_encoder.onnx,\n\
                         decoder_model_merged.onnx, encodec_decode.onnx,\n\
                         tokenizer.json",
                    ));
                    label.set_wrap(true);
                    body.append(&label);
                    dialog.content_area().append(&body);
                    dialog.connect_response(|d, _| d.close());
                    dialog.present();
                    return;
                }

                let fixed_duration_ns = target.requested_duration_ns();
                let fixed_duration_secs = fixed_duration_ns.map(|ns| ns as f64 / 1_000_000_000.0);
                let dialog_title = if fixed_duration_ns.is_some() {
                    "Generate Music Region"
                } else {
                    "Generate Music"
                };
                let dialog = gtk::Dialog::builder()
                    .title(dialog_title)
                    .default_width(400)
                    .modal(true)
                    .transient_for(&win)
                    .build();
                dialog.add_button("Cancel", gtk::ResponseType::Cancel);
                dialog.add_button("Generate", gtk::ResponseType::Accept);

                let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
                body.set_margin_start(16);
                body.set_margin_end(16);
                body.set_margin_top(16);
                body.set_margin_bottom(16);

                let prompt_label = gtk::Label::new(Some("Describe the music to generate:"));
                prompt_label.set_halign(gtk::Align::Start);
                let prompt_entry = gtk::TextView::new();
                prompt_entry.set_wrap_mode(gtk::WrapMode::Word);
                prompt_entry.set_height_request(80);
                let prompt_scroll = gtk::ScrolledWindow::builder()
                    .child(&prompt_entry)
                    .min_content_height(80)
                    .build();

                let dur_label = gtk::Label::new(Some("Duration (seconds):"));
                dur_label.set_halign(gtk::Align::Start);
                let dur_spin = gtk::SpinButton::with_range(1.0, 30.0, 1.0);
                dur_spin.set_digits(0);
                dur_spin.set_value(10.0);
                dur_spin.set_halign(gtk::Align::Start);
                dur_spin.set_hexpand(false);

                let hint = gtk::Label::new(Some(
                    "Examples: \"upbeat jazz piano\", \"calm ambient synth\",\n\
                     \"energetic rock drums and guitar\"",
                ));
                hint.set_halign(gtk::Align::Start);
                hint.add_css_class("dim-label");

                body.append(&prompt_label);
                body.append(&prompt_scroll);
                if let (Some(duration_ns), Some(end_ns)) =
                    (fixed_duration_ns, target.timeline_end_ns)
                {
                    let region_summary = gtk::Label::new(Some(&format!(
                        "Selected region: {} - {} ({:.1}s)",
                        format_source_keyword_time(target.timeline_start_ns),
                        format_source_keyword_time(end_ns),
                        duration_ns as f64 / 1_000_000_000.0
                    )));
                    region_summary.set_halign(gtk::Align::Start);
                    region_summary.set_wrap(true);
                    region_summary.add_css_class("dim-label");
                    body.append(&region_summary);

                    let fixed_duration_label =
                        gtk::Label::new(Some("Duration is fixed by the selected region."));
                    fixed_duration_label.set_halign(gtk::Align::Start);
                    fixed_duration_label.add_css_class("dim-label");
                    body.append(&fixed_duration_label);
                } else {
                    body.append(&dur_label);
                    body.append(&dur_spin);
                }
                body.append(&hint);

                // ── Reference audio (optional) ─────────────────────────
                // Lets the user point at an existing audio/video clip; we
                // analyze BPM/key/brightness/dynamics and append the result
                // as a natural-language hint to the MusicGen prompt. The
                // model itself is unchanged — this is purely text augmentation.
                let ref_separator = gtk::Separator::new(gtk::Orientation::Horizontal);
                ref_separator.set_margin_top(8);
                ref_separator.set_margin_bottom(4);
                body.append(&ref_separator);

                let ref_label = gtk::Label::new(Some("Reference audio (optional):"));
                ref_label.set_halign(gtk::Align::Start);
                body.append(&ref_label);

                let ref_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                let choose_ref_btn = gtk::Button::with_label("Choose Reference Audio…");
                let clear_ref_btn = gtk::Button::with_label("Clear");
                clear_ref_btn.set_visible(false);
                let ref_path_label = gtk::Label::new(Some("None"));
                ref_path_label.set_halign(gtk::Align::Start);
                ref_path_label.set_hexpand(true);
                ref_path_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                ref_path_label.set_max_width_chars(32);
                ref_path_label.add_css_class("dim-label");
                ref_row.append(&choose_ref_btn);
                ref_row.append(&clear_ref_btn);
                ref_row.append(&ref_path_label);
                body.append(&ref_row);

                let ref_status_label = gtk::Label::new(None);
                ref_status_label.set_halign(gtk::Align::Start);
                ref_status_label.set_wrap(true);
                ref_status_label.add_css_class("dim-label");
                body.append(&ref_status_label);

                let hints_label = gtk::Label::new(Some("Style hints (appended to prompt):"));
                hints_label.set_halign(gtk::Align::Start);
                body.append(&hints_label);
                let hints_entry = gtk::Entry::new();
                hints_entry.set_placeholder_text(Some(
                    "e.g. around 120 BPM, in the key of C major, bright timbre",
                ));
                body.append(&hints_entry);

                dialog.content_area().append(&body);

                let chosen_ref_path: Rc<RefCell<Option<std::path::PathBuf>>> =
                    Rc::new(RefCell::new(None));
                let analysis_generation: Rc<Cell<u64>> = Rc::new(Cell::new(0));

                // Choose Reference Audio button — opens a file picker, then
                // kicks off background analysis on a worker thread and polls
                // the result via a glib timeout so we never block the UI.
                {
                    let chosen_ref_path = chosen_ref_path.clone();
                    let analysis_generation = analysis_generation.clone();
                    let ref_path_label_inner = ref_path_label.clone();
                    let ref_status_label_inner = ref_status_label.clone();
                    let hints_entry_inner = hints_entry.clone();
                    let clear_ref_btn_inner = clear_ref_btn.clone();
                    let win_for_picker = win.clone();
                    choose_ref_btn.connect_clicked(move |_| {
                        let file_dialog = gtk::FileDialog::new();
                        file_dialog.set_title("Choose Reference Audio");
                        let filter = gtk::FileFilter::new();
                        filter.add_mime_type("audio/*");
                        filter.add_mime_type("video/*");
                        filter.set_name(Some("Audio / Video"));
                        let filters = gio::ListStore::new::<gtk::FileFilter>();
                        filters.append(&filter);
                        file_dialog.set_filters(Some(&filters));

                        let chosen_ref_path = chosen_ref_path.clone();
                        let analysis_generation = analysis_generation.clone();
                        let ref_path_label = ref_path_label_inner.clone();
                        let ref_status_label = ref_status_label_inner.clone();
                        let hints_entry = hints_entry_inner.clone();
                        let clear_ref_btn = clear_ref_btn_inner.clone();
                        file_dialog.open(
                            Some(&win_for_picker),
                            gio::Cancellable::NONE,
                            move |res| {
                                let file = match res {
                                    Ok(f) => f,
                                    Err(_) => return,
                                };
                                let path = match file.path() {
                                    Some(p) => p,
                                    None => return,
                                };
                                let basename = path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("(unknown)")
                                    .to_string();
                                ref_path_label.set_text(&basename);
                                ref_path_label.set_tooltip_text(Some(&path.to_string_lossy()));
                                *chosen_ref_path.borrow_mut() = Some(path.clone());
                                clear_ref_btn.set_visible(true);
                                ref_status_label.set_text("Analyzing reference…");

                                // Generation guard: bumping invalidates any
                                // in-flight result so a stale analysis from
                                // a previously-picked file cannot overwrite
                                // the current selection.
                                let gen_id =
                                    analysis_generation.get().wrapping_add(1);
                                analysis_generation.set(gen_id);

                                let path_str = path.to_string_lossy().to_string();
                                let (tx, rx) = std::sync::mpsc::sync_channel::<
                                    Result<
                                        crate::media::audio_features::AudioFeatures,
                                        crate::media::audio_features::AudioFeaturesError,
                                    >,
                                >(1);
                                std::thread::spawn(move || {
                                    let r = crate::media::audio_features::analyze_audio_file(
                                        &path_str, 0, u64::MAX,
                                    );
                                    let _ = tx.send(r);
                                });

                                let analysis_generation_poll = analysis_generation.clone();
                                let ref_status_label_poll = ref_status_label.clone();
                                let hints_entry_poll = hints_entry.clone();
                                glib::timeout_add_local(
                                    std::time::Duration::from_millis(150),
                                    move || match rx.try_recv() {
                                        Ok(Ok(features)) => {
                                            if analysis_generation_poll.get() != gen_id {
                                                return glib::ControlFlow::Break;
                                            }
                                            let hint =
                                                crate::media::audio_features::features_to_prompt_hint(
                                                    &features,
                                                );
                                            ref_status_label_poll
                                                .set_text(&format!("Detected: {hint}"));
                                            hints_entry_poll.set_text(&hint);
                                            glib::ControlFlow::Break
                                        }
                                        Ok(Err(e)) => {
                                            if analysis_generation_poll.get() != gen_id {
                                                return glib::ControlFlow::Break;
                                            }
                                            ref_status_label_poll
                                                .set_text(&format!(
                                                    "Reference analysis failed: {e}"
                                                ));
                                            glib::ControlFlow::Break
                                        }
                                        Err(std::sync::mpsc::TryRecvError::Empty) => {
                                            glib::ControlFlow::Continue
                                        }
                                        Err(_) => glib::ControlFlow::Break,
                                    },
                                );
                            },
                        );
                    });
                }

                // Clear button — drops the reference, blanks the hints
                // entry, and bumps the generation so any in-flight analysis
                // result will be ignored.
                {
                    let chosen_ref_path = chosen_ref_path.clone();
                    let analysis_generation = analysis_generation.clone();
                    let ref_path_label_inner = ref_path_label.clone();
                    let ref_status_label_inner = ref_status_label.clone();
                    let hints_entry_inner = hints_entry.clone();
                    let clear_ref_btn_inner = clear_ref_btn.clone();
                    clear_ref_btn.connect_clicked(move |_| {
                        *chosen_ref_path.borrow_mut() = None;
                        ref_path_label_inner.set_text("None");
                        ref_path_label_inner.set_tooltip_text(None);
                        ref_status_label_inner.set_text("");
                        hints_entry_inner.set_text("");
                        clear_ref_btn_inner.set_visible(false);
                        analysis_generation.set(analysis_generation.get().wrapping_add(1));
                    });
                }

                let music_gen_cache = music_gen_cache.clone();
                let timeline_state = timeline_state_for_music.clone();
                let timeline_panel_cell = timeline_panel_cell.clone();
                let _project = project.clone();
                let chosen_ref_path_for_response = chosen_ref_path.clone();
                let hints_entry_for_response = hints_entry.clone();
                dialog.connect_response(move |d, resp| {
                    if resp == gtk::ResponseType::Accept {
                        let buffer = prompt_entry.buffer();
                        let prompt = buffer
                            .text(&buffer.start_iter(), &buffer.end_iter(), false)
                            .to_string();
                        if prompt.trim().is_empty() {
                            d.close();
                            return;
                        }
                        // Append the (possibly user-edited) style hints to
                        // the prompt before sending it to MusicGen.
                        let hints = hints_entry_for_response.text().to_string();
                        let final_prompt = if hints.trim().is_empty() {
                            prompt.trim().to_string()
                        } else {
                            format!("{}, {}", prompt.trim(), hints.trim())
                        };
                        let reference_audio_path = chosen_ref_path_for_response.borrow().clone();
                        let duration_secs = fixed_duration_secs.unwrap_or_else(|| dur_spin.value());
                        let requested_end_ns = target.timeline_end_ns.unwrap_or_else(|| {
                            target
                                .timeline_start_ns
                                .saturating_add((duration_secs * 1_000_000_000.0).round() as u64)
                        });
                        let job_id = uuid::Uuid::new_v4().to_string();
                        {
                            let mut st = timeline_state.borrow_mut();
                            st.add_pending_music_generation_overlay(
                                job_id.clone(),
                                target.track_id.clone(),
                                target.timeline_start_ns,
                                requested_end_ns,
                            );
                        }
                        if let Some(ref w) = *timeline_panel_cell.borrow() {
                            w.queue_draw();
                        }
                        let job = crate::media::music_gen::MusicGenJob {
                            job_id,
                            prompt: final_prompt,
                            duration_secs,
                            output_path: std::path::PathBuf::new(),
                            track_id: target.track_id.clone(),
                            timeline_start_ns: target.timeline_start_ns,
                            reference_audio_path,
                        };
                        music_gen_cache.borrow_mut().request(job);
                    }
                    d.close();
                });

                dialog.present();
            },
        ));
    }

    // Wire on_match_color — triggers the inspector Match Color button via keyboard shortcut.
    {
        let match_btn = inspector_view.match_color_btn.clone();
        timeline_state.borrow_mut().on_match_color = Some(Rc::new(move || {
            match_btn.emit_clicked();
        }));
    }

    // ── Build program monitor ──────────────────────────────────────────────
    let prog_monitor_host = gtk::Box::new(Orientation::Vertical, 0);
    prog_monitor_host.set_hexpand(true);
    prog_monitor_host.set_vexpand(true);
    let monitor_state = Rc::new(RefCell::new(crate::ui_state::load_program_monitor_state()));
    let popout_window_cell: Rc<RefCell<Option<ApplicationWindow>>> = Rc::new(RefCell::new(None));
    let monitor_popped = Rc::new(Cell::new(false));
    // Loudness Radar popover is built inside the build_program_monitor
    // call (so its button can be passed as `extra_header_button`) but the
    // view is cached here so poll-timer drains + callback wiring can
    // reach it later in this function.
    let loudness_popover_view_cell: Rc<
        RefCell<Option<Rc<crate::ui::loudness_popover::LoudnessPopoverView>>>,
    > = Rc::new(RefCell::new(None));
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
        prog_false_color_setter,
        prog_zebra_setter,
        prog_frame_updater,
        prog_subtitle_text_setter,
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
                        if let Some(clip) = proj.clip_mut(clip_id) {
                            clip.scale = sc;
                            clip.position_x = px;
                            clip.position_y = py;
                        }
                        proj.dirty = true;
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
                    let use_paused_refresh = {
                        let proj = project.borrow();
                        selected_clip_is_static_image(&proj, selected.as_deref())
                    };
                    let mut pp = prog_player.borrow_mut();
                    if use_paused_refresh {
                        if let Some(ref clip_id) = selected {
                            pp.update_transform_for_clip(
                                clip_id, cl, crv, ct, cb, rot, fh, fv, sc, px, py,
                            );
                        }
                    } else {
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
                    }
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
                        if let Some(clip) = proj.clip_mut(clip_id) {
                            clip.rotate = rot;
                        }
                        proj.dirty = true;
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
                    let is_adjustment = {
                        let proj = project.borrow();
                        selected_clip_is_adjustment(&proj, selected.as_deref())
                    };
                    let use_paused_refresh = {
                        let proj = project.borrow();
                        selected_clip_is_static_image(&proj, selected.as_deref())
                    };
                    if !is_adjustment {
                        player.borrow().set_transform(cl, cr, ct, cb, rot, fh, fv);
                    }
                    let mut pp = prog_player.borrow_mut();
                    if use_paused_refresh {
                        if let Some(ref clip_id) = selected {
                            pp.update_transform_for_clip(
                                clip_id, cl, cr, ct, cb, rot, fh, fv, sc, px, py,
                            );
                        }
                    } else {
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
                    }
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
                        if let Some(clip) = proj.clip_mut(clip_id) {
                            clip.crop_left = cl;
                            clip.crop_right = cr;
                            clip.crop_top = ct;
                            clip.crop_bottom = cb;
                        }
                        proj.dirty = true;
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
                    let is_adjustment = {
                        let proj = project.borrow();
                        selected_clip_is_adjustment(&proj, selected.as_deref())
                    };
                    let use_paused_refresh = {
                        let proj = project.borrow();
                        selected_clip_is_static_image(&proj, selected.as_deref())
                    };
                    if !is_adjustment {
                        player.borrow().set_transform(cl, cr, ct, cb, rot, fh, fv);
                    }
                    let mut pp = prog_player.borrow_mut();
                    if use_paused_refresh {
                        if let Some(ref clip_id) = selected {
                            pp.update_transform_for_clip(
                                clip_id, cl, cr, ct, cb, rot, fh, fv, sc, px, py,
                            );
                        }
                    } else {
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
                    }
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
                let transform_overlay_cell = transform_overlay_cell.clone();
                move || {
                    prog_player.borrow_mut().exit_transform_live_mode();
                    if transform_overlay_cell
                        .borrow()
                        .as_ref()
                        .map(|overlay| overlay.is_tracking_editing())
                        .unwrap_or(false)
                    {
                        return;
                    }
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
                                if let Some(clip) = proj.clip_mut(&clip_id) {
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
                                }
                            }
                            if changed {
                                on_project_changed();
                            }
                        }
                    }
                }
            },
            // on_mask_path_change: live update during drag
            {
                let project = project.clone();
                let timeline_state = timeline_state.clone();
                let prog_player = prog_player.clone();
                move |points: &[crate::model::clip::BezierPoint]| {
                    let selected = timeline_state.borrow().selected_clip_id.clone();
                    if let Some(ref clip_id) = selected {
                        let mut proj = project.borrow_mut();
                        if let Some(clip) = proj.clip_mut(clip_id) {
                            if let Some(ref mut mask) = clip.masks.first_mut() {
                                mask.path = Some(crate::model::clip::MaskPath {
                                    points: points.to_vec(),
                                });
                            }
                        }
                        proj.dirty = true;
                        drop(proj);
                        // Push live mask update to preview pipeline.
                        let masks = {
                            let proj = project.borrow();
                            proj.clip_ref(clip_id)
                                .map(|c| c.masks.clone())
                                .unwrap_or_default()
                        };
                        prog_player
                            .borrow_mut()
                            .update_masks_for_clip(clip_id, &masks);
                    }
                }
            },
            // on_mask_path_dbl_click: add/delete point (commits as undo snapshot)
            {
                let project = project.clone();
                let timeline_state = timeline_state.clone();
                let prog_player = prog_player.clone();
                let on_project_changed = on_project_changed.clone();
                move |points: &[crate::model::clip::BezierPoint]| {
                    let selected = timeline_state.borrow().selected_clip_id.clone();
                    if let Some(ref clip_id) = selected {
                        {
                            let mut proj = project.borrow_mut();
                            if let Some(clip) = proj.clip_mut(clip_id) {
                                if let Some(ref mut mask) = clip.masks.first_mut() {
                                    mask.path = Some(crate::model::clip::MaskPath {
                                        points: points.to_vec(),
                                    });
                                }
                            }
                            proj.dirty = true;
                        }
                        // Push to preview + trigger full project change for inspector sync.
                        let masks = {
                            let proj = project.borrow();
                            proj.clip_ref(clip_id)
                                .map(|c| c.masks.clone())
                                .unwrap_or_default()
                        };
                        prog_player
                            .borrow_mut()
                            .update_masks_for_clip(clip_id, &masks);
                        on_project_changed();
                    }
                }
            },
            {
                let inspector_view = inspector_view.clone();
                let project = project.clone();
                let tracking_status_by_clip = tracking_status_by_clip.clone();
                let sync_tracking_controls = sync_tracking_controls.clone();
                let window_weak = window_weak.clone();
                move |center_x, center_y, width, height| {
                    let clip_id = inspector_view.selected_clip_id.borrow().clone();
                    let tracker_id = inspector_view.current_motion_tracker_id();
                    let (Some(clip_id), Some(tracker_id)) = (clip_id, tracker_id) else {
                        return;
                    };
                    {
                        let mut proj = project.borrow_mut();
                        if let Some(tracker) = proj
                            .clip_mut(&clip_id)
                            .and_then(|clip| clip.motion_tracker_mut(&tracker_id))
                        {
                            tracker.analysis_region.center_x = center_x;
                            tracker.analysis_region.center_y = center_y;
                            tracker.analysis_region.width = width;
                            tracker.analysis_region.height = height;
                            tracker.samples.clear();
                            proj.dirty = true;
                        }
                        if let Some(win) = window_weak.upgrade() {
                            win.set_title(Some(&format!("UltimateSlice — {} •", proj.title)));
                        }
                    }
                    tracking_status_by_clip.borrow_mut().insert(
                        clip_id.clone(),
                        ("Region changed. Run tracking again.".to_string(), false),
                    );
                    {
                        *inspector_view.updating.borrow_mut() = true;
                        inspector_view.tracking_center_x_slider.set_value(center_x);
                        inspector_view.tracking_center_y_slider.set_value(center_y);
                        inspector_view.tracking_width_slider.set_value(width);
                        inspector_view.tracking_height_slider.set_value(height);
                        *inspector_view.updating.borrow_mut() = false;
                    }
                    sync_tracking_controls();
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
        sync_tracking_controls();

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
                    // When inside a compound deep-dive, set the root playhead
                    // to the compound's start so editing_playhead_ns() maps to 0.
                    let root_pos = {
                        let st = ts.borrow();
                        st.root_playhead_from_internal_ns(0)
                    };
                    ts.borrow_mut().playhead_ns = root_pos;
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
            monitor_state.borrow().show_false_color,
            {
                let monitor_state = monitor_state.clone();
                move |show| {
                    let mut state = monitor_state.borrow_mut();
                    if state.show_false_color != show {
                        state.show_false_color = show;
                        crate::ui_state::save_program_monitor_state(&state);
                    }
                }
            },
            monitor_state.borrow().show_zebra,
            monitor_state.borrow().zebra_threshold,
            {
                let monitor_state = monitor_state.clone();
                move |show, threshold| {
                    let mut state = monitor_state.borrow_mut();
                    if state.show_zebra != show || (state.zebra_threshold - threshold).abs() > 1e-6
                    {
                        state.show_zebra = show;
                        state.zebra_threshold = threshold;
                        crate::ui_state::save_program_monitor_state(&state);
                    }
                }
            },
            // The Loudness Radar popover button is placed next to the
            // Scopes toggle below `prog_monitor_host`, not in the
            // Program Monitor header, so we pass None here.
            None,
        )
    };

    // ── Voiceover countdown overlay on the program monitor ────────────────
    let countdown_overlay_da = gtk4::DrawingArea::new();
    countdown_overlay_da.set_hexpand(true);
    countdown_overlay_da.set_vexpand(true);
    countdown_overlay_da.set_halign(gtk4::Align::Fill);
    countdown_overlay_da.set_valign(gtk4::Align::Fill);
    countdown_overlay_da.set_can_target(false);
    {
        let cv = voiceover_countdown.clone();
        countdown_overlay_da.set_draw_func(move |_da, cr, width, height| {
            let val = cv.get();
            if val == 0 || width <= 0 || height <= 0 {
                return;
            }
            let w = width as f64;
            let h = height as f64;
            // Semi-transparent dark background.
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
            cr.rectangle(0.0, 0.0, w, h);
            let _ = cr.fill();
            // Large white countdown number.
            let text = val.to_string();
            let font_size = h * 0.35;
            cr.set_font_size(font_size);
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
            if let Ok(extents) = cr.text_extents(&text) {
                let x = (w - extents.width()) * 0.5 - extents.x_bearing();
                let y = (h - extents.height()) * 0.5 - extents.y_bearing();
                cr.move_to(x, y);
                let _ = cr.show_text(&text);
                // Hint text below.
                cr.set_font_size(font_size * 0.18);
                let hint = "Recording starts in\u{2026}";
                if let Ok(hint_ext) = cr.text_extents(hint) {
                    cr.move_to(
                        (w - hint_ext.width()) * 0.5 - hint_ext.x_bearing(),
                        y + font_size * 0.3,
                    );
                    let _ = cr.show_text(hint);
                }
            }
        });
    }
    // Wrap the program monitor widget in an overlay so the countdown draws on top.
    let prog_monitor_overlay = gtk4::Overlay::new();
    prog_monitor_overlay.set_child(Some(&prog_monitor_widget));
    prog_monitor_overlay.add_overlay(&countdown_overlay_da);

    // ── Loudness Radar popover callbacks ──────────────────────────────
    //
    // The popover view was constructed inside the build_program_monitor
    // call so its button could be passed as `extra_header_button`. Now
    // that the project + preferences + prog_player refs are all stable,
    // we can wire up the three actions.
    //
    // `on_analyze` spawns a background thread that renders the project
    // audio to a temp file and runs ebur128; the result is drained on a
    // 100 ms poll timer (mirrors the `on_normalize_audio` pattern).
    // `on_normalize` and `on_reset_gain` push a
    // `SetProjectMasterGainCommand` through the undo history and update
    // the preview in place via `prog_player.set_master_gain_db`.
    if let Some(view) = loudness_popover_view_cell.borrow().clone() {
        // Channel for background-thread analysis results.
        let (loudness_tx, loudness_rx) =
            std::sync::mpsc::channel::<Result<crate::media::export::LoudnessReport, String>>();
        let loudness_tx = Rc::new(RefCell::new(Some(loudness_tx)));
        let loudness_rx = Rc::new(RefCell::new(Some(loudness_rx)));

        // Poll the channel every 100 ms and push results into the popover.
        // When a result arrives we also restore the window title to its
        // idle form so the "Analyzing project loudness…" flash goes away.
        {
            let view_poll = view.clone();
            let rx = loudness_rx.clone();
            let project_poll = project.clone();
            let window_weak = window_weak.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                if let Some(ref r) = *rx.borrow() {
                    while let Ok(result) = r.try_recv() {
                        match result {
                            Ok(report) => {
                                let current_gain = project_poll.borrow().master_gain_db;
                                view_poll.set_report(report, current_gain);
                            }
                            Err(msg) => view_poll.set_analyze_error(&msg),
                        }
                        if let Some(win) = window_weak.upgrade() {
                            let proj = project_poll.borrow();
                            win.set_title(Some(&format!(
                                "UltimateSlice \u{2014} {} \u{2022}",
                                proj.title
                            )));
                        }
                    }
                }
                glib::ControlFlow::Continue
            });
        }

        // Analyze button: set UI state, flash window title, spawn
        // background thread. The title is restored by the poll-drain
        // above when the result (or error) comes back.
        {
            let project = project.clone();
            let view_click = view.clone();
            let tx_rc = loudness_tx.clone();
            let window_weak = window_weak.clone();
            view.analyze_btn.connect_clicked(move |_btn| {
                if view_click.analyzing.get() {
                    return;
                }
                view_click.set_analyzing();
                if let Some(win) = window_weak.upgrade() {
                    let proj = project.borrow();
                    win.set_title(Some(&format!(
                        "UltimateSlice \u{2014} {} (Analyzing project loudness\u{2026})",
                        proj.title
                    )));
                }
                // Clone the project snapshot so the background thread
                // doesn't need to cross the Rc boundary.
                let project_snapshot = project.borrow().clone();
                let tx_clone = tx_rc.borrow().as_ref().cloned();
                if let Some(tx_send) = tx_clone {
                    std::thread::spawn(move || {
                        let result = crate::media::export::analyze_project_loudness(
                            &project_snapshot,
                        )
                        .map_err(|e| e.to_string());
                        let _ = tx_send.send(result);
                    });
                }
            });
        }

        // Normalize button: compute delta from last report + current
        // target, apply as SetProjectMasterGainCommand via timeline_state
        // history. The action is effectively instant so we confirm
        // success with a terse status message in the popover header
        // (cleared on next Analyze).
        {
            let project = project.clone();
            let timeline_state_c = timeline_state.clone();
            let prog_player = prog_player.clone();
            let view_click = view.clone();
            let on_project_changed = on_project_changed.clone();
            view.normalize_btn.connect_clicked(move |_btn| {
                let Some(report) = view_click.last_report.borrow().clone() else {
                    return;
                };
                let target = view_click.current_target_lufs();
                let old_db = project.borrow().master_gain_db;
                let delta = target - report.integrated_lufs;
                let new_db = (old_db + delta).clamp(-24.0, 24.0);
                if (new_db - old_db).abs() < 1e-6 {
                    view_click
                        .status_label
                        .set_text("Already at target — nothing to apply");
                    return;
                }
                let applied_db = new_db - old_db;
                let cmd: Box<dyn crate::undo::EditCommand> =
                    Box::new(crate::undo::SetProjectMasterGainCommand { old_db, new_db });
                {
                    let mut proj = project.borrow_mut();
                    timeline_state_c
                        .borrow_mut()
                        .history
                        .execute(cmd, &mut proj);
                }
                prog_player.borrow_mut().set_master_gain_db(new_db);
                view_click.set_current_gain(new_db);
                view_click.status_label.set_text(&format!(
                    "Normalized ({:+.2} dB applied)",
                    applied_db
                ));
                on_project_changed();
            });
        }

        // Reset Gain button: snap back to 0.0.
        {
            let project = project.clone();
            let timeline_state_c = timeline_state.clone();
            let prog_player = prog_player.clone();
            let view_click = view.clone();
            let on_project_changed = on_project_changed.clone();
            view.reset_gain_btn.connect_clicked(move |_btn| {
                let old_db = project.borrow().master_gain_db;
                if old_db.abs() < 1e-6 {
                    view_click.status_label.set_text("Gain already 0 dB");
                    return;
                }
                let cmd: Box<dyn crate::undo::EditCommand> = Box::new(
                    crate::undo::SetProjectMasterGainCommand {
                        old_db,
                        new_db: 0.0,
                    },
                );
                {
                    let mut proj = project.borrow_mut();
                    timeline_state_c
                        .borrow_mut()
                        .history
                        .execute(cmd, &mut proj);
                }
                prog_player.borrow_mut().set_master_gain_db(0.0);
                view_click.set_current_gain(0.0);
                view_click
                    .status_label
                    .set_text(&format!("Gain reset ({:+.2} dB → 0.00 dB)", old_db));
                on_project_changed();
            });
        }
    }
    // Poll to redraw the countdown overlay when active.
    {
        let cv = voiceover_countdown.clone();
        let da = countdown_overlay_da.clone();
        let mut last_val = 0u32;
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let v = cv.get();
            if v != last_val {
                last_val = v;
                da.queue_draw();
            }
            glib::ControlFlow::Continue
        });
    }

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
    docked_scopes_paned.set_start_child(Some(&prog_monitor_overlay));
    docked_scopes_paned.set_end_child(Option::<&gtk::Widget>::None);
    {
        let state = monitor_state.borrow().clone();
        docked_scopes_paned.set_position(state.docked_split_pos.max(160));
    }
    {
        let monitor_state = monitor_state.clone();
        let monitor_popped = monitor_popped.clone();
        let workspace_layouts_applying = workspace_layouts_applying.clone();
        let sync_workspace_layout_state = sync_workspace_layout_state.clone();
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
            if !workspace_layouts_applying.get() {
                sync_workspace_layout_state();
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
        let keyframe_editor_poll = keyframe_editor_cell.clone();
        let transcript_panel_poll = transcript_panel_cell.clone();
        let timeline_state_poll = timeline_state.clone();
        let inspector_view_poll = inspector_view.clone();
        let prog_frame_updater_poll = prog_frame_updater.clone();
        let prog_subtitle_setter_poll = prog_subtitle_text_setter.clone();
        let monitor_state_poll = monitor_state.clone();
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
                        let variants = {
                            let proj = project.borrow();
                            collect_unique_proxy_variants(&proj, desired_scale)
                        };
                        {
                            let mut cache = proxy_cache.borrow_mut();
                            request_proxy_variants(&mut cache, &variants);
                        }
                        let paths = proxy_cache.borrow().proxies.clone();
                        player.update_proxy_paths(paths);
                    } else if !desired_proxy_enabled
                        && preview_luts
                        && now_us - last_proxy_refresh_us_c.get() >= 1_000_000
                    {
                        last_proxy_refresh_us_c.set(now_us);
                        let variants = {
                            let proj = project.borrow();
                            collect_unique_preview_lut_proxy_variants(&proj)
                        };
                        {
                            let mut cache = proxy_cache.borrow_mut();
                            request_proxy_variants(&mut cache, &variants);
                        }
                        let paths = proxy_cache.borrow().proxies.clone();
                        player.update_proxy_paths(paths);
                    }
                }
                player.poll();
                let (oa, ob) = player.transition_opacities();
                let sf = if scopes_rev.reveals_child()
                    || monitor_state_poll.borrow().show_false_color
                    || monitor_state_poll.borrow().show_zebra
                {
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
                prog_frame_updater_poll(frame.clone());
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
                // When inside a compound deep-dive, the program player reports
                // positions in compound-internal coordinates.  Translate back
                // to root-timeline coordinates so playhead_ns stays in root
                // space (editing_playhead_ns handles the forward translation
                // when drawing).
                let root_pos = {
                    let st = ts.borrow();
                    if st.compound_nav_stack.is_empty() {
                        pos_ns
                    } else {
                        st.root_playhead_from_internal_ns(pos_ns)
                    }
                };
                pos_label.set_text(&program_monitor::format_timecode(pos_ns, &frame_rate));
                ts.borrow_mut().playhead_ns = root_pos;
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
                        let pp_ref = pp.borrow();
                        sync_transform_overlay_to_playhead_resolved(
                            to,
                            &proj,
                            &pp_ref,
                            selected.as_deref(),
                            pos_ns,
                        );
                        let (ix, iy) = pp_ref.content_inset_for_clip(selected.as_deref());
                        to.set_content_inset(ix, iy);
                    }
                }
                // Update inspector sliders to reflect keyframe-evaluated values
                // at the new playhead position.
                {
                    let proj = project.borrow();
                    inspector_view_poll.update_keyframed_sliders(&proj, pos_ns);
                }
                if let Some(ref editor) = *keyframe_editor_poll.borrow() {
                    editor.queue_redraw();
                }
                if let Some(ref tp) = *transcript_panel_poll.borrow() {
                    let proj = project.borrow();
                    tp.update_playhead(&proj, pos_ns);
                }
                last_pos_ns_c.set(pos_ns);
            }
            // Update subtitle overlay text for the current playhead position.
            // Runs every poll iteration (not gated on position change) so that
            // subtitle text edits made while paused are reflected immediately
            // and so the overlay re-evaluates when project state changes
            // without waiting for the next playhead movement.
            {
                let proj = project.borrow();
                let mut lines: Vec<crate::ui::program_monitor::SubtitleLine> = Vec::new();
                fn collect_subtitle_lines(
                    tracks: &[crate::model::track::Track],
                    pos_ns: u64,
                    lines: &mut Vec<crate::ui::program_monitor::SubtitleLine>,
                ) {
                    for track in tracks {
                        for clip in &track.clips {
                            // Recurse into compound clips with translated time
                            if let Some(ref inner) = clip.compound_tracks {
                                let clip_end = clip.timeline_start + clip.duration();
                                if pos_ns >= clip.timeline_start && pos_ns < clip_end {
                                    let internal_pos = pos_ns
                                        .saturating_sub(clip.timeline_start)
                                        .saturating_add(clip.source_in);
                                    collect_subtitle_lines(inner, internal_pos, lines);
                                }
                                continue;
                            }
                            if clip.subtitle_segments.is_empty() || !clip.subtitle_visible {
                                continue;
                            }
                            let clip_end = clip.timeline_start + clip.duration();
                            if pos_ns >= clip.timeline_start && pos_ns < clip_end {
                                let local_ns =
                                    ((pos_ns - clip.timeline_start) as f64 * clip.speed) as u64;
                                for seg in &clip.subtitle_segments {
                                    if local_ns >= seg.start_ns && local_ns < seg.end_ns {
                                        let c = clip.subtitle_color;
                                        let oc = clip.subtitle_outline_color;
                                        let bc = clip.subtitle_bg_box_color;
                                        let hc = clip.subtitle_highlight_color;
                                        let base_size =
                                            crate::media::title_font::parse_subtitle_font(
                                                &clip.subtitle_font,
                                            )
                                            .size_points();
                                        let font_desc =
                                            crate::media::title_font::build_preview_subtitle_font_desc(
                                                &clip.subtitle_font,
                                                base_size,
                                            );

                                        // Build word-level display with active word highlighting.
                                        // Fixed groups: divide words into groups of N, show the
                                        // group containing the active word. The group stays on
                                        // screen until its last word finishes, then advances.
                                        let group_size =
                                            (clip.subtitle_word_window_secs as usize).max(2);
                                        let mut word_displays = Vec::new();
                                        if !seg.words.is_empty()
                                            && !clip.subtitle_highlight_flags.is_none()
                                        {
                                            // Find which word is active.
                                            let active_idx = seg.words.iter().position(|w| {
                                                local_ns >= w.start_ns && local_ns < w.end_ns
                                            });
                                            // Determine which fixed group the active word belongs to.
                                            let center = active_idx.unwrap_or(0);
                                            let group_start = (center / group_size) * group_size;
                                            let group_end =
                                                (group_start + group_size).min(seg.words.len());
                                            for (wi, word) in
                                                seg.words[group_start..group_end].iter().enumerate()
                                            {
                                                word_displays.push(crate::ui::program_monitor::SubtitleWordDisplay {
                                                    text: word.text.clone(),
                                                    active: Some(group_start + wi) == active_idx,
                                                });
                                            }
                                        }

                                        lines.push(crate::ui::program_monitor::SubtitleLine {
                                            words: word_displays,
                                            text: seg.text.clone(),
                                            color: crate::ui::colors::rgba_u32_to_f64(c),
                                            highlight_color: crate::ui::colors::rgba_u32_to_f64(hc),
                                            highlight_stroke_color:
                                                crate::ui::colors::rgba_u32_to_f64(
                                                    clip.subtitle_highlight_stroke_color,
                                                ),
                                            highlight_flags: clip.subtitle_highlight_flags,
                                            outline_color: crate::ui::colors::rgba_u32_to_f64(oc),
                                            outline_width: clip.subtitle_outline_width,
                                            bg_box: clip.subtitle_bg_box,
                                            bg_box_color: crate::ui::colors::rgba_u32_to_f64(bc),
                                            font_desc,
                                            position_y: clip.subtitle_position_y,
                                            subtitle_bold: clip.subtitle_bold,
                                            subtitle_italic: clip.subtitle_italic,
                                            subtitle_underline: clip.subtitle_underline,
                                            subtitle_shadow: clip.subtitle_shadow,
                                            subtitle_shadow_color:
                                                crate::ui::colors::rgba_u32_to_f64(
                                                    clip.subtitle_shadow_color,
                                                ),
                                            subtitle_shadow_offset: (
                                                clip.subtitle_shadow_offset_x,
                                                clip.subtitle_shadow_offset_y,
                                            ),
                                            bg_highlight_color: crate::ui::colors::rgba_u32_to_f64(
                                                clip.subtitle_bg_highlight_color,
                                            ),
                                        });
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                // When inside a compound deep-dive, collect subtitles
                // from the internal tracks using the player's compound-
                // internal position (pos_ns) directly.
                // When inside a compound deep-dive, collect subtitles
                // from the internal tracks using the player's compound-
                // internal position (pos_ns) directly.
                let editing_ptr = {
                    let st = ts.borrow();
                    if st.compound_nav_stack.is_empty() {
                        proj.tracks.as_slice() as *const [crate::model::track::Track]
                    } else {
                        st.resolve_editing_tracks(&proj) as *const [crate::model::track::Track]
                    }
                };
                // SAFETY: proj is borrowed immutably for this block.
                let editing: &[crate::model::track::Track] = unsafe { &*editing_ptr };
                collect_subtitle_lines(editing, pos_ns, &mut lines);
                prog_subtitle_setter_poll(lines);
            }
            glib::ControlFlow::Continue
        });
    }

    // Scopes toggle for the docked monitor/scopes split.
    let scopes_btn = gtk::ToggleButton::with_label("▾ Scopes");
    scopes_btn.add_css_class("flat");
    scopes_btn.set_halign(gtk::Align::Start);
    scopes_btn.set_margin_start(4);
    scopes_btn.set_active(monitor_state.borrow().scopes_visible);
    {
        let rev = scopes_revealer.clone();
        let docked_paned = docked_scopes_paned.clone();
        let monitor_state = monitor_state.clone();
        let prog_player_scope = prog_player.clone();
        let workspace_layouts_applying = workspace_layouts_applying.clone();
        let sync_workspace_layout_state = sync_workspace_layout_state.clone();
        scopes_btn.connect_toggled(move |b| {
            let visible = b.is_active();
            prog_player_scope.borrow().set_scope_enabled(visible);
            if visible {
                if docked_paned.end_child().is_none() {
                    docked_paned.set_end_child(Some(&rev));
                }
                {
                    let state = monitor_state.borrow();
                    docked_paned.set_position(state.docked_split_pos.max(160));
                }
                rev.set_reveal_child(true);
            } else {
                let pos = docked_paned.position().max(160);
                rev.set_reveal_child(false);
                docked_paned.set_end_child(Option::<&gtk::Widget>::None);
                {
                    let mut state = monitor_state.borrow_mut();
                    state.docked_split_pos = pos;
                    state.scopes_visible = false;
                    crate::ui_state::save_program_monitor_state(&state);
                }
            }
            if visible {
                let mut state = monitor_state.borrow_mut();
                state.scopes_visible = true;
                crate::ui_state::save_program_monitor_state(&state);
            }
            if !workspace_layouts_applying.get() {
                sync_workspace_layout_state();
            }
        });
    }
    // Build the Loudness Radar popover next to the Scopes toggle. The
    // popover view is cached in `loudness_popover_view_cell` so later
    // wiring (analyze/normalize/reset callbacks + MCP + poll-timer
    // drain) can reach it.
    let loudness_row = gtk::Box::new(Orientation::Horizontal, 4);
    loudness_row.set_halign(gtk::Align::Start);
    loudness_row.set_margin_start(4);
    loudness_row.append(&scopes_btn);
    {
        let view = crate::ui::loudness_popover::build_loudness_popover(
            &preferences_state.borrow(),
            project.borrow().master_gain_db,
        );
        loudness_row.append(&view.button);
        *loudness_popover_view_cell.borrow_mut() = Some(view);
    }
    prog_monitor_host.append(&loudness_row);
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
        let monitor = prog_monitor_overlay.clone();
        let pop_cell = popout_window_cell.clone();
        let popped = monitor_popped.clone();
        let monitor_state = monitor_state.clone();
        let scopes_rev = scopes_revealer.clone();
        let workspace_layouts_applying = workspace_layouts_applying.clone();
        let sync_workspace_layout_state = sync_workspace_layout_state.clone();
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
                let workspace_layouts_applying_c = workspace_layouts_applying.clone();
                let sync_workspace_layout_state_c = sync_workspace_layout_state.clone();
                pop_win.connect_close_request(move |w| {
                    // Release the `monitor_state` borrow before touching
                    // any widget that can fire signals which themselves
                    // re-borrow `monitor_state` — notably
                    // `docked_paned_c.set_start_child()` (via position-
                    // notify) and `sync_workspace_layout_state_c()`
                    // (which reads the monitor snapshot). A double-
                    // borrow inside a GTK4 C trampoline aborts the
                    // process without unwinding — see
                    // docs/ARCHITECTURE.md "GTK4 C trampolines cannot
                    // unwind".
                    {
                        let mut state = monitor_state_c.borrow_mut();
                        state.width = w.width().max(320);
                        state.height = w.height().max(180);
                        state.popped = false;
                        crate::ui_state::save_program_monitor_state(&state);
                    }
                    w.set_child(Option::<&gtk::Widget>::None);
                    if monitor_c.parent().is_none() {
                        docked_paned_c.set_start_child(Some(&monitor_c));
                    }
                    scopes_rev_c.set_vexpand(false);
                    popped_c.set(false);
                    *pop_cell_c.borrow_mut() = None;
                    if !workspace_layouts_applying_c.get() {
                        sync_workspace_layout_state_c();
                    }
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
                if !workspace_layouts_applying.get() {
                    sync_workspace_layout_state();
                }
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
            let media_dur = marks.duration_ns;
            let source_info = SourcePlacementInfo {
                is_audio_only: marks.is_audio_only,
                has_audio: marks.has_audio,
                is_image: marks.is_image,
                is_animated_svg: marks.is_animated_svg,
                source_timecode_base_ns: marks.source_timecode_base_ns,
                audio_channel_mode: marks.audio_channel_mode,
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
                ensure_matching_source_track_exists(&mut proj, source_info);
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
                    let media_dur_opt = if source_info.is_image {
                        if source_info.is_animated_svg {
                            Some(media_dur)
                        } else {
                            None
                        }
                    } else {
                        Some(media_dur)
                    };
                    for (track_idx, clip) in build_source_clips_for_plan(
                        &placement_plan,
                        &path,
                        in_ns,
                        out_ns,
                        timeline_start,
                        source_info.source_timecode_base_ns,
                        source_info.audio_channel_mode,
                        media_dur_opt,
                        source_info.is_animated_svg,
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
            let media_dur = marks.duration_ns;
            let source_info = SourcePlacementInfo {
                is_audio_only: marks.is_audio_only,
                has_audio: marks.has_audio,
                is_image: marks.is_image,
                is_animated_svg: marks.is_animated_svg,
                source_timecode_base_ns: marks.source_timecode_base_ns,
                audio_channel_mode: marks.audio_channel_mode,
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
                ensure_matching_source_track_exists(&mut proj, source_info);
                let placement_plan = build_source_placement_plan_by_track_id(
                    &proj,
                    active_tid.as_deref(),
                    source_info,
                    source_monitor_auto_link_av,
                );
                let mut track_changes: Vec<TrackClipsChange> = Vec::new();
                let magnetic_mode_for_placement =
                    magnetic_mode && !placement_plan.uses_linked_pair();
                let media_dur_opt = if source_info.is_image {
                    if source_info.is_animated_svg {
                        Some(media_dur)
                    } else {
                        None
                    }
                } else {
                    Some(media_dur)
                };
                for (track_idx, clip) in build_source_clips_for_plan(
                    &placement_plan,
                    &path,
                    in_ns,
                    out_ns,
                    playhead,
                    source_info.source_timecode_base_ns,
                    source_info.audio_channel_mode,
                    media_dur_opt,
                    source_info.is_animated_svg,
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
            let media_dur = marks.duration_ns;
            let source_info = SourcePlacementInfo {
                is_audio_only: marks.is_audio_only,
                has_audio: marks.has_audio,
                is_image: marks.is_image,
                is_animated_svg: marks.is_animated_svg,
                source_timecode_base_ns: marks.source_timecode_base_ns,
                audio_channel_mode: marks.audio_channel_mode,
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
                ensure_matching_source_track_exists(&mut proj, source_info);
                let placement_plan = build_source_placement_plan_by_track_id(
                    &proj,
                    active_tid.as_deref(),
                    source_info,
                    source_monitor_auto_link_av,
                );
                let mut track_changes: Vec<TrackClipsChange> = Vec::new();
                let magnetic_mode_for_placement =
                    magnetic_mode && !placement_plan.uses_linked_pair();
                let media_dur_opt = if source_info.is_image {
                    if source_info.is_animated_svg {
                        Some(media_dur)
                    } else {
                        None
                    }
                } else {
                    Some(media_dur)
                };
                for (track_idx, clip) in build_source_clips_for_plan(
                    &placement_plan,
                    &path,
                    in_ns,
                    out_ns,
                    playhead,
                    source_info.source_timecode_base_ns,
                    source_info.audio_channel_mode,
                    media_dur_opt,
                    source_info.is_animated_svg,
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
        let source_monitor_panel = source_monitor_panel.clone();
        let clip_name_label = clip_name_label.clone();
        let library = library.clone();
        let project = project.clone();
        let proxy_cache = proxy_cache.clone();
        let preferences_state = preferences_state.clone();
        let source_original_uri_for_proxy_fallback = source_original_uri_for_proxy_fallback.clone();
        let set_audio_only = set_audio_only.clone();
        let selected_source_keyword_id = selected_source_keyword_id.clone();
        let source_keyword_entry = source_keyword_entry.clone();
        let refresh_source_keyword_picker = refresh_source_keyword_picker.clone();
        Rc::new(move |path: String, duration_ns: u64| {
            // Show the source preview now that a clip is selected
            source_monitor_panel.set_visible(true);
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
                lookup_source_placement_info(&lib.items, &proj, &path)
            };
            if should_reload {
                let proxy_mode = preferences_state.borrow().proxy_mode.clone();
                reload_source_preview_selection(
                    &path,
                    duration_ns,
                    source_info,
                    &player,
                    &project,
                    &proxy_cache,
                    &proxy_mode,
                    &source_original_uri_for_proxy_fallback,
                    &set_audio_only,
                );
            } else {
                set_audio_only(source_info.is_audio_only);
            }
            let mut m = source_marks.borrow_mut();
            m.path = path;
            m.duration_ns = duration_ns;
            m.in_ns = 0;
            m.out_ns = duration_ns;
            m.display_pos_ns = 0;
            m.is_audio_only = source_info.is_audio_only;
            m.has_audio = source_info.has_audio;
            m.is_image = source_info.is_image;
            m.is_animated_svg = source_info.is_animated_svg;
            m.source_timecode_base_ns = source_info.source_timecode_base_ns;
            m.audio_channel_mode = source_info.audio_channel_mode;
            drop(m);
            *selected_source_keyword_id.borrow_mut() = None;
            source_keyword_entry.set_text("");
            refresh_source_keyword_picker();
        })
    };
    *on_apply_collected_files_impl.borrow_mut() = Some({
        let project = project.clone();
        let library = library.clone();
        let source_marks = source_marks.clone();
        let on_source_selected = on_source_selected.clone();
        let on_project_changed = on_project_changed.clone();
        Rc::new(move |manifest| {
            apply_collected_files_manifest_to_project_state(
                &project,
                &library,
                &source_marks,
                &on_source_selected,
                &on_project_changed,
                &manifest,
            );
        })
    });

    // Wire on_match_frame — locates the selected clip's source in the media
    // library, loads it in the source monitor, and seeks to the matching frame.
    {
        let project = project.clone();
        let library = library.clone();
        let player = player.clone();
        let source_marks = source_marks.clone();
        let on_source_selected = on_source_selected.clone();
        let timeline_state_for_mf = timeline_state.clone();
        let refresh_source_keyword_actions = refresh_source_keyword_actions.clone();
        timeline_state.borrow_mut().on_match_frame = Some(Rc::new(move || {
            let (selected_id, playhead_ns) = {
                let st = timeline_state_for_mf.borrow();
                match st.selected_clip_id.clone() {
                    Some(id) => (id, st.playhead_ns),
                    None => return,
                }
            };
            let clip_info = {
                let proj = project.borrow();
                proj.tracks
                    .iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == selected_id)
                    .map(|c| {
                        (
                            c.source_path.clone(),
                            c.source_in,
                            c.source_out,
                            c.timeline_start,
                        )
                    })
            };
            let Some((source_path, source_in, source_out, timeline_start)) = clip_info else {
                return;
            };
            if source_path.is_empty() {
                return; // Title/adjustment clips have no source
            }
            let duration_ns = {
                let lib = library.borrow();
                lib.items
                    .iter()
                    .find(|item| item.source_path == source_path)
                    .map(|item| item.duration_ns)
                    .unwrap_or(source_out)
            };
            // Load the source clip in the source monitor.
            on_source_selected(source_path, duration_ns);
            // Compute the source position matching the playhead.
            let source_pos = source_in + playhead_ns.saturating_sub(timeline_start);
            let source_pos = source_pos.min(source_out).max(source_in);
            // Seek the source player to the matching frame.
            let _ = player.borrow().seek(source_pos);
            // Update source marks to reflect the clip's in/out range.
            let mut m = source_marks.borrow_mut();
            m.in_ns = source_in;
            m.out_ns = source_out;
            m.display_pos_ns = source_pos;
            drop(m);
            refresh_source_keyword_actions();
        }));
    }

    // ── Media browser ─────────────────────────────────────────────────────
    // Callback for "Create Multicam Clip" from media browser.
    // Places selected media files on the first video track, then triggers multicam creation.
    let on_create_multicam_from_browser: Rc<dyn Fn(Vec<String>)> = {
        let project = project.clone();
        let library = library.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        Rc::new(move |source_paths: Vec<String>| {
            if source_paths.len() < 2 {
                return;
            }
            // Place clips on the first video track at timeline end
            let mut clip_ids = Vec::new();
            {
                let mut proj = project.borrow_mut();
                let video_track = proj.tracks.iter_mut().find(|t| t.is_video());
                let Some(track) = video_track else { return };
                let mut pos = track
                    .clips
                    .iter()
                    .map(|c| c.timeline_end())
                    .max()
                    .unwrap_or(0);
                let lib = library.borrow();
                for path in &source_paths {
                    let dur = lib
                        .items
                        .iter()
                        .find(|item| item.source_path == *path)
                        .map(|item| item.duration_ns)
                        .unwrap_or(5_000_000_000);
                    let clip = crate::model::clip::Clip::new(
                        path,
                        dur,
                        pos,
                        crate::model::clip::ClipKind::Video,
                    );
                    clip_ids.push(clip.id.clone());
                    track.add_clip(clip);
                    pos += dur;
                }
                proj.dirty = true;
            }
            // Select the placed clips and trigger multicam creation
            {
                let mut st = timeline_state.borrow_mut();
                st.set_selected_clip_ids(clip_ids.into_iter().collect());
                let _ = st.request_create_multicam();
            }
            on_project_changed();
        })
    };
    let on_library_changed: Rc<dyn Fn()> = {
        let project = project.clone();
        let library = library.clone();
        let on_project_changed = on_project_changed.clone();
        let refresh_source_keyword_picker = refresh_source_keyword_picker.clone();
        Rc::new(move || {
            {
                let lib = library.borrow();
                let mut proj = project.borrow_mut();
                crate::model::media_library::sync_bins_to_project(&lib, &mut proj);
                proj.dirty = true;
            }
            on_project_changed();
            refresh_source_keyword_picker();
        })
    };
    {
        let library = library.clone();
        let source_marks = source_marks.clone();
        let source_keyword_entry = source_keyword_entry.clone();
        let selected_source_keyword_id = selected_source_keyword_id.clone();
        let source_keyword_status_label = source_keyword_status_label.clone();
        let refresh_source_keyword_picker = refresh_source_keyword_picker.clone();
        let on_library_changed = on_library_changed.clone();
        add_source_keyword_btn.connect_clicked(move |_| {
            let label = source_keyword_entry.text().trim().to_string();
            if label.is_empty() {
                source_keyword_status_label.set_text("Enter a keyword label.");
                return;
            }
            let (path, start_ns, end_ns) = {
                let marks = source_marks.borrow();
                (marks.path.clone(), marks.in_ns, marks.out_ns)
            };
            if path.is_empty() {
                source_keyword_status_label.set_text("Load a source clip to add keyword ranges.");
                return;
            }
            if end_ns <= start_ns {
                source_keyword_status_label
                    .set_text("Set source In and Out to define a keyword range.");
                return;
            }
            let new_range_id = {
                let mut lib = library.borrow_mut();
                lib.items
                    .iter_mut()
                    .find(|item| item.source_path == path)
                    .map(|item| {
                        let range = MediaKeywordRange::new(label.clone(), start_ns, end_ns);
                        let range_id = range.id.clone();
                        item.keyword_ranges.push(range);
                        range_id
                    })
            };
            let Some(new_range_id) = new_range_id else {
                source_keyword_status_label
                    .set_text("This source is not available in the media library.");
                return;
            };
            *selected_source_keyword_id.borrow_mut() = Some(new_range_id);
            on_library_changed();
            refresh_source_keyword_picker();
        });
    }
    {
        let library = library.clone();
        let source_marks = source_marks.clone();
        let source_keyword_entry = source_keyword_entry.clone();
        let selected_source_keyword_id = selected_source_keyword_id.clone();
        let source_keyword_status_label = source_keyword_status_label.clone();
        let refresh_source_keyword_picker = refresh_source_keyword_picker.clone();
        let on_library_changed = on_library_changed.clone();
        update_source_keyword_btn.connect_clicked(move |_| {
            let Some(selected_range_id) = selected_source_keyword_id.borrow().clone() else {
                source_keyword_status_label.set_text("Select a keyword range to update.");
                return;
            };
            let label = source_keyword_entry.text().trim().to_string();
            if label.is_empty() {
                source_keyword_status_label.set_text("Enter a keyword label.");
                return;
            }
            let (path, start_ns, end_ns) = {
                let marks = source_marks.borrow();
                (marks.path.clone(), marks.in_ns, marks.out_ns)
            };
            if path.is_empty() {
                source_keyword_status_label
                    .set_text("Load a source clip to update keyword ranges.");
                return;
            }
            if end_ns <= start_ns {
                source_keyword_status_label
                    .set_text("Set source In and Out to define a keyword range.");
                return;
            }
            let updated = {
                let mut lib = library.borrow_mut();
                lib.items
                    .iter_mut()
                    .find(|item| item.source_path == path)
                    .and_then(|item| {
                        item.keyword_ranges
                            .iter_mut()
                            .find(|range| range.id == selected_range_id)
                    })
                    .map(|range| {
                        range.label = label.clone();
                        range.start_ns = start_ns;
                        range.end_ns = end_ns;
                    })
                    .is_some()
            };
            if !updated {
                source_keyword_status_label
                    .set_text("Selected keyword range is no longer available.");
                return;
            }
            on_library_changed();
            refresh_source_keyword_picker();
        });
    }
    {
        let library = library.clone();
        let source_marks = source_marks.clone();
        let source_keyword_entry = source_keyword_entry.clone();
        let selected_source_keyword_id = selected_source_keyword_id.clone();
        let source_keyword_status_label = source_keyword_status_label.clone();
        let refresh_source_keyword_picker = refresh_source_keyword_picker.clone();
        let on_library_changed = on_library_changed.clone();
        remove_source_keyword_btn.connect_clicked(move |_| {
            let Some(selected_range_id) = selected_source_keyword_id.borrow().clone() else {
                source_keyword_status_label.set_text("Select a keyword range to remove.");
                return;
            };
            let path = source_marks.borrow().path.clone();
            if path.is_empty() {
                source_keyword_status_label
                    .set_text("Load a source clip to remove keyword ranges.");
                return;
            }
            let removed = {
                let mut lib = library.borrow_mut();
                if let Some(item) = lib.items.iter_mut().find(|item| item.source_path == path) {
                    let before = item.keyword_ranges.len();
                    item.keyword_ranges
                        .retain(|range| range.id != selected_range_id);
                    item.keyword_ranges.len() != before
                } else {
                    false
                }
            };
            if !removed {
                source_keyword_status_label
                    .set_text("Selected keyword range is no longer available.");
                return;
            }
            *selected_source_keyword_id.borrow_mut() = None;
            source_keyword_entry.set_text("");
            on_library_changed();
            refresh_source_keyword_picker();
        });
    }

    let (browser, clear_media_selection, force_rebuild_media_browser) =
        media_browser::build_media_browser(
            library.clone(),
            on_source_selected.clone(),
            on_relink_media_gui.clone(),
            on_create_multicam_from_browser,
            on_library_changed,
            proxy_cache.clone(),
            preferences_state.clone(),
        );

    // Now that both on_source_selected and force_rebuild_media_browser exist,
    // fill in the real relink implementation.
    *on_relink_media_impl.borrow_mut() = Some({
        let window_weak = window_weak.clone();
        let project = project.clone();
        let library = library.clone();
        let timeline_state = timeline_state.clone();
        let source_marks = source_marks.clone();
        let on_source_selected = on_source_selected.clone();
        let on_project_changed = on_project_changed.clone();
        let inspector_view = inspector_view.clone();
        let force_rebuild_media_browser = force_rebuild_media_browser.clone();
        let timeline_panel_cell = timeline_panel_cell.clone();
        Rc::new(move || {
            let Some(win) = window_weak.upgrade() else {
                return;
            };
            let missing_paths = {
                let proj = project.borrow();
                let lib = library.borrow();
                collect_missing_source_paths(&proj, &lib.items)
            };
            if missing_paths.is_empty() {
                flash_window_status_title(&win, &project, "No offline media to relink");
                return;
            }

            if missing_paths.len() == 1 {
                // Single missing file: use a file picker for direct replacement.
                let old_path = missing_paths[0].clone();
                let dialog = gtk::FileDialog::new();
                let fname = std::path::Path::new(&old_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                dialog.set_title(&format!("Relink — Select replacement for {}", fname));
                let filter = gtk::FileFilter::new();
                filter.add_mime_type("video/*");
                filter.add_mime_type("audio/*");
                filter.add_mime_type("image/*");
                filter.set_name(Some("Media Files"));
                let filters = gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                let all_filter = gtk::FileFilter::new();
                all_filter.add_pattern("*");
                all_filter.set_name(Some("All Files"));
                filters.append(&all_filter);
                dialog.set_filters(Some(&filters));

                let project = project.clone();
                let library = library.clone();
                let timeline_state = timeline_state.clone();
                let source_marks = source_marks.clone();
                let on_source_selected = on_source_selected.clone();
                let on_project_changed = on_project_changed.clone();
                let inspector_view = inspector_view.clone();
                let force_rebuild_media_browser = force_rebuild_media_browser.clone();
                let timeline_panel_cell = timeline_panel_cell.clone();
                let win_for_result = win.clone();
                dialog.open(Some(&win), gio::Cancellable::NONE, move |result| {
                    let Ok(file) = result else { return };
                    let Some(new_file_path) = file.path() else {
                        return;
                    };
                    let new_path_str = new_file_path.to_string_lossy().to_string();

                    // Replace old_path → new_path_str in project clips and library
                    let (clip_count, lib_count) = {
                        let mut proj = project.borrow_mut();
                        let mut lib = library.borrow_mut();
                        let mut cc = 0usize;
                        for track in proj.tracks.iter_mut() {
                            for clip in track.clips.iter_mut() {
                                if clip.source_path == old_path {
                                    clip.source_path = new_path_str.clone();
                                    cc += 1;
                                }
                            }
                        }
                        let mut lc = 0usize;
                        for item in lib.items.iter_mut() {
                            if item.source_path == old_path {
                                item.source_path = new_path_str.clone();
                                lc += 1;
                            }
                        }
                        if cc > 0 {
                            proj.dirty = true;
                        }
                        (cc, lc)
                    };
                    log::info!(
                        "[relink] direct: {} → {} (clips={}, lib={})",
                        old_path,
                        new_path_str,
                        clip_count,
                        lib_count,
                    );

                    // Refresh source monitor if the relinked path was loaded
                    {
                        let current_path = source_marks.borrow().path.clone();
                        if current_path == old_path {
                            let duration_ns = library
                                .borrow()
                                .items
                                .iter()
                                .find(|item| item.source_path == new_path_str)
                                .map(|item| item.duration_ns)
                                .unwrap_or_else(|| source_marks.borrow().duration_ns);
                            on_source_selected(new_path_str.clone(), duration_ns);
                        }
                    }

                    // Refresh availability + project changed + belt-and-suspenders
                    {
                        let proj = project.borrow();
                        let mut lib = library.borrow_mut();
                        let mut st = timeline_state.borrow_mut();
                        refresh_media_availability_state(&proj, lib.items.as_mut_slice(), &mut st);
                    }
                    on_project_changed();
                    {
                        let proj = project.borrow();
                        let mut lib = library.borrow_mut();
                        let mut st = timeline_state.borrow_mut();
                        let mp = refresh_media_availability_state(
                            &proj,
                            lib.items.as_mut_slice(),
                            &mut st,
                        );
                        let (selected, playhead_ns) = (st.selected_clip_id.clone(), st.playhead_ns);
                        drop(st);
                        inspector_view.update(&proj, selected.as_deref(), playhead_ns, Some(&mp));
                        drop(proj);
                        drop(lib);
                        force_rebuild_media_browser();
                        if let Some(ref w) = *timeline_panel_cell.borrow() {
                            w.queue_draw();
                        }
                    }

                    let msg = format!(
                        "Relinked: {}\n→ {}\n({} clip(s), {} library item(s) updated)",
                        old_path, new_path_str, clip_count, lib_count,
                    );
                    log::info!("[relink] result: {}", msg.replace('\n', " | "));
                    let alert = gtk::AlertDialog::builder()
                        .message("Relink Complete")
                        .detail(&msg)
                        .buttons(["OK"])
                        .build();
                    alert.show(Some(&win_for_result));
                });
            } else {
                // Multiple missing files: scan a folder for matching filenames.
                let dialog = gtk::FileDialog::new();
                dialog.set_title(&format!(
                    "Relink {} Missing Files — Choose Search Folder",
                    missing_paths.len(),
                ));
                let project = project.clone();
                let library = library.clone();
                let timeline_state = timeline_state.clone();
                let source_marks = source_marks.clone();
                let on_source_selected = on_source_selected.clone();
                let on_project_changed = on_project_changed.clone();
                let inspector_view = inspector_view.clone();
                let force_rebuild_media_browser = force_rebuild_media_browser.clone();
                let timeline_panel_cell = timeline_panel_cell.clone();
                let win_for_result = win.clone();
                dialog.select_folder(Some(&win), gio::Cancellable::NONE, move |result| {
                    let Ok(folder) = result else { return };
                    let Some(root_path) = folder.path() else { return };
                    if !root_path.is_dir() {
                        flash_window_status_title(&win_for_result, &project, "Relink failed: invalid folder");
                        return;
                    }
                    let summary = {
                        let mut proj = project.borrow_mut();
                        let mut lib = library.borrow_mut();
                        relink_missing_media_under_root(&mut proj, lib.items.as_mut_slice(), &root_path)
                    };
                    log::info!(
                        "[relink] scanned={} remapped={} unresolved={} clips={} library={}",
                        summary.scanned_files,
                        summary.remapped.len(),
                        summary.unresolved.len(),
                        summary.updated_clip_count,
                        summary.updated_library_count,
                    );

                    // Refresh source monitor if the relinked path was loaded
                    let remapped_source = {
                        let current_path = source_marks.borrow().path.clone();
                        if current_path.is_empty() {
                            None
                        } else {
                            summary.remapped.iter().find_map(|(from, to)| {
                                if from == &current_path { Some(to.clone()) } else { None }
                            })
                        }
                    };
                    if let Some(new_path) = remapped_source {
                        let duration_ns = library
                            .borrow()
                            .items.iter()
                            .find(|item| item.source_path == new_path)
                            .map(|item| item.duration_ns)
                            .unwrap_or_else(|| source_marks.borrow().duration_ns);
                        on_source_selected(new_path, duration_ns);
                    }

                    // Refresh availability + project changed + belt-and-suspenders
                    {
                        let proj = project.borrow();
                        let mut lib = library.borrow_mut();
                        let mut st = timeline_state.borrow_mut();
                        refresh_media_availability_state(&proj, lib.items.as_mut_slice(), &mut st);
                    }
                    on_project_changed();
                    let remaining_missing = {
                        let proj = project.borrow();
                        let mut lib = library.borrow_mut();
                        let mut st = timeline_state.borrow_mut();
                        let mp = refresh_media_availability_state(&proj, lib.items.as_mut_slice(), &mut st);
                        let (selected, playhead_ns) = (st.selected_clip_id.clone(), st.playhead_ns);
                        drop(st);
                        inspector_view.update(&proj, selected.as_deref(), playhead_ns, Some(&mp));
                        drop(proj);
                        drop(lib);
                        force_rebuild_media_browser();
                        if let Some(ref w) = *timeline_panel_cell.borrow() {
                            w.queue_draw();
                        }
                        mp.len()
                    };

                    let msg = if summary.remapped.is_empty() && !summary.unresolved.is_empty() {
                        format!(
                            "No matching files found.\n{} file(s) still offline.\n({} files scanned under selected folder)",
                            summary.unresolved.len(),
                            summary.scanned_files,
                        )
                    } else {
                        let mut lines = Vec::new();
                        lines.push(format!("{} file(s) relinked", summary.remapped.len()));
                        if !summary.unresolved.is_empty() {
                            lines.push(format!("{} file(s) still unresolved", summary.unresolved.len()));
                        }
                        if remaining_missing > 0 {
                            lines.push(format!("{} offline item(s) remaining", remaining_missing));
                        }
                        lines.push(format!("({} files scanned)", summary.scanned_files));
                        lines.join("\n")
                    };
                    log::info!("[relink] result: {}", msg.replace('\n', " | "));
                    let alert = gtk::AlertDialog::builder()
                        .message("Relink Results")
                        .detail(&msg)
                        .buttons(["OK"])
                        .build();
                    alert.show(Some(&win_for_result));
                });
            }
        })
    });

    // ── on_close_preview: deselect media + hide preview + reset source state ──
    *on_close_preview_impl.borrow_mut() = Some({
        let clear_media_selection = clear_media_selection.clone();
        let source_monitor_panel = source_monitor_panel.clone();
        let clip_name_label = clip_name_label.clone();
        let source_marks = source_marks.clone();
        let player = player.clone();
        let selected_source_keyword_id = selected_source_keyword_id.clone();
        let source_keyword_entry = source_keyword_entry.clone();
        let refresh_source_keyword_picker = refresh_source_keyword_picker.clone();
        let source_original_uri_for_proxy_fallback = source_original_uri_for_proxy_fallback.clone();
        Rc::new(move || {
            clear_media_selection();
            source_monitor_panel.set_visible(false);
            clip_name_label.set_text("No source loaded");
            {
                let mut m = source_marks.borrow_mut();
                *m = crate::model::media_library::SourceMarks::default();
            }
            *selected_source_keyword_id.borrow_mut() = None;
            source_keyword_entry.set_text("");
            refresh_source_keyword_picker();
            if let Ok(mut fallback_uri) = source_original_uri_for_proxy_fallback.lock() {
                *fallback_uri = None;
            }
            let _ = player.borrow().stop();
        })
    });
    // Left panel: vertical Paned — browser/effects stack (top) + source preview (bottom)
    // The Paned lets the user resize the split after a source is selected.
    source_monitor_panel.set_visible(false);

    // ── Effects Browser ──────────────────────────────────────────────────
    let on_apply_effect: Rc<dyn Fn(String)> = {
        let timeline_state = timeline_state.clone();
        let project = project.clone();
        Rc::new(move |plugin_name: String| {
            let (clip_id, track_id) = {
                let st = timeline_state.borrow();
                let cid = match st.selected_clip_id.clone() {
                    Some(id) => id,
                    None => return,
                };
                let tid = match st.selected_track_id.clone() {
                    Some(id) => id,
                    None => return,
                };
                (cid, tid)
            };
            // Check clip is video/image and find its effect count for insert index.
            let index = {
                let proj = project.borrow();
                let clip = proj
                    .tracks
                    .iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == clip_id);
                match clip {
                    Some(c) if c.kind != crate::model::clip::ClipKind::Audio => {
                        c.frei0r_effects.len()
                    }
                    _ => return,
                }
            };
            // Populate default parameter values from the registry so that
            // parameter sliders appear in the inspector immediately.
            let registry = crate::media::frei0r_registry::Frei0rRegistry::get_or_discover();
            let mut default_params = std::collections::HashMap::new();
            let mut default_string_params = std::collections::HashMap::new();
            if let Some(info) = registry.find_by_name(&plugin_name) {
                for p in &info.params {
                    if p.param_type == crate::media::frei0r_registry::Frei0rParamType::String {
                        if let Some(ref s) = p.default_string {
                            default_string_params.insert(p.name.clone(), s.clone());
                        }
                    } else {
                        default_params.insert(p.name.clone(), p.default_value);
                    }
                }
            }
            let effect = crate::model::clip::Frei0rEffect::with_all_params(
                &plugin_name,
                default_params,
                default_string_params,
            );
            let cmd = crate::undo::AddFrei0rEffectCommand {
                clip_id,
                track_id,
                effect,
                index,
            };
            {
                let mut st = timeline_state.borrow_mut();
                let mut proj = project.borrow_mut();
                st.history.execute(Box::new(cmd), &mut proj);
            }
            let cb = {
                let st = timeline_state.borrow();
                st.on_project_changed.clone()
            };
            if let Some(cb) = cb {
                cb();
            }
        })
    };

    let (effects_browser_widget, set_effects_registry) =
        effects_browser::build_effects_browser(on_apply_effect);

    // Audio effects (LADSPA) browser
    let on_apply_ladspa_effect: Rc<dyn Fn(String)> = {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        Rc::new(move |ladspa_name: String| {
            let reg = crate::media::ladspa_registry::LadspaRegistry::get_or_discover();
            let Some(info) = reg.find_by_name(&ladspa_name) else {
                return;
            };
            let effect =
                crate::model::clip::LadspaEffect::new(&info.ladspa_name, &info.gst_element_name);
            let clip_id = {
                let st = timeline_state.borrow();
                st.selected_clip_id.clone()
            };
            let Some(clip_id) = clip_id else { return };
            {
                let mut proj = project.borrow_mut();
                if let Some(clip) = proj.clip_mut(&clip_id) {
                    clip.ladspa_effects.push(effect);
                }
                proj.dirty = true;
            }
            let cb = {
                let st = timeline_state.borrow();
                st.on_project_changed.clone()
            };
            if let Some(cb) = cb {
                cb();
            }
        })
    };
    let (audio_effects_browser_widget, set_ladspa_registry) =
        audio_effects_browser::build_audio_effects_browser(on_apply_ladspa_effect);

    // ── Titles browser callbacks ─────────────────────────────────────────
    let on_add_title: Rc<dyn Fn(String)> = {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        Rc::new(move |template_id: String| {
            let template = match title_templates::find_template(&template_id) {
                Some(t) => t,
                None => return,
            };
            let playhead = {
                let st = timeline_state.borrow();
                st.playhead_ns
            };
            let clip = title_templates::create_title_clip(template, playhead);
            let mut proj = project.borrow_mut();
            // Prefer the selected track (if it's a video track), fall back to first video track.
            let selected_tid = timeline_state.borrow().selected_track_id.clone();
            let track_idx = selected_tid
                .and_then(|tid| proj.tracks.iter().position(|t| t.id == tid && t.is_video()))
                .or_else(|| proj.tracks.iter().position(|t| t.is_video()))
                .unwrap_or_else(|| {
                    let t = crate::model::track::Track::new_video("Video 1");
                    proj.tracks.push(t);
                    proj.tracks.len() - 1
                });
            let magnetic_mode = {
                let st = timeline_state.borrow();
                st.magnetic_mode
            };
            let track = &mut proj.tracks[track_idx];
            let change = insert_clip_at_playhead_on_track(track, clip, playhead, magnetic_mode);
            let cmd = crate::undo::SetTrackClipsCommand {
                track_id: change.track_id,
                old_clips: change.old_clips,
                new_clips: change.new_clips,
                label: "Add title clip".to_string(),
            };
            drop(proj);
            {
                let mut st = timeline_state.borrow_mut();
                let mut proj = project.borrow_mut();
                st.history.execute(Box::new(cmd), &mut proj);
            }
            let cb = {
                let st = timeline_state.borrow();
                st.on_project_changed.clone()
            };
            if let Some(cb) = cb {
                cb();
            }
        })
    };

    let on_apply_title_to_clip: Rc<dyn Fn(String)> = {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        Rc::new(move |template_id: String| {
            let template = match title_templates::find_template(&template_id) {
                Some(t) => t,
                None => return,
            };
            let clip_id = {
                let st = timeline_state.borrow();
                st.selected_clip_id.clone()
            };
            let clip_id = match clip_id {
                Some(id) => id,
                None => return,
            };
            // Find clip, snapshot, apply.
            let cmd = {
                let mut proj = project.borrow_mut();
                let clip = proj
                    .tracks
                    .iter_mut()
                    .flat_map(|t| t.clips.iter_mut())
                    .find(|c| c.id == clip_id);
                match clip {
                    Some(clip) => {
                        let before = crate::undo::TitlePropertySnapshot::from_clip(clip);
                        title_templates::apply_template_to_clip(template, clip);
                        if clip.title_text.is_empty() {
                            clip.title_text = template.display_name.to_string();
                        }
                        let after = crate::undo::TitlePropertySnapshot::from_clip(clip);
                        Some(crate::undo::SetTitlePropertiesCommand {
                            clip_id: clip_id.clone(),
                            before,
                            after,
                        })
                    }
                    None => None,
                }
            };
            if let Some(cmd) = cmd {
                {
                    let mut st = timeline_state.borrow_mut();
                    let mut proj = project.borrow_mut();
                    st.history.execute(Box::new(cmd), &mut proj);
                }
                let cb = {
                    let st = timeline_state.borrow();
                    st.on_project_changed.clone()
                };
                if let Some(cb) = cb {
                    cb();
                }
            }
        })
    };

    let titles_browser_widget =
        titles_browser::build_titles_browser(on_add_title, on_apply_title_to_clip);

    // Stack: Media Browser + Effects Browser + Titles Browser as switchable tabs.
    let left_stack = gtk::Stack::new();
    left_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    left_stack.set_transition_duration(150);
    left_stack.add_titled(&browser, Some("media"), "Media");
    left_stack.add_titled(&effects_browser_widget, Some("effects"), "Effects");
    left_stack.add_titled(
        &audio_effects_browser_widget,
        Some("audio_effects"),
        "Audio FX",
    );
    left_stack.add_titled(&titles_browser_widget, Some("titles"), "Titles");

    // ── Dynamic tab bar (replaces StackSwitcher) ─────────────────────────
    // 4 ToggleButtons in a radio group.  Layout restructures at runtime:
    //   wide (≥ 330 px): 1 row  [Media][Effects][Audio FX][Titles]
    //   narrow (< 330 px): row1 [Media][Effects]
    //                      row2 [Audio FX][Titles]
    let tab_btn = |label: &str| -> gtk::ToggleButton {
        let b = gtk::ToggleButton::with_label(label);
        b.set_hexpand(true);
        b
    };
    let tb_media = tab_btn("Media");
    let tb_effects = tab_btn("Effects");
    let tb_audio_fx = tab_btn("Audio FX");
    let tb_titles = tab_btn("Titles");
    tb_media.set_active(true);
    tb_effects.set_group(Some(&tb_media));
    tb_audio_fx.set_group(Some(&tb_media));
    tb_titles.set_group(Some(&tb_media));

    let tab_row1 = gtk::Box::new(Orientation::Horizontal, 0);
    tab_row1.add_css_class("linked");
    tab_row1.set_hexpand(true);
    tab_row1.set_margin_top(4);
    tab_row1.set_margin_bottom(2);
    tab_row1.append(&tb_media);
    tab_row1.append(&tb_effects);
    tab_row1.append(&tb_audio_fx);
    tab_row1.append(&tb_titles);

    let tab_row2 = gtk::Box::new(Orientation::Horizontal, 0);
    tab_row2.add_css_class("linked");
    tab_row2.set_hexpand(true);
    tab_row2.set_margin_bottom(2);
    tab_row2.set_visible(false); // hidden in wide mode

    let tab_bar = gtk::Box::new(Orientation::Vertical, 0);
    tab_bar.set_hexpand(true);
    tab_bar.append(&tab_row1);
    tab_bar.append(&tab_row2);

    // Wire buttons → stack page switches
    {
        let s = left_stack.clone();
        let workspace_layouts_applying = workspace_layouts_applying.clone();
        let sync_workspace_layout_state = sync_workspace_layout_state.clone();
        tb_media.connect_toggled(move |b| {
            if b.is_active() {
                s.set_visible_child_name("media");
                if !workspace_layouts_applying.get() {
                    sync_workspace_layout_state();
                }
            }
        });
    }
    {
        let s = left_stack.clone();
        let workspace_layouts_applying = workspace_layouts_applying.clone();
        let sync_workspace_layout_state = sync_workspace_layout_state.clone();
        tb_effects.connect_toggled(move |b| {
            if b.is_active() {
                s.set_visible_child_name("effects");
                if !workspace_layouts_applying.get() {
                    sync_workspace_layout_state();
                }
            }
        });
    }
    {
        let s = left_stack.clone();
        let workspace_layouts_applying = workspace_layouts_applying.clone();
        let sync_workspace_layout_state = sync_workspace_layout_state.clone();
        tb_audio_fx.connect_toggled(move |b| {
            if b.is_active() {
                s.set_visible_child_name("audio_effects");
                if !workspace_layouts_applying.get() {
                    sync_workspace_layout_state();
                }
            }
        });
    }
    {
        let s = left_stack.clone();
        let workspace_layouts_applying = workspace_layouts_applying.clone();
        let sync_workspace_layout_state = sync_workspace_layout_state.clone();
        tb_titles.connect_toggled(move |b| {
            if b.is_active() {
                s.set_visible_child_name("titles");
                if !workspace_layouts_applying.get() {
                    sync_workspace_layout_state();
                }
            }
        });
    }

    // Respond to width changes: dynamically restructure into 1 or 2 rows
    {
        let tab_row1 = tab_row1.clone();
        let tab_row2 = tab_row2.clone();
        let tb_audio_fx = tb_audio_fx.clone();
        let tb_titles = tb_titles.clone();
        let narrow_state = std::cell::Cell::new(false);
        let narrow_state = std::rc::Rc::new(narrow_state);
        tab_bar.connect_notify_local(Some("width"), move |widget, _| {
            let w = widget.width();
            if w == 0 {
                return;
            }
            let narrow = w < 330;
            if narrow == narrow_state.get() {
                return;
            }
            narrow_state.set(narrow);
            if narrow {
                tab_row1.remove(&tb_audio_fx);
                tab_row1.remove(&tb_titles);
                tab_row2.append(&tb_audio_fx);
                tab_row2.append(&tb_titles);
                tab_row2.set_visible(true);
            } else {
                tab_row2.remove(&tb_audio_fx);
                tab_row2.remove(&tb_titles);
                tab_row1.append(&tb_audio_fx);
                tab_row1.append(&tb_titles);
                tab_row2.set_visible(false);
            }
        });
    }

    let left_stack_container = gtk::Box::new(Orientation::Vertical, 0);
    left_stack_container.append(&tab_bar);
    left_stack_container.append(&left_stack);

    let left_vpaned = Paned::new(Orientation::Vertical);
    left_vpaned.set_vexpand(true);
    left_vpaned.set_position(320); // browser gets ~320 px by default
    left_vpaned.set_start_child(Some(&left_stack_container));
    left_vpaned.set_end_child(Some(&source_monitor_panel));
    top_paned.set_start_child(Some(&left_vpaned));

    root_vpaned.set_start_child(Some(&top_paned));

    // ── Timeline ──────────────────────────────────────────────────────────
    let timeline_scroll = ScrolledWindow::new();
    timeline_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    timeline_scroll.set_vexpand(true);
    timeline_scroll.set_hexpand(true);

    let (timeline_panel, timeline_area) =
        build_timeline_panel(timeline_state.clone(), on_project_changed.clone());

    // Extract the track-management bar from timeline_panel so we can place
    // the keyframe dopesheet between the timeline and the bar.
    let timeline_bar_widget = timeline_panel.last_child();
    if let Some(ref bar) = timeline_bar_widget {
        timeline_panel.remove(bar);
    }
    timeline_scroll.set_child(Some(&timeline_panel));

    // Vertical Paned: top = timeline scroll, bottom = keyframe dopesheet.
    // The dopesheet is added later (see keyframe editor section below).
    let timeline_paned = Paned::new(Orientation::Vertical);
    timeline_paned.set_vexpand(true);
    timeline_paned.set_hexpand(true);
    timeline_paned.set_start_child(Some(&timeline_scroll));
    timeline_paned.set_resize_start_child(true);
    timeline_paned.set_shrink_start_child(false);
    timeline_paned.set_resize_end_child(true);
    timeline_paned.set_shrink_end_child(false);

    // Outer vbox: Paned on top, fixed bar at bottom.
    let timeline_outer_vbox = gtk::Box::new(Orientation::Vertical, 0);
    timeline_outer_vbox.set_vexpand(true);
    timeline_outer_vbox.set_hexpand(true);
    timeline_outer_vbox.append(&timeline_paned);
    if let Some(ref bar) = timeline_bar_widget {
        timeline_outer_vbox.append(bar);
    }
    root_vpaned.set_end_child(Some(&timeline_outer_vbox));

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
        let voice_enhance_cache = voice_enhance_cache.clone();
        let frame_interp_cache = frame_interp_cache.clone();
        let preferences_state = preferences_state.clone();
        let panel_weak = timeline_area.downgrade();
        let transform_overlay_cell = transform_overlay_cell.clone();
        let keyframe_editor_cell = keyframe_editor_cell.clone();
        let transcript_panel_cell = transcript_panel_cell.clone();
        let prog_canvas_frame = prog_canvas_frame.clone();
        let program_empty_hint = program_empty_hint.clone();
        let picture_a = picture_a.clone();
        let picture_b = picture_b.clone();
        let pending_reload_ticket = pending_reload_ticket.clone();
        let mcp_light_refresh_next = mcp_light_refresh_next.clone();
        let suppress_resume_on_next_reload = suppress_resume_on_next_reload.clone();
        let clear_media_browser_on_next_reload = clear_media_browser_on_next_reload.clone();
        let force_rebuild_media_browser = force_rebuild_media_browser.clone();
        let sync_tracking_controls = sync_tracking_controls.clone();

        *on_project_changed_impl.borrow_mut() = Some(Box::new(move || {
            // Sync compound clip duration when editing inside a compound.
            timeline_state.borrow().sync_compound_duration();
            let use_light_refresh = mcp_light_refresh_next.replace(false);
            if clear_media_browser_on_next_reload.replace(false) {
                on_close_preview();
                {
                    let mut lib = library.borrow_mut();
                    lib.items.clear();
                    lib.bins.clear();
                    lib.collections.clear();
                }
                prog_player.borrow_mut().stop();
                let preserve_sidecar_proxies = {
                    let prefs = preferences_state.borrow();
                    prefs.proxy_mode.is_enabled() && prefs.persist_proxies_next_to_original_media
                };
                {
                    let mut cache = proxy_cache.borrow_mut();
                    cache.cleanup_for_unload(preserve_sidecar_proxies);
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
                Vec<ProjectLibraryEntry>,
                (u32, u32),
                (u32, u32),
            ) = {
                let proj = project.borrow();
                let selected = timeline_state.borrow().selected_clip_id.clone();
                let playhead_ns = timeline_state.borrow().playhead_ns;
                if let Some(ref editor) = *keyframe_editor_cell.borrow() {
                    editor.queue_redraw();
                }
                if let Some(ref tp) = *transcript_panel_cell.borrow() {
                    tp.rebuild_from_project(&proj);
                }

                // Sync transform overlay: show handles when a clip is selected,
                // using keyframe-interpolated values at the current playhead.
                if let Some(ref to) = *transform_overlay_cell.borrow() {
                    to.set_project_dimensions(proj.width, proj.height);
                    // Keep canvas frame aspect ratio in sync with project dimensions.
                    if proj.height > 0 {
                        prog_canvas_frame.set_ratio(proj.width as f32 / proj.height as f32);
                    }
                    sync_transform_overlay_to_playhead(to, &proj, selected.as_deref(), playhead_ns);
                    let (ix, iy) = prog_player
                        .borrow()
                        .content_inset_for_clip(selected.as_deref());
                    to.set_content_inset(ix, iy);
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

                // When inside a compound deep-dive, play only the
                // compound's internal tracks instead of the full root
                // timeline so the program monitor shows compound content.
                let editing_tracks = {
                    let st = timeline_state.borrow();
                    st.resolve_editing_tracks(&proj) as *const [crate::model::track::Track]
                };
                // SAFETY: proj is borrowed immutably for the duration of this block;
                // the raw pointer just avoids a lifetime conflict with the RefCell borrow.
                let editing_tracks: &[crate::model::track::Track] = unsafe { &*editing_tracks };
                let mut clips: Vec<ProgramClip> = editing_tracks
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| proj.track_is_active_for_output(t))
                    .flat_map(|(t_idx, t)| {
                        let audio_only = t.is_audio();
                        let suppress = suppress_embedded_audio_ids.clone();
                        t.clips.iter().flat_map(move |c| {
                            clip_to_program_clips(
                                c,
                                audio_only,
                                t.duck,
                                t.duck_amount_db,
                                t_idx,
                                &suppress,
                                0,
                                0,
                            )
                        })
                    })
                    .collect();
                crate::media::tracking::apply_tracking_bindings_to_program_clips(
                    &mut clips,
                    editing_tracks,
                );
                if let Some(ref to) = *transform_overlay_cell.borrow() {
                    sync_transform_overlay_to_playhead_from_program_clips(
                        to,
                        &clips,
                        selected.as_deref(),
                        playhead_ns,
                    );
                    let (ix, iy) = prog_player
                        .borrow()
                        .content_inset_for_clip(selected.as_deref());
                    to.set_content_inset(ix, iy);
                }
                // Keep media browser in sync with timeline clip sources after project open/load.
                // Source-backed clips still dedupe by source path, while sourceless timeline-native
                // clips keep distinct clip-id keys so title cards don't collapse onto one
                // empty-path browser item.
                let media = collect_project_library_entries(&proj);
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
                sync_library_with_project_entries(&mut lib, &media_from_project);
                // Restore bin assignments from parsed FCPXML data.
                let mut proj = project.borrow_mut();
                crate::model::media_library::apply_bins_from_project(&mut lib, &mut proj);
                drop(proj);
            }

            let missing_paths = {
                let proj = project.borrow();
                let mut lib = library.borrow_mut();
                let mut st = timeline_state.borrow_mut();
                let mp = refresh_media_availability_state(&proj, lib.items.as_mut_slice(), &mut st);
                log::debug!(
                    "[on_project_changed] missing_count={} lib_missing_count={}",
                    mp.len(),
                    lib.items.iter().filter(|i| i.is_missing).count(),
                );
                mp
            };
            {
                let proj = project.borrow();
                let (selected, playhead_ns) = {
                    let st = timeline_state.borrow();
                    (st.selected_clip_id.clone(), st.playhead_ns)
                };
                inspector_view.update(
                    &proj,
                    selected.as_deref(),
                    playhead_ns,
                    Some(&missing_paths),
                );
                sync_tracking_controls();
                inspector_view.update_keyframe_indicator(&proj, playhead_ns);
            }

            // Synchronously rebuild media browser grid so offline badges and
            // source paths are always current (don't wait for the 100ms timer).
            force_rebuild_media_browser();

            // Reload program player — preserve current position so the monitor
            // doesn't jump to 0 on every project change (e.g., clip name edit).
            let suppress_resume = suppress_resume_on_next_reload.replace(false);
            let (prev_pos, was_playing) = {
                let pp = prog_player.borrow();
                let raw_pos = pp.timeline_pos_ns;
                let playing = !suppress_resume
                    && matches!(pp.state(), crate::media::player::PlayerState::Playing);
                // When inside a compound deep-dive, translate the root-level
                // playhead into compound-internal coordinates so the seek
                // targets the correct position within the internal clips.
                let st = timeline_state.borrow();
                let pos = if st.compound_nav_stack.is_empty() {
                    raw_pos
                } else {
                    st.internal_playhead_ns()
                };
                (pos, playing)
            };
            let (proj_w, proj_h) = project_dims;
            let (fr_num, fr_den) = project_frame_rate;
            let prog_player_reload = prog_player.clone();
            let preferences_state_reload = preferences_state.clone();
            let project_reload = project.clone();
            let proxy_cache_reload = proxy_cache.clone();
            let bg_removal_cache_reload = bg_removal_cache.clone();
            let voice_enhance_cache_reload = voice_enhance_cache.clone();
            let frame_interp_cache_reload = frame_interp_cache.clone();
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
                            let near_playhead_variants = {
                                let proj = project_reload.borrow();
                                collect_near_playhead_proxy_variants(
                                    &proj,
                                    prev_pos,
                                    NEAR_PLAYHEAD_PROXY_PRIME_WINDOW_NS,
                                    NEAR_PLAYHEAD_PROXY_PRIME_MAX_SOURCES,
                                    manual_scale,
                                )
                            };
                            let clip_variants = {
                                let proj = project_reload.borrow();
                                collect_unique_proxy_variants(&proj, manual_scale)
                            };
                            {
                                let mut cache = proxy_cache_reload.borrow_mut();
                                cache.cleanup_stale_variants(&clip_variants);
                                request_proxy_variants(&mut cache, &near_playhead_variants);
                                request_proxy_variants(&mut cache, &clip_variants);
                            }
                            if !near_playhead_variants.is_empty() {
                                log::debug!(
                                    "window:on_project_changed primed {} near-playhead proxy source(s) around {}ns",
                                    near_playhead_variants.len(),
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

                    // Request voice-enhance prerender for clips that
                    // have it enabled. The cache is keyed by
                    // (source_path, strength) so changing the strength
                    // slider produces a new request, and toggling
                    // voice_enhance off doesn't invalidate previously
                    // generated files.
                    {
                        let proj = project_reload.borrow();
                        let mut cache = voice_enhance_cache_reload.borrow_mut();
                        fn walk_voice_enhance_request(
                            cache: &mut crate::media::voice_enhance_cache::VoiceEnhanceCache,
                            tracks: &[crate::model::track::Track],
                        ) {
                            for track in tracks {
                                for clip in &track.clips {
                                    if clip.voice_enhance {
                                        cache.request(
                                            &clip.source_path,
                                            clip.voice_enhance_strength,
                                        );
                                    }
                                    if let Some(ref ctracks) = clip.compound_tracks {
                                        walk_voice_enhance_request(cache, ctracks);
                                    }
                                }
                            }
                        }
                        walk_voice_enhance_request(&mut cache, &proj.tracks);
                        let paths = cache.paths.clone();
                        prog_player_reload
                            .borrow_mut()
                            .update_voice_enhance_paths(paths);
                    }

                    // Request AI frame interpolation for slow-motion clips
                    // that opt in via SlowMotionInterp::Ai. The cache is a
                    // no-op for clips with no slow-motion segment or with
                    // any other interpolation mode.
                    {
                        let proj = project_reload.borrow();
                        let mut cache = frame_interp_cache_reload.borrow_mut();
                        fn walk_request(
                            cache: &mut crate::media::frame_interp_cache::FrameInterpCache,
                            tracks: &[crate::model::track::Track],
                        ) {
                            for track in tracks {
                                for clip in &track.clips {
                                    cache.request_for_clip(clip);
                                    if let Some(ref ctracks) = clip.compound_tracks {
                                        walk_request(cache, ctracks);
                                    }
                                }
                            }
                        }
                        walk_request(&mut cache, &proj.tracks);
                        let interp_paths = cache.snapshot_paths_by_clip_id(&proj);
                        prog_player_reload
                            .borrow_mut()
                            .update_frame_interp_paths(interp_paths);
                    }
                }

                let animated_svg_paths = {
                    let mut paths: HashMap<String, String> = HashMap::new();
                    for clip in &clips {
                        if !clip.animated_svg {
                            continue;
                        }
                        let key = crate::media::animated_svg::animated_svg_render_key(
                            &clip.source_path,
                            clip.source_in_ns,
                            clip.source_out_ns,
                            clip.media_duration_ns,
                            fr_num,
                            fr_den,
                        );
                        if paths.contains_key(&key) {
                            continue;
                        }
                        match crate::media::animated_svg::ensure_rendered_clip(
                            &clip.source_path,
                            clip.source_in_ns,
                            clip.source_out_ns,
                            clip.media_duration_ns,
                            fr_num,
                            fr_den,
                        ) {
                            Ok(render_path) => {
                                paths.insert(key, render_path);
                            }
                            Err(err) => {
                                log::warn!(
                                    "window:on_project_changed failed to render animated SVG clip {} [{}..{}]: {}",
                                    clip.source_path,
                                    clip.source_in_ns,
                                    clip.source_out_ns,
                                    err
                                );
                            }
                        }
                    }
                    paths
                };
                prog_player_reload
                    .borrow_mut()
                    .update_animated_svg_paths(animated_svg_paths);

                let project_file_path = { project_reload.borrow().file_path.clone() };
                let master_gain_db = { project_reload.borrow().master_gain_db };
                {
                    let mut pp = prog_player_reload.borrow_mut();
                    pp.set_prerender_project_path(
                        project_file_path.as_deref(),
                        preferences_state_reload
                            .borrow()
                            .persist_prerenders_next_to_project_file,
                    );
                    pp.set_project_dimensions(proj_w, proj_h);
                    pp.set_frame_rate(fr_num, fr_den);
                    pp.load_clips(clips);
                    // Apply the project's Loudness Radar master gain so
                    // playback reflects the normalized mix.
                    pp.set_master_gain_db(master_gain_db);
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

    right_sidebar.append(&multicam_panel);

    let right_sidebar_default_split_pos =
        crate::ui_state::WorkspaceArrangement::default().right_sidebar_paned_pos;
    let transitions_last_visible_split_pos = Rc::new(Cell::new(right_sidebar_default_split_pos));
    let right_sidebar_paned = Paned::new(Orientation::Vertical);
    right_sidebar_paned.set_vexpand(true);
    right_sidebar_paned.set_hexpand(true);
    right_sidebar_paned.set_position(right_sidebar_default_split_pos);
    right_sidebar_paned.set_resize_start_child(true);
    right_sidebar_paned.set_shrink_start_child(false);
    right_sidebar_paned.set_resize_end_child(true);
    right_sidebar_paned.set_shrink_end_child(false);

    let inspector_scroll = ScrolledWindow::new();
    inspector_scroll.set_vexpand(true);
    inspector_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    inspector_scroll.set_child(Some(&inspector_box));

    let transitions_header = gtk::Box::new(Orientation::Horizontal, 6);
    let transitions_title = gtk::Label::new(Some("Transitions"));
    transitions_title.set_halign(gtk::Align::Start);
    transitions_title.set_hexpand(true);
    let transitions_toggle = gtk::Button::with_label("Hide Transitions");
    transitions_toggle.add_css_class("small-btn");
    transitions_header.append(&transitions_title);
    transitions_header.append(&transitions_toggle);

    let transitions_revealer = gtk::Revealer::new();
    transitions_revealer.set_reveal_child(true);
    transitions_revealer.set_vexpand(true);
    let transitions_list = gtk::ListBox::new();
    transitions_list.add_css_class("boxed-list");
    transitions_list.set_selection_mode(gtk::SelectionMode::None);
    let transitions_scroll = ScrolledWindow::new();
    transitions_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    transitions_scroll.set_min_content_height(120);
    transitions_scroll.set_vexpand(true);
    transitions_scroll.set_child(Some(&transitions_list));

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

    for transition in supported_transition_definitions() {
        add_transition_row(&transitions_list, transition.label, transition.kind);
    }

    let transitions_section = gtk::Box::new(Orientation::Vertical, 6);
    transitions_section.set_vexpand(true);
    transitions_section.append(&transitions_header);
    transitions_revealer.set_child(Some(&transitions_scroll));
    transitions_section.append(&transitions_revealer);
    right_sidebar_paned.set_start_child(Some(&inspector_scroll));
    right_sidebar_paned.set_end_child(Some(&transitions_section));
    right_sidebar.append(&right_sidebar_paned);

    // ── Keyframe dopesheet (resizable via Paned) ───────────────────────────
    let dopesheet_on_seek: Rc<dyn Fn(u64)> = {
        let timeline_state = timeline_state.clone();
        let prog_player = prog_player.clone();
        let inspector_view = inspector_view.clone();
        let project = project.clone();
        let timeline_panel_cell = timeline_panel_cell.clone();
        Rc::new(move |ns: u64| {
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
        })
    };
    let (keyframe_editor_widget, keyframe_editor_view) = keyframe_editor::build_keyframe_editor(
        project.clone(),
        timeline_state.clone(),
        on_project_changed.clone(),
        dopesheet_on_seek.clone(),
    );
    keyframe_editor_widget.set_size_request(-1, 120);
    // Wrap in a vertical-only ScrolledWindow so the dopesheet is usable on
    // small displays. The DrawingArea's own EventControllerScroll (pan/zoom)
    // fires first (target phase, returns Stop) so the outer scroller won't
    // intercept those events.
    let keyframe_scroller = ScrolledWindow::new();
    keyframe_scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    keyframe_scroller.set_child(Some(&keyframe_editor_widget));

    // Transcript panel — peer of the dopesheet, sharing the same vertical
    // slot in `timeline_paned` via a `gtk::Stack`. The Stack lets the user
    // toggle between Keyframes and Transcript without ever showing both at
    // once (cleaner on laptops than a 3-way vertical split).
    let (transcript_widget, transcript_view) = crate::ui::transcript_panel::build_transcript_panel(
        project.clone(),
        timeline_state.clone(),
        on_project_changed.clone(),
        dopesheet_on_seek,
    );
    transcript_widget.set_size_request(-1, 120);
    let transcript_scroller = ScrolledWindow::new();
    transcript_scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    transcript_scroller.set_child(Some(&transcript_widget));

    let bottom_panel_stack = gtk::Stack::new();
    bottom_panel_stack.set_transition_type(gtk::StackTransitionType::None);
    bottom_panel_stack.add_named(&keyframe_scroller, Some("keyframes"));
    bottom_panel_stack.add_named(&transcript_scroller, Some("transcript"));
    bottom_panel_stack.set_visible_child_name("keyframes");
    bottom_panel_stack.set_visible(false);
    timeline_paned.set_end_child(Some(&bottom_panel_stack));
    // Default split: allocate ~70% to timeline, ~30% to dopesheet.
    // We'll set a reasonable initial position after the first allocation.
    {
        let paned = timeline_paned.clone();
        paned.connect_map(move |p| {
            let total = p.allocation().height();
            if total > 0 && p.position() == 0 {
                p.set_position((total as f64 * 0.7) as i32);
            }
        });
    }
    *keyframe_editor_cell.borrow_mut() = Some(keyframe_editor_view);
    *transcript_panel_cell.borrow_mut() = Some(transcript_view);

    // Add spacer + toggle buttons to the track-management bar. Both toggles
    // share the same `bottom_panel_stack`: clicking one switches the visible
    // child to that panel (and ensures the stack is showing); clicking the
    // active panel's toggle hides the stack entirely.
    let keyframes_toggle = gtk::Button::with_label("Show Keyframes");
    keyframes_toggle.add_css_class("small-btn");
    let transcript_toggle = gtk::Button::with_label("Show Transcript");
    transcript_toggle.add_css_class("small-btn");
    if let Some(ref bar_widget) = timeline_bar_widget {
        if let Ok(bar) = bar_widget.clone().downcast::<gtk::Box>() {
            let spacer = gtk::Box::new(Orientation::Horizontal, 0);
            spacer.set_hexpand(true);
            bar.append(&spacer);
            bar.append(&keyframes_toggle);
            bar.append(&transcript_toggle);
        }
    }

    {
        let revealer = transitions_revealer.clone();
        let paned = right_sidebar_paned.clone();
        let header = transitions_header.clone();
        let last_visible_pos = transitions_last_visible_split_pos.clone();
        transitions_toggle.connect_clicked(move |btn| {
            let show = !revealer.reveals_child();
            if show {
                revealer.set_reveal_child(true);
                paned.set_position(clamp_workspace_paned_position(
                    &paned,
                    last_visible_pos.get(),
                ));
            } else {
                let current_pos = clamp_workspace_paned_position(&paned, paned.position());
                if current_pos > 0 {
                    last_visible_pos.set(current_pos);
                }
                revealer.set_reveal_child(false);
                let total = workspace_paned_extent(&paned);
                if total > 0 {
                    let header_height = header.measure(Orientation::Vertical, -1).0.max(1);
                    let collapsed_pos = total.saturating_sub(header_height);
                    paned.set_position(clamp_workspace_paned_position(&paned, collapsed_pos));
                }
            }
            btn.set_label(if show {
                "Hide Transitions"
            } else {
                "Show Transitions"
            });
        });
    }
    // Shared toggle handlers for the two bottom panels (Keyframes /
    // Transcript). Each toggle either:
    //   * activates its panel (and shows the stack if it was hidden), or
    //   * hides the stack entirely (clicking the active panel's toggle).
    // After mutating the stack, both buttons' labels are refreshed to match
    // the new visibility state.
    let refresh_bottom_toggle_labels: Rc<dyn Fn()> = {
        let stack = bottom_panel_stack.clone();
        let kf = keyframes_toggle.clone();
        let tr = transcript_toggle.clone();
        Rc::new(move || {
            let visible = stack.is_visible();
            let active = stack.visible_child_name();
            let kf_active =
                visible && active.as_deref() == Some("keyframes");
            let tr_active =
                visible && active.as_deref() == Some("transcript");
            kf.set_label(if kf_active {
                "Hide Keyframes"
            } else {
                "Show Keyframes"
            });
            tr.set_label(if tr_active {
                "Hide Transcript"
            } else {
                "Show Transcript"
            });
        })
    };
    {
        let stack = bottom_panel_stack.clone();
        let paned = timeline_paned.clone();
        let workspace_layouts_applying = workspace_layouts_applying.clone();
        let sync_workspace_layout_state = sync_workspace_layout_state.clone();
        let refresh_labels = refresh_bottom_toggle_labels.clone();
        keyframes_toggle.connect_clicked(move |_| {
            let active = stack.visible_child_name();
            if stack.is_visible() && active.as_deref() == Some("keyframes") {
                stack.set_visible(false);
            } else {
                stack.set_visible_child_name("keyframes");
                if !stack.is_visible() {
                    stack.set_visible(true);
                    let total = paned.allocation().height();
                    if total > 0 {
                        paned.set_position((total as f64 * 0.7) as i32);
                    }
                }
            }
            refresh_labels();
            if !workspace_layouts_applying.get() {
                sync_workspace_layout_state();
            }
        });
    }
    {
        let stack = bottom_panel_stack.clone();
        let paned = timeline_paned.clone();
        let workspace_layouts_applying = workspace_layouts_applying.clone();
        let sync_workspace_layout_state = sync_workspace_layout_state.clone();
        let refresh_labels = refresh_bottom_toggle_labels.clone();
        transcript_toggle.connect_clicked(move |_| {
            let active = stack.visible_child_name();
            if stack.is_visible() && active.as_deref() == Some("transcript") {
                stack.set_visible(false);
            } else {
                stack.set_visible_child_name("transcript");
                if !stack.is_visible() {
                    stack.set_visible(true);
                    let total = paned.allocation().height();
                    if total > 0 {
                        paned.set_position((total as f64 * 0.7) as i32);
                    }
                }
            }
            refresh_labels();
            if !workspace_layouts_applying.get() {
                sync_workspace_layout_state();
            }
        });
    }

    root_hpaned.set_end_child(Some(&right_sidebar));
    {
        let workspace_layouts_applying = workspace_layouts_applying.clone();
        let sync_workspace_layout_state = sync_workspace_layout_state.clone();
        root_hpaned.connect_position_notify(move |_| {
            if !workspace_layouts_applying.get() {
                sync_workspace_layout_state();
            }
        });
    }
    {
        let workspace_layouts_applying = workspace_layouts_applying.clone();
        let sync_workspace_layout_state = sync_workspace_layout_state.clone();
        let transitions_revealer = transitions_revealer.clone();
        let transitions_last_visible_split_pos = transitions_last_visible_split_pos.clone();
        right_sidebar_paned.connect_position_notify(move |paned| {
            if !transitions_revealer.reveals_child() {
                return;
            }
            let current_pos = clamp_workspace_paned_position(paned, paned.position());
            if current_pos > 0 {
                transitions_last_visible_split_pos.set(current_pos);
            }
            if !workspace_layouts_applying.get() {
                sync_workspace_layout_state();
            }
        });
    }
    {
        let workspace_layouts_applying = workspace_layouts_applying.clone();
        let sync_workspace_layout_state = sync_workspace_layout_state.clone();
        root_vpaned.connect_position_notify(move |_| {
            if !workspace_layouts_applying.get() {
                sync_workspace_layout_state();
            }
        });
    }
    {
        let workspace_layouts_applying = workspace_layouts_applying.clone();
        let sync_workspace_layout_state = sync_workspace_layout_state.clone();
        top_paned.connect_position_notify(move |_| {
            if !workspace_layouts_applying.get() {
                sync_workspace_layout_state();
            }
        });
    }
    {
        let workspace_layouts_applying = workspace_layouts_applying.clone();
        let sync_workspace_layout_state = sync_workspace_layout_state.clone();
        left_vpaned.connect_position_notify(move |_| {
            if !workspace_layouts_applying.get() {
                sync_workspace_layout_state();
            }
        });
    }
    {
        let workspace_layouts_applying = workspace_layouts_applying.clone();
        let sync_workspace_layout_state = sync_workspace_layout_state.clone();
        timeline_paned.connect_position_notify(move |_| {
            if !workspace_layouts_applying.get() {
                sync_workspace_layout_state();
            }
        });
    }

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
    status_progress.set_pulse_step(0.12);
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
    let background_render_toggle_updating = Rc::new(Cell::new(false));
    let background_render_icon = gtk::Image::from_icon_name(if initial_background_prerender {
        "system-run-symbolic"
    } else {
        "process-stop-symbolic"
    });
    let background_render_text =
        gtk::Label::new(Some(background_render_label(initial_background_prerender)));
    background_render_row.append(&background_render_icon);
    background_render_row.append(&background_render_text);
    background_render_toggle.set_child(Some(&background_render_row));
    background_render_toggle.add_css_class("round");
    background_render_toggle.add_css_class("flat");
    let proxy_quick_toggle = gtk::ToggleButton::with_label(proxy_toggle_label(&initial_proxy_mode));
    proxy_quick_toggle.set_active(initial_proxy_mode.is_enabled());
    proxy_quick_toggle.set_tooltip_text(Some(&proxy_toggle_tooltip(
        &initial_proxy_mode,
        &preferences_state.borrow().remembered_proxy_mode(),
    )));
    proxy_quick_toggle.add_css_class("round");
    proxy_quick_toggle.add_css_class("flat");
    {
        let toggle_proxy_quick = toggle_proxy_quick.clone();
        proxy_quick_toggle.connect_toggled(move |btn| toggle_proxy_quick(btn.is_active()));
    }
    // ── Media Browser toggle ──
    let media_browser_toggle = gtk::ToggleButton::new();
    media_browser_toggle.set_active(true);
    let media_browser_row = gtk::Box::new(Orientation::Horizontal, 4);
    let media_browser_icon = gtk::Image::from_icon_name("view-reveal-symbolic");
    let media_browser_text = gtk::Label::new(Some("Media Browser"));
    media_browser_row.append(&media_browser_icon);
    media_browser_row.append(&media_browser_text);
    media_browser_toggle.set_child(Some(&media_browser_row));
    media_browser_toggle.add_css_class("round");
    media_browser_toggle.add_css_class("flat");
    {
        let panel = left_vpaned.clone();
        let icon = media_browser_icon.clone();
        let workspace_layouts_applying = workspace_layouts_applying.clone();
        let sync_workspace_layout_state = sync_workspace_layout_state.clone();
        media_browser_toggle.connect_toggled(move |btn| {
            let show = btn.is_active();
            panel.set_visible(show);
            icon.set_icon_name(Some(if show {
                "view-reveal-symbolic"
            } else {
                "view-conceal-symbolic"
            }));
            if !workspace_layouts_applying.get() {
                sync_workspace_layout_state();
            }
        });
    }

    // ── Inspector toggle ──
    let inspector_toggle = gtk::ToggleButton::new();
    inspector_toggle.set_active(true);
    let inspector_toggle_row = gtk::Box::new(Orientation::Horizontal, 4);
    let inspector_toggle_icon = gtk::Image::from_icon_name("view-reveal-symbolic");
    let inspector_toggle_text = gtk::Label::new(Some("Inspector"));
    inspector_toggle_row.append(&inspector_toggle_icon);
    inspector_toggle_row.append(&inspector_toggle_text);
    inspector_toggle.set_child(Some(&inspector_toggle_row));
    inspector_toggle.add_css_class("round");
    inspector_toggle.add_css_class("flat");
    {
        let sidebar = right_sidebar.clone();
        let icon = inspector_toggle_icon.clone();
        let workspace_layouts_applying = workspace_layouts_applying.clone();
        let sync_workspace_layout_state = sync_workspace_layout_state.clone();
        inspector_toggle.connect_toggled(move |btn| {
            let show = btn.is_active();
            sidebar.set_visible(show);
            icon.set_icon_name(Some(if show {
                "view-reveal-symbolic"
            } else {
                "view-conceal-symbolic"
            }));
            if !workspace_layouts_applying.get() {
                sync_workspace_layout_state();
            }
        });
    }

    let workspace_layout_dropdown = gtk::DropDown::from_strings(&[]);
    workspace_layout_dropdown.set_hexpand(true);
    let workspace_apply_btn = gtk::Button::with_label("Apply");
    let workspace_save_btn = gtk::Button::with_label("Save Current…");
    let workspace_rename_btn = gtk::Button::with_label("Rename…");
    let workspace_delete_btn = gtk::Button::with_label("Delete");
    let workspace_reset_btn = gtk::Button::with_label("Reset");
    let workspace_popover_box = gtk::Box::new(Orientation::Vertical, 8);
    workspace_popover_box.set_margin_start(12);
    workspace_popover_box.set_margin_end(12);
    workspace_popover_box.set_margin_top(10);
    workspace_popover_box.set_margin_bottom(10);
    let workspace_title = gtk::Label::new(Some("Workspace Layouts"));
    workspace_title.set_halign(gtk::Align::Start);
    workspace_title.add_css_class("dim-label");
    workspace_popover_box.append(&workspace_title);
    workspace_popover_box.append(&workspace_layout_dropdown);
    let workspace_actions_row = gtk::Box::new(Orientation::Horizontal, 4);
    workspace_actions_row.append(&workspace_apply_btn);
    workspace_actions_row.append(&workspace_reset_btn);
    workspace_popover_box.append(&workspace_actions_row);
    let workspace_manage_row = gtk::Box::new(Orientation::Horizontal, 4);
    workspace_manage_row.append(&workspace_save_btn);
    workspace_manage_row.append(&workspace_rename_btn);
    workspace_manage_row.append(&workspace_delete_btn);
    workspace_popover_box.append(&workspace_manage_row);
    let workspace_popover = gtk::Popover::new();
    workspace_popover.set_child(Some(&workspace_popover_box));
    workspace_popover.set_autohide(true);
    let workspace_menu_btn = gtk::MenuButton::new();
    workspace_menu_btn.set_label("Workspace");
    workspace_menu_btn.set_popover(Some(&workspace_popover));
    workspace_menu_btn.set_tooltip_text(Some(
        "Apply, save, rename, or delete saved workspace layouts",
    ));

    status_bar.append(&media_browser_toggle);
    status_bar.append(&track_levels_toggle);
    status_bar.append(&background_render_toggle);
    status_bar.append(&proxy_quick_toggle);
    status_bar.append(&status_label);
    status_bar.append(&status_progress);
    let status_spacer = gtk::Box::new(Orientation::Horizontal, 0);
    status_spacer.set_hexpand(true);
    status_bar.append(&status_spacer);
    status_bar.append(&workspace_menu_btn);
    status_bar.append(&inspector_toggle);
    *sync_background_render_toggle_impl.borrow_mut() = Some({
        let background_render_toggle = background_render_toggle.clone();
        let background_render_toggle_updating = background_render_toggle_updating.clone();
        let background_render_icon = background_render_icon.clone();
        let background_render_text = background_render_text.clone();
        Rc::new(move |prefs: &crate::ui_state::PreferencesState| {
            background_render_toggle_updating.set(true);
            let enabled = prefs.background_prerender;
            if background_render_toggle.is_active() != enabled {
                background_render_toggle.set_active(enabled);
            }
            background_render_icon.set_icon_name(Some(if enabled {
                "system-run-symbolic"
            } else {
                "process-stop-symbolic"
            }));
            background_render_text.set_text(background_render_label(enabled));
            background_render_toggle_updating.set(false);
        })
    });
    *sync_proxy_toggle_impl.borrow_mut() = Some({
        let proxy_quick_toggle = proxy_quick_toggle.clone();
        let proxy_toggle_updating = proxy_toggle_updating.clone();
        Rc::new(move |prefs: &crate::ui_state::PreferencesState| {
            proxy_toggle_updating.set(true);
            let enabled = prefs.proxy_mode.is_enabled();
            if proxy_quick_toggle.is_active() != enabled {
                proxy_quick_toggle.set_active(enabled);
            }
            proxy_quick_toggle.set_label(proxy_toggle_label(&prefs.proxy_mode));
            proxy_quick_toggle.set_tooltip_text(Some(&proxy_toggle_tooltip(
                &prefs.proxy_mode,
                &prefs.remembered_proxy_mode(),
            )));
            proxy_toggle_updating.set(false);
        })
    });
    {
        let workspace_apply_btn = workspace_apply_btn.clone();
        let workspace_rename_btn = workspace_rename_btn.clone();
        let workspace_delete_btn = workspace_delete_btn.clone();
        let workspace_layout_controls_updating = workspace_layout_controls_updating.clone();
        let workspace_layouts_state = workspace_layouts_state.clone();
        workspace_layout_dropdown.connect_selected_notify(move |dropdown| {
            if workspace_layout_controls_updating.get() {
                return;
            }
            let selected = dropdown.selected();
            workspace_apply_btn.set_sensitive(selected != 0);
            let named_selected = selected >= 2
                && ((selected - 2) as usize) < workspace_layouts_state.borrow().layouts.len();
            workspace_rename_btn.set_sensitive(named_selected);
            workspace_delete_btn.set_sensitive(named_selected);
        });
    }
    *sync_workspace_layout_controls_impl.borrow_mut() = Some({
        let workspace_layout_dropdown = workspace_layout_dropdown.clone();
        let workspace_apply_btn = workspace_apply_btn.clone();
        let workspace_rename_btn = workspace_rename_btn.clone();
        let workspace_delete_btn = workspace_delete_btn.clone();
        let workspace_layout_controls_updating = workspace_layout_controls_updating.clone();
        let workspace_layouts_state = workspace_layouts_state.clone();
        Rc::new(move || {
            workspace_layout_controls_updating.set(true);
            let (layout_names, active_layout) = {
                let state = workspace_layouts_state.borrow();
                (
                    state
                        .layouts
                        .iter()
                        .map(|layout| layout.name.clone())
                        .collect::<Vec<_>>(),
                    state.active_layout.clone(),
                )
            };
            let model = gtk::StringList::new(&[]);
            model.append("(Current)");
            model.append("Default Layout");
            for name in &layout_names {
                model.append(name);
            }
            workspace_layout_dropdown.set_model(Some(&model));
            let selected = active_layout
                .as_ref()
                .and_then(|name| {
                    layout_names
                        .iter()
                        .position(|candidate| candidate.eq_ignore_ascii_case(name))
                        .map(|idx| idx as u32 + 2)
                })
                .unwrap_or(0);
            workspace_layout_dropdown.set_selected(selected);
            workspace_apply_btn.set_sensitive(selected != 0);
            let named_selected = selected >= 2 && ((selected - 2) as usize) < layout_names.len();
            workspace_rename_btn.set_sensitive(named_selected);
            workspace_delete_btn.set_sensitive(named_selected);
            workspace_layout_controls_updating.set(false);
        })
    });
    let capture_workspace_arrangement: Rc<dyn Fn() -> crate::ui_state::WorkspaceArrangement> = {
        let root_hpaned = root_hpaned.clone();
        let root_vpaned = root_vpaned.clone();
        let top_paned = top_paned.clone();
        let left_vpaned = left_vpaned.clone();
        let timeline_paned = timeline_paned.clone();
        let media_browser_toggle = media_browser_toggle.clone();
        let inspector_toggle = inspector_toggle.clone();
        let keyframe_scroller = keyframe_scroller.clone();
        let scopes_btn = scopes_btn.clone();
        let docked_scopes_paned = docked_scopes_paned.clone();
        let monitor_state = monitor_state.clone();
        let monitor_popped = monitor_popped.clone();
        let popout_window_cell = popout_window_cell.clone();
        let tb_media = tb_media.clone();
        let tb_effects = tb_effects.clone();
        let tb_audio_fx = tb_audio_fx.clone();
        let tb_titles = tb_titles.clone();
        let workspace_layouts_state = workspace_layouts_state.clone();
        let right_sidebar_paned = right_sidebar_paned.clone();
        let transitions_revealer = transitions_revealer.clone();
        let transitions_last_visible_split_pos = transitions_last_visible_split_pos.clone();
        Rc::new(move || {
            let previous_arrangement = workspace_layouts_state.borrow().current.clone();
            let left_panel_tab = if tb_effects.is_active() {
                crate::ui_state::WorkspaceLeftPanelTab::Effects
            } else if tb_audio_fx.is_active() {
                crate::ui_state::WorkspaceLeftPanelTab::AudioEffects
            } else if tb_titles.is_active() {
                crate::ui_state::WorkspaceLeftPanelTab::Titles
            } else {
                let _ = tb_media.is_active();
                crate::ui_state::WorkspaceLeftPanelTab::Media
            };
            let monitor_snapshot = monitor_state.borrow().clone();
            let (width, height) = if monitor_popped.get() {
                if let Some(window) = popout_window_cell.borrow().as_ref() {
                    (window.width().max(320), window.height().max(180))
                } else {
                    (
                        monitor_snapshot.width.max(320),
                        monitor_snapshot.height.max(180),
                    )
                }
            } else {
                (
                    monitor_snapshot.width.max(320),
                    monitor_snapshot.height.max(180),
                )
            };
            let (root_hpaned_pos, root_hpaned_ratio_permille) = capture_workspace_paned_state(
                &root_hpaned,
                previous_arrangement.root_hpaned_pos,
                previous_arrangement.root_hpaned_ratio_permille,
            );
            let (root_vpaned_pos, root_vpaned_ratio_permille) = capture_workspace_paned_state(
                &root_vpaned,
                previous_arrangement.root_vpaned_pos,
                previous_arrangement.root_vpaned_ratio_permille,
            );
            let (top_paned_pos, top_paned_ratio_permille) = capture_workspace_paned_state(
                &top_paned,
                previous_arrangement.top_paned_pos,
                previous_arrangement.top_paned_ratio_permille,
            );
            let (left_vpaned_pos, left_vpaned_ratio_permille) = capture_workspace_paned_state(
                &left_vpaned,
                previous_arrangement.left_vpaned_pos,
                previous_arrangement.left_vpaned_ratio_permille,
            );
            let (timeline_paned_pos, timeline_paned_ratio_permille) = capture_workspace_paned_state(
                &timeline_paned,
                previous_arrangement.timeline_paned_pos,
                previous_arrangement.timeline_paned_ratio_permille,
            );
            let (right_sidebar_paned_pos, right_sidebar_paned_ratio_permille) =
                if transitions_revealer.reveals_child() {
                    capture_workspace_paned_state(
                        &right_sidebar_paned,
                        previous_arrangement.right_sidebar_paned_pos,
                        previous_arrangement.right_sidebar_paned_ratio_permille,
                    )
                } else {
                    let total = workspace_paned_extent(&right_sidebar_paned);
                    if total <= 0 {
                        (
                            previous_arrangement.right_sidebar_paned_pos,
                            previous_arrangement.right_sidebar_paned_ratio_permille,
                        )
                    } else {
                        let pos = clamp_workspace_paned_position(
                            &right_sidebar_paned,
                            transitions_last_visible_split_pos.get(),
                        );
                        (
                            pos,
                            crate::ui_state::workspace_split_ratio_from_pixels(pos, total),
                        )
                    }
                };
            crate::ui_state::WorkspaceArrangement {
                root_hpaned_pos,
                root_hpaned_ratio_permille,
                root_vpaned_pos,
                root_vpaned_ratio_permille,
                top_paned_pos,
                top_paned_ratio_permille,
                left_vpaned_pos,
                left_vpaned_ratio_permille,
                timeline_paned_pos,
                timeline_paned_ratio_permille,
                right_sidebar_paned_pos,
                right_sidebar_paned_ratio_permille,
                media_browser_visible: media_browser_toggle.is_active(),
                inspector_visible: inspector_toggle.is_active(),
                keyframe_editor_visible: keyframe_scroller.is_visible(),
                left_panel_tab,
                program_monitor: crate::ui_state::ProgramMonitorWorkspaceState {
                    popped: monitor_popped.get(),
                    width,
                    height,
                    docked_split_pos: if scopes_btn.is_active() {
                        docked_scopes_paned.position().max(160)
                    } else {
                        monitor_snapshot.docked_split_pos.max(160)
                    },
                    scopes_visible: scopes_btn.is_active(),
                },
            }
        })
    };
    *sync_workspace_layout_state_impl.borrow_mut() = Some({
        let workspace_layouts_state = workspace_layouts_state.clone();
        let capture_workspace_arrangement = capture_workspace_arrangement.clone();
        let sync_workspace_layout_controls = sync_workspace_layout_controls.clone();
        let workspace_layouts_applying = workspace_layouts_applying.clone();
        let workspace_layout_pending_name = workspace_layout_pending_name.clone();
        Rc::new(move || {
            if workspace_layouts_applying.get() {
                return;
            }
            let arrangement = capture_workspace_arrangement();
            {
                let pending_name = workspace_layout_pending_name.borrow_mut().take();
                let mut state = workspace_layouts_state.borrow_mut();
                if let Some(name) = pending_name {
                    if state
                        .upsert_layout(crate::ui_state::WorkspaceLayout {
                            name,
                            arrangement: arrangement.clone(),
                        })
                        .is_err()
                    {
                        state.set_current_arrangement(arrangement.clone());
                    }
                } else {
                    state.set_current_arrangement(arrangement.clone());
                }
                crate::ui_state::save_workspace_layouts_state(&state);
            }
            sync_workspace_layout_controls();
        })
    });
    *apply_workspace_arrangement_impl.borrow_mut() = Some({
        let root_hpaned = root_hpaned.clone();
        let root_vpaned = root_vpaned.clone();
        let top_paned = top_paned.clone();
        let left_vpaned = left_vpaned.clone();
        let timeline_paned = timeline_paned.clone();
        let media_browser_toggle = media_browser_toggle.clone();
        let inspector_toggle = inspector_toggle.clone();
        let keyframe_scroller = keyframe_scroller.clone();
        let keyframes_toggle = keyframes_toggle.clone();
        let scopes_btn = scopes_btn.clone();
        let docked_scopes_paned = docked_scopes_paned.clone();
        let monitor_state = monitor_state.clone();
        let monitor_popped = monitor_popped.clone();
        let popout_window_cell = popout_window_cell.clone();
        let on_toggle_popout = on_toggle_popout.clone();
        let tb_media = tb_media.clone();
        let tb_effects = tb_effects.clone();
        let tb_audio_fx = tb_audio_fx.clone();
        let tb_titles = tb_titles.clone();
        let workspace_layouts_applying = workspace_layouts_applying.clone();
        let workspace_layout_apply_generation = workspace_layout_apply_generation.clone();
        let workspace_layout_pending_name = workspace_layout_pending_name.clone();
        let sync_workspace_layout_state = sync_workspace_layout_state.clone();
        let right_sidebar_paned = right_sidebar_paned.clone();
        let transitions_last_visible_split_pos = transitions_last_visible_split_pos.clone();
        Rc::new(move |arrangement: crate::ui_state::WorkspaceArrangement| {
            workspace_layouts_applying.set(true);
            let apply_generation = workspace_layout_apply_generation.get().wrapping_add(1);
            workspace_layout_apply_generation.set(apply_generation);
            {
                let mut state = monitor_state.borrow_mut();
                arrangement
                    .program_monitor
                    .apply_to_program_monitor_state(&mut state);
                crate::ui_state::save_program_monitor_state(&state);
            }
            if media_browser_toggle.is_active() != arrangement.media_browser_visible {
                media_browser_toggle.set_active(arrangement.media_browser_visible);
            }
            if inspector_toggle.is_active() != arrangement.inspector_visible {
                inspector_toggle.set_active(arrangement.inspector_visible);
            }
            match arrangement.left_panel_tab {
                crate::ui_state::WorkspaceLeftPanelTab::Media => tb_media.set_active(true),
                crate::ui_state::WorkspaceLeftPanelTab::Effects => tb_effects.set_active(true),
                crate::ui_state::WorkspaceLeftPanelTab::AudioEffects => {
                    tb_audio_fx.set_active(true)
                }
                crate::ui_state::WorkspaceLeftPanelTab::Titles => tb_titles.set_active(true),
            }
            if keyframe_scroller.is_visible() != arrangement.keyframe_editor_visible {
                keyframe_scroller.set_visible(arrangement.keyframe_editor_visible);
            }
            keyframes_toggle.set_label(if arrangement.keyframe_editor_visible {
                "Hide Keyframes"
            } else {
                "Show Keyframes"
            });
            if scopes_btn.is_active() != arrangement.program_monitor.scopes_visible {
                scopes_btn.set_active(arrangement.program_monitor.scopes_visible);
            }
            if monitor_popped.get() != arrangement.program_monitor.popped {
                on_toggle_popout();
            } else if arrangement.program_monitor.popped {
                if let Some(window) = popout_window_cell.borrow().as_ref() {
                    window.set_default_size(
                        arrangement.program_monitor.width.max(320),
                        arrangement.program_monitor.height.max(180),
                    );
                }
            }
            let apply_split_positions: Rc<dyn Fn()> = Rc::new({
                let root_hpaned = root_hpaned.clone();
                let root_vpaned = root_vpaned.clone();
                let top_paned = top_paned.clone();
                let left_vpaned = left_vpaned.clone();
                let timeline_paned = timeline_paned.clone();
                let right_sidebar_paned = right_sidebar_paned.clone();
                let docked_scopes_paned = docked_scopes_paned.clone();
                let transitions_last_visible_split_pos = transitions_last_visible_split_pos.clone();
                let arrangement = arrangement.clone();
                move || {
                    if let Some(pos) = workspace_target_paned_position(
                        &root_hpaned,
                        arrangement.root_hpaned_pos,
                        arrangement.root_hpaned_ratio_permille,
                    ) {
                        root_hpaned.set_position(pos);
                    }
                    if let Some(pos) = workspace_target_paned_position(
                        &root_vpaned,
                        arrangement.root_vpaned_pos,
                        arrangement.root_vpaned_ratio_permille,
                    ) {
                        root_vpaned.set_position(pos);
                    }
                    if let Some(pos) = workspace_target_paned_position(
                        &top_paned,
                        arrangement.top_paned_pos,
                        arrangement.top_paned_ratio_permille,
                    ) {
                        top_paned.set_position(pos);
                    }
                    if let Some(pos) = workspace_target_paned_position(
                        &left_vpaned,
                        arrangement.left_vpaned_pos,
                        arrangement.left_vpaned_ratio_permille,
                    ) {
                        left_vpaned.set_position(pos);
                    }
                    if arrangement.keyframe_editor_visible {
                        if let Some(pos) = workspace_target_paned_position(
                            &timeline_paned,
                            arrangement.timeline_paned_pos,
                            arrangement.timeline_paned_ratio_permille,
                        ) {
                            timeline_paned.set_position(pos);
                        } else {
                            let total = timeline_paned.allocation().height();
                            if total > 0 {
                                timeline_paned.set_position((total as f64 * 0.7) as i32);
                            }
                        }
                    }
                    if arrangement.inspector_visible {
                        if let Some(pos) = workspace_target_paned_position(
                            &right_sidebar_paned,
                            arrangement.right_sidebar_paned_pos,
                            arrangement.right_sidebar_paned_ratio_permille,
                        ) {
                            transitions_last_visible_split_pos.set(pos);
                            right_sidebar_paned.set_position(pos);
                        }
                    }
                    if arrangement.program_monitor.scopes_visible {
                        docked_scopes_paned
                            .set_position(arrangement.program_monitor.docked_split_pos.max(160));
                    }
                }
            });
            apply_split_positions();
            let pane_positions_ready: Rc<dyn Fn() -> bool> = Rc::new({
                let root_hpaned = root_hpaned.clone();
                let root_vpaned = root_vpaned.clone();
                let top_paned = top_paned.clone();
                let left_vpaned = left_vpaned.clone();
                let timeline_paned = timeline_paned.clone();
                let arrangement = arrangement.clone();
                let right_sidebar_paned = right_sidebar_paned.clone();
                move || {
                    workspace_paned_extent(&root_hpaned) > 0
                        && workspace_paned_extent(&root_vpaned) > 0
                        && workspace_paned_extent(&top_paned) > 0
                        && workspace_paned_extent(&left_vpaned) > 0
                        && workspace_paned_extent(&timeline_paned) > 0
                        && (!arrangement.inspector_visible
                            || workspace_paned_extent(&right_sidebar_paned) > 0)
                }
            });
            schedule_workspace_layout_apply_completion(
                apply_generation,
                workspace_layout_apply_generation.clone(),
                workspace_layouts_applying.clone(),
                workspace_layout_pending_name.clone(),
                sync_workspace_layout_state.clone(),
                apply_split_positions.clone(),
                pane_positions_ready,
                20,
            );
        })
    });
    {
        let workspace_layouts_state = workspace_layouts_state.clone();
        let workspace_layout_dropdown = workspace_layout_dropdown.clone();
        let workspace_popover = workspace_popover.clone();
        let apply_workspace_arrangement = apply_workspace_arrangement.clone();
        let workspace_layout_pending_name = workspace_layout_pending_name.clone();
        workspace_apply_btn.connect_clicked(move |_| {
            let selected = workspace_layout_dropdown.selected();
            if selected == 0 {
                return;
            }
            let (arrangement, pending_name) = if selected == 1 {
                (crate::ui_state::WorkspaceArrangement::default(), None)
            } else {
                let state = workspace_layouts_state.borrow();
                let Some(layout) = state.layouts.get((selected - 2) as usize) else {
                    return;
                };
                (layout.arrangement.clone(), Some(layout.name.clone()))
            };
            *workspace_layout_pending_name.borrow_mut() = pending_name;
            apply_workspace_arrangement(arrangement);
            workspace_popover.popdown();
        });
    }
    {
        let apply_workspace_arrangement = apply_workspace_arrangement.clone();
        let workspace_popover = workspace_popover.clone();
        let workspace_layout_pending_name = workspace_layout_pending_name.clone();
        workspace_reset_btn.connect_clicked(move |_| {
            *workspace_layout_pending_name.borrow_mut() = None;
            apply_workspace_arrangement(crate::ui_state::WorkspaceArrangement::default());
            workspace_popover.popdown();
        });
    }
    {
        let window = window.clone();
        let capture_workspace_arrangement = capture_workspace_arrangement.clone();
        let workspace_layouts_state = workspace_layouts_state.clone();
        let sync_workspace_layout_controls = sync_workspace_layout_controls.clone();
        let workspace_popover = workspace_popover.clone();
        workspace_save_btn.connect_clicked(move |_| {
            present_text_entry_dialog(
                &window,
                "Save Workspace Layout",
                "Save",
                "Create or overwrite a named workspace layout.",
                "",
                Some("Editing"),
                Rc::new({
                    let capture_workspace_arrangement = capture_workspace_arrangement.clone();
                    let workspace_layouts_state = workspace_layouts_state.clone();
                    let sync_workspace_layout_controls = sync_workspace_layout_controls.clone();
                    let workspace_popover = workspace_popover.clone();
                    move |name| {
                        let arrangement = capture_workspace_arrangement();
                        {
                            let mut state = workspace_layouts_state.borrow_mut();
                            state.set_current_arrangement(arrangement.clone());
                            state.upsert_layout(crate::ui_state::WorkspaceLayout {
                                name,
                                arrangement,
                            })?;
                            crate::ui_state::save_workspace_layouts_state(&state);
                        }
                        sync_workspace_layout_controls();
                        workspace_popover.popdown();
                        Ok(())
                    }
                }),
            );
        });
    }
    {
        let window = window.clone();
        let workspace_layouts_state = workspace_layouts_state.clone();
        let workspace_layout_dropdown = workspace_layout_dropdown.clone();
        let sync_workspace_layout_controls = sync_workspace_layout_controls.clone();
        let workspace_popover = workspace_popover.clone();
        workspace_rename_btn.connect_clicked(move |_| {
            let selected = workspace_layout_dropdown.selected();
            if selected < 2 {
                return;
            }
            let current_name = {
                let state = workspace_layouts_state.borrow();
                state
                    .layouts
                    .get((selected - 2) as usize)
                    .map(|layout| layout.name.clone())
            };
            let Some(current_name) = current_name else {
                return;
            };
            let old_name_for_submit = current_name.clone();
            present_text_entry_dialog(
                &window,
                "Rename Workspace Layout",
                "Rename",
                "Rename the selected saved workspace layout.",
                &current_name,
                Some("Workspace name"),
                Rc::new({
                    let workspace_layouts_state = workspace_layouts_state.clone();
                    let sync_workspace_layout_controls = sync_workspace_layout_controls.clone();
                    let workspace_popover = workspace_popover.clone();
                    move |new_name| {
                        {
                            let mut state = workspace_layouts_state.borrow_mut();
                            state.rename_layout(&old_name_for_submit, &new_name)?;
                            crate::ui_state::save_workspace_layouts_state(&state);
                        }
                        sync_workspace_layout_controls();
                        workspace_popover.popdown();
                        Ok(())
                    }
                }),
            );
        });
    }
    {
        let workspace_layouts_state = workspace_layouts_state.clone();
        let workspace_layout_dropdown = workspace_layout_dropdown.clone();
        let sync_workspace_layout_controls = sync_workspace_layout_controls.clone();
        workspace_delete_btn.connect_clicked(move |_| {
            let selected = workspace_layout_dropdown.selected();
            if selected < 2 {
                return;
            }
            let deleted = {
                let mut state = workspace_layouts_state.borrow_mut();
                let Some(name) = state
                    .layouts
                    .get((selected - 2) as usize)
                    .map(|layout| layout.name.clone())
                else {
                    return;
                };
                let deleted = state.delete_layout(&name);
                if deleted {
                    crate::ui_state::save_workspace_layouts_state(&state);
                }
                deleted
            };
            if deleted {
                sync_workspace_layout_controls();
            }
        });
    }
    sync_workspace_layout_controls();

    // Wrap main content + status bar in a vertical box
    let outer_vbox = gtk::Box::new(Orientation::Vertical, 0);
    outer_vbox.append(&root_hpaned);
    outer_vbox.append(&status_bar);

    // Welcome/editor stack — show welcome on fresh launch, editor when a project is loaded.
    let main_stack = gtk::Stack::new();
    main_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    main_stack.set_transition_duration(200);
    *on_show_editor_impl.borrow_mut() = Some({
        let main_stack = main_stack.clone();
        Rc::new(move || {
            main_stack.set_visible_child_name("editor");
        })
    });

    let welcome_panel = {
        let project = project.clone();
        let on_project_changed = on_project_changed.clone();
        let timeline_state_for_welcome = timeline_state.clone();
        let window_for_welcome = window.clone();
        let stack_for_new = main_stack.clone();
        let stack_for_open = main_stack.clone();
        let stack_for_recent = main_stack.clone();

        crate::ui::welcome::build_welcome_panel(
            // on_new_project
            Rc::new({
                let stack = stack_for_new;
                move || {
                    stack.set_visible_child_name("editor");
                }
            }),
            // on_open_project
            Rc::new({
                let project = project.clone();
                let timeline_state = timeline_state_for_welcome.clone();
                let on_project_changed = on_project_changed.clone();
                let window = window_for_welcome.clone();
                let stack = stack_for_open;
                move || {
                    let dialog = gtk::FileDialog::new();
                    dialog.set_title("Open Project");
                    let filter = gtk::FileFilter::new();
                    filter.add_pattern("*.uspxml");
                    filter.add_pattern("*.fcpxml");
                    filter.add_pattern("*.xml");
                    filter.add_pattern("*.otio");
                    filter.set_name(Some("Project Files"));
                    let filters = gtk4::gio::ListStore::new::<gtk::FileFilter>();
                    filters.append(&filter);
                    dialog.set_filters(Some(&filters));
                    let project = project.clone();
                    let timeline_state = timeline_state.clone();
                    let on_project_changed = on_project_changed.clone();

                    let stack = stack.clone();
                    dialog.open(Some(&window), gtk4::gio::Cancellable::NONE, move |result| {
                        if let Ok(file) = result {
                            if let Some(path) = file.path() {
                                let path_str = path.to_string_lossy().to_string();
                                let (tx, rx) = std::sync::mpsc::sync_channel::<
                                    Result<crate::model::project::Project, String>,
                                >(1);
                                let path_bg = path.clone();
                                std::thread::spawn(move || {
                                    let result =
                                        crate::ui::project_loader::load_project_from_path(&path_bg);
                                    let _ = tx.send(result);
                                });
                                let project = project.clone();
                                let on_project_changed = on_project_changed.clone();

                                let timeline_state = timeline_state.clone();
                                let stack = stack.clone();
                                timeline_state.borrow_mut().loading = true;
                                glib::timeout_add_local(
                                    std::time::Duration::from_millis(50),
                                    move || match rx.try_recv() {
                                        Ok(Ok(mut new_proj)) => {
                                            new_proj.dirty = false;
                                            new_proj.file_path = Some(path_str.clone());
                                            crate::recent::push(&path_str);
                                            *project.borrow_mut() = new_proj;
                                            timeline_state.borrow_mut().loading = false;

                                            on_project_changed();
                                            stack.set_visible_child_name("editor");
                                            glib::ControlFlow::Break
                                        }
                                        Ok(Err(e)) => {
                                            log::error!("Failed to open project: {e}");
                                            timeline_state.borrow_mut().loading = false;
                                            glib::ControlFlow::Break
                                        }
                                        Err(std::sync::mpsc::TryRecvError::Empty) => {
                                            glib::ControlFlow::Continue
                                        }
                                        Err(_) => {
                                            timeline_state.borrow_mut().loading = false;
                                            glib::ControlFlow::Break
                                        }
                                    },
                                );
                            }
                        }
                    });
                }
            }),
            // on_open_recent
            Rc::new({
                let project = project.clone();
                let timeline_state = timeline_state_for_welcome.clone();
                let on_project_changed = on_project_changed.clone();

                let stack = stack_for_recent;
                move |path_str: String| {
                    let (tx, rx) = std::sync::mpsc::sync_channel::<
                        Result<crate::model::project::Project, String>,
                    >(1);
                    let path_bg = std::path::PathBuf::from(&path_str);
                    std::thread::spawn(move || {
                        let result = crate::ui::project_loader::load_project_from_path(&path_bg)
                            .map_err(|e| format!("Failed to open recent project: {e}"));
                        let _ = tx.send(result);
                    });
                    let project = project.clone();
                    let on_project_changed = on_project_changed.clone();

                    let timeline_state = timeline_state.clone();
                    let stack = stack.clone();
                    timeline_state.borrow_mut().loading = true;
                    glib::timeout_add_local(std::time::Duration::from_millis(50), move || match rx
                        .try_recv()
                    {
                        Ok(Ok(mut new_proj)) => {
                            new_proj.dirty = false;
                            new_proj.file_path = Some(path_str.clone());
                            crate::recent::push(&path_str);
                            *project.borrow_mut() = new_proj;
                            timeline_state.borrow_mut().loading = false;
                            on_project_changed();
                            stack.set_visible_child_name("editor");
                            glib::ControlFlow::Break
                        }
                        Ok(Err(e)) => {
                            log::error!("Failed to open recent project: {e}");
                            timeline_state.borrow_mut().loading = false;
                            glib::ControlFlow::Break
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                        Err(_) => {
                            timeline_state.borrow_mut().loading = false;
                            glib::ControlFlow::Break
                        }
                    });
                }
            }),
        )
    };

    main_stack.add_named(&welcome_panel, Some("welcome"));
    main_stack.add_named(&outer_vbox, Some("editor"));
    // Show welcome on fresh launch (no startup project), editor otherwise.
    if startup_project_path.is_some() {
        main_stack.set_visible_child_name("editor");
    } else {
        main_stack.set_visible_child_name("welcome");
    }
    window.set_child(Some(&main_stack));

    // ── Plugin discovery (deferred to avoid blocking startup) ──────────
    glib::idle_add_local_once(move || {
        if gstreamer::init().is_ok() {
            let registry =
                Rc::new(crate::media::frei0r_registry::Frei0rRegistry::get_or_discover().clone());
            set_effects_registry(registry);
            let ladspa_reg =
                Rc::new(crate::media::ladspa_registry::LadspaRegistry::get_or_discover().clone());
            set_ladspa_registry(ladspa_reg);
        }
    });

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
        let apply_preferences_state = apply_preferences_state.clone();
        let background_render_toggle_updating = background_render_toggle_updating.clone();
        background_render_toggle.connect_toggled(move |btn| {
            if background_render_toggle_updating.get() {
                return;
            }
            let mut new_state = preferences_state.borrow().clone();
            new_state.background_prerender = btn.is_active();
            apply_preferences_state(new_state);
        });
    }

    {
        let proxy_cache = proxy_cache.clone();
        let bg_removal_cache = bg_removal_cache.clone();
        let voice_enhance_cache = voice_enhance_cache.clone();
        let on_project_changed_voice_enhance = on_project_changed.clone();
        let frame_interp_cache = frame_interp_cache.clone();
        let stt_cache = stt_cache.clone();
        let tracking_cache = tracking_cache.clone();
        let project_for_stt = project.clone();
        let project_for_tracking = project.clone();
        let prog_player = prog_player.clone();
        let effective_proxy_enabled = effective_proxy_enabled.clone();
        let status_label = status_label.clone();
        let status_progress = status_progress.clone();
        let player = player.clone();
        let source_marks = source_marks.clone();
        let audio_sync_in_progress = audio_sync_in_progress.clone();
        let silence_detect_in_progress = silence_detect_in_progress.clone();
        let scene_detect_in_progress = scene_detect_in_progress.clone();
        let match_audio_in_progress = match_audio_in_progress.clone();
        let music_gen_cache = music_gen_cache.clone();
        let project_for_music = project.clone();
        let timeline_state_music = timeline_state.clone();
        let timeline_panel_cell_music = timeline_panel_cell.clone();
        let on_project_changed_music = on_project_changed.clone();
        let on_project_changed_tracking = on_project_changed.clone();
        let window_weak_music = window.downgrade();
        let inspector_view = inspector_view.clone();
        let preferences_state = preferences_state.clone();
        let timeline_state_stt = timeline_state.clone();
        let tracking_job_owner_by_key = tracking_job_owner_by_key.clone();
        let tracking_job_key_by_clip = tracking_job_key_by_clip.clone();
        let tracking_status_by_clip = tracking_status_by_clip.clone();
        let sync_tracking_controls = sync_tracking_controls.clone();
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
                inspector_view
                    .stt_model_available
                    .set(stt_cache.borrow().is_available());
            }
            // Poll voice-enhance prerender cache. When a new file becomes
            // ready we push the updated path map AND trigger a project-
            // changed cycle so the slot rebuild swaps in the cached file
            // immediately — otherwise the user has to scrub or replay
            // before they hear the effect.
            {
                let ve_resolved = voice_enhance_cache.borrow_mut().poll();
                let any_paths = !voice_enhance_cache.borrow().paths.is_empty();
                if !ve_resolved.is_empty() || any_paths {
                    let paths = voice_enhance_cache.borrow().paths.clone();
                    prog_player.borrow_mut().update_voice_enhance_paths(paths);
                }
                if !ve_resolved.is_empty() {
                    on_project_changed_voice_enhance();
                }
            }
            // Poll AI frame-interpolation cache. New sidecars are pushed to
            // the Program Monitor as a clip-id-keyed snapshot so the next
            // decoder rebuild will swap them in.
            {
                let interp_resolved = frame_interp_cache.borrow_mut().poll();
                if !interp_resolved.is_empty() {
                    let proj = project_for_stt.borrow();
                    let snapshot = frame_interp_cache.borrow().snapshot_paths_by_clip_id(&proj);
                    prog_player.borrow_mut().update_frame_interp_paths(snapshot);
                }
                // Toggle the "AI Interpolation (RIFE)" dropdown entry in
                // sync with the on-disk model. So users can drop the model
                // file in and have the option appear without restarting.
                // Re-stat the model path each tick (cheap) so the cache's
                // `is_available()` reflects current filesystem state, not
                // what was true at startup.
                {
                    use gtk::prelude::ListModelExt;
                    frame_interp_cache.borrow_mut().refresh_model_path();
                    let model_ready = frame_interp_cache.borrow().is_available();
                    let dropdown_has_ai = inspector_view.slow_motion_has_ai.get();
                    if model_ready && !dropdown_has_ai {
                        inspector_view
                            .slow_motion_model
                            .append("AI Interpolation (RIFE)");
                        inspector_view.slow_motion_has_ai.set(true);
                    } else if !model_ready && dropdown_has_ai {
                        let n = inspector_view.slow_motion_model.n_items();
                        if n > 0 {
                            // Drop the most recently appended entry (the AI
                            // option). If the user had it selected, GTK
                            // automatically falls back to the previous
                            // valid index.
                            inspector_view.slow_motion_model.remove(n - 1);
                        }
                        inspector_view.slow_motion_has_ai.set(false);
                    }
                }
                // Refresh the Inspector status row from the currently
                // selected clip's cache state. Cheap — the cache lookup is
                // a HashMap probe; the Label is only updated if the text
                // actually changes (GTK no-op when identical).
                use crate::media::frame_interp_cache::FrameInterpStatus;
                let status_text = {
                    let proj = project_for_stt.borrow();
                    let selected = inspector_view.selected_clip_id.borrow().clone();
                    selected
                        .and_then(|id| proj.clip_ref(&id).cloned())
                        .map(|clip| {
                            match frame_interp_cache.borrow().status_for_clip(&clip) {
                                FrameInterpStatus::NotApplicable => "",
                                FrameInterpStatus::ModelMissing => {
                                    "AI model not installed (Preferences → Models)"
                                }
                                FrameInterpStatus::Generating => "AI sidecar: Generating…",
                                FrameInterpStatus::Ready => "AI sidecar: Ready",
                                FrameInterpStatus::Failed => "AI sidecar: generation failed",
                            }
                            .to_string()
                        })
                };
                let text = status_text.unwrap_or_default();
                if inspector_view.frame_interp_status.text() != text.as_str() {
                    inspector_view.frame_interp_status.set_text(&text);
                    inspector_view
                        .frame_interp_status
                        .set_visible(!text.is_empty());
                }
            }
            // Refresh the per-clip Voice Enhance status row from the
            // currently selected clip's cache state. Mirrors the frame
            // interp status pattern above. The Retry button is only
            // visible when the cache reports `Failed` for the current
            // (source_path, strength) pair.
            {
                use crate::media::voice_enhance_cache::VoiceEnhanceStatus;
                let (text, show_retry) = {
                    let proj = project_for_stt.borrow();
                    let selected = inspector_view.selected_clip_id.borrow().clone();
                    let clip = selected.and_then(|id| proj.clip_ref(&id).cloned());
                    match clip {
                        Some(c) if c.voice_enhance => {
                            let status = voice_enhance_cache
                                .borrow()
                                .status(&c.source_path, c.voice_enhance_strength);
                            match status {
                                VoiceEnhanceStatus::Idle => ("".to_string(), false),
                                VoiceEnhanceStatus::Pending => {
                                    ("Generating enhanced audio…".to_string(), false)
                                }
                                VoiceEnhanceStatus::Ready => {
                                    ("Enhanced audio ready".to_string(), false)
                                }
                                VoiceEnhanceStatus::Failed => (
                                    "Voice enhance failed — click Retry".to_string(),
                                    true,
                                ),
                            }
                        }
                        _ => ("".to_string(), false),
                    }
                };
                if inspector_view.voice_enhance_status_label.text() != text.as_str() {
                    inspector_view.voice_enhance_status_label.set_text(&text);
                    inspector_view
                        .voice_enhance_status_label
                        .set_visible(!text.is_empty());
                }
                if inspector_view.voice_enhance_retry_btn.is_visible() != show_retry {
                    inspector_view
                        .voice_enhance_retry_btn
                        .set_visible(show_retry);
                }
            }
            // Poll STT cache — apply generated subtitles via undo system.
            {
                let stt_results = stt_cache.borrow_mut().poll();
                if !stt_results.is_empty() {
                    for result in stt_results {
                        // Find the matching clip (recursively, including inside compounds).
                        let proj = project_for_stt.borrow();
                        fn find_stt_clip(
                            tracks: &[crate::model::track::Track],
                            source_path: &str,
                            source_in: u64,
                            source_out: u64,
                        ) -> Option<(String, String, Vec<crate::model::clip::SubtitleSegment>)>
                        {
                            for track in tracks {
                                for clip in &track.clips {
                                    if clip.source_path == source_path
                                        && clip.source_in == source_in
                                        && clip.source_out == source_out
                                    {
                                        return Some((
                                            clip.id.clone(),
                                            track.id.clone(),
                                            clip.subtitle_segments.clone(),
                                        ));
                                    }
                                    if let Some(ref inner) = clip.compound_tracks {
                                        if let Some(found) =
                                            find_stt_clip(inner, source_path, source_in, source_out)
                                        {
                                            return Some(found);
                                        }
                                    }
                                }
                            }
                            None
                        }
                        let found = find_stt_clip(
                            &proj.tracks,
                            &result.source_path,
                            result.source_in_ns,
                            result.source_out_ns,
                        );
                        if found.is_none() {
                            log::warn!(
                                "STT result: could not find clip for source={} in={} out={} ({} segments lost)",
                                result.source_path,
                                result.source_in_ns,
                                result.source_out_ns,
                                result.segments.len(),
                            );
                        }
                        drop(proj);

                        if let Some((clip_id, track_id, old_segments)) = found {
                            log::info!(
                                "STT result: writing {} segments to clip {} (track {})",
                                result.segments.len(),
                                clip_id,
                                track_id,
                            );
                            let cmd = crate::undo::GenerateSubtitlesCommand {
                                clip_id: clip_id.clone(),
                                track_id,
                                old_segments,
                                new_segments: result.segments,
                            };
                            cmd.execute(&mut project_for_stt.borrow_mut());
                            // Verify the write succeeded
                            {
                                let proj = project_for_stt.borrow();
                                let sub_count = proj
                                    .clip_ref(&clip_id)
                                    .map(|c| c.subtitle_segments.len())
                                    .unwrap_or(0);
                                log::info!(
                                    "STT result: after execute, clip {} has {} subtitle segments",
                                    clip_id,
                                    sub_count,
                                );
                            }
                            timeline_state_stt
                                .borrow_mut()
                                .history
                                .undo_stack
                                .push(Box::new(cmd));
                            timeline_state_stt.borrow_mut().history.redo_stack.clear();
                        }
                    }
                    // Clear generating state and force segment list rebuild.
                    inspector_view.stt_generating.set(false);
                    inspector_view
                        .subtitle_segments_snapshot
                        .borrow_mut()
                        .clear();
                }
                // Also clear if no jobs are pending (handles edge cases like failure).
                if !stt_cache.borrow().progress().in_flight {
                    inspector_view.stt_generating.set(false);
                }
                // Show/hide error label from last STT result.
                let stt_err = stt_cache.borrow().last_error.clone();
                if let Some(err) = stt_err {
                    inspector_view.subtitle_error_label.set_text(&err);
                    inspector_view.subtitle_error_label.set_visible(true);
                } else {
                    inspector_view.subtitle_error_label.set_visible(false);
                }
            }
            {
                let tracking_results = tracking_cache.borrow_mut().poll();
                let mut tracking_changed_project = false;
                for result in tracking_results {
                    let clip_id = tracking_job_owner_by_key
                        .borrow_mut()
                        .remove(&result.cache_key);
                    if let Some(ref clip_id) = clip_id {
                        tracking_job_key_by_clip.borrow_mut().remove(clip_id);
                    }
                    let Some(clip_id) = clip_id else {
                        continue;
                    };

                    if let Some(tracker) = result.tracker {
                        let mut proj = project_for_tracking.borrow_mut();
                        if upsert_motion_tracker_on_clip(&mut proj, &clip_id, tracker.clone()) {
                            tracking_status_by_clip.borrow_mut().insert(
                                clip_id.clone(),
                                (
                                    format!(
                                        "Tracking ready: {} samples loaded.",
                                        tracker.samples.len()
                                    ),
                                    false,
                                ),
                            );
                            tracking_changed_project = true;
                        } else {
                            tracking_status_by_clip.borrow_mut().insert(
                                clip_id.clone(),
                                ("Tracked clip no longer exists.".to_string(), true),
                            );
                        }
                    } else if result.canceled {
                        tracking_status_by_clip
                            .borrow_mut()
                            .insert(clip_id.clone(), ("Tracking canceled.".to_string(), false));
                    } else if let Some(error) = result.error {
                        tracking_status_by_clip
                            .borrow_mut()
                            .insert(clip_id.clone(), (error, true));
                    }
                }
                if tracking_changed_project {
                    on_project_changed_tracking();
                }
                sync_tracking_controls();
            }
            let proxy_progress = proxy_cache.borrow().progress();
            let prerender_progress = prog_player.borrow().background_prerender_progress();
            let bg_progress = bg_removal_cache.borrow().progress();
            let stt_progress = stt_cache.borrow().progress();
            let tracking_progress = tracking_cache.borrow().progress();
            let voice_enhance_progress = voice_enhance_cache.borrow().progress();
            let proxy_active = proxy_progress.in_flight;
            let prerender_active = prerender_progress.in_flight;
            let bg_active = bg_progress.in_flight;
            let stt_active = stt_progress.in_flight;
            let tracking_active = tracking_progress.in_flight;
            let voice_enhance_active = voice_enhance_progress.in_flight;
            let syncing_audio = audio_sync_in_progress.get();
            let detecting_silence = silence_detect_in_progress.get();
            let detecting_scene_cuts = scene_detect_in_progress.get();
            let matching_audio = match_audio_in_progress.get();
            // Poll music generation results and place completed clips.
            let music_progress = music_gen_cache.borrow().progress();
            let music_active = music_progress.in_flight;
            {
                let music_results = music_gen_cache.borrow_mut().poll();
                for result in music_results {
                    let mut error = None;
                    if result.success {
                        if !result.output_path.exists() {
                            error = Some("Generated audio file was not found.".to_string());
                        } else {
                            let clip = crate::model::clip::Clip::new(
                                result.output_path.to_string_lossy().as_ref(),
                                result.duration_ns,
                                result.timeline_start_ns,
                                crate::model::clip::ClipKind::Audio,
                            );
                            let track_snapshot = {
                                let proj = project_for_music.borrow();
                                proj.track_ref(&result.track_id)
                                    .map(|track| track.clips.clone())
                            };
                            if let Some(old_clips) = track_snapshot {
                                let mut new_clips = old_clips.clone();
                                new_clips.push(clip);
                                new_clips.sort_by_key(|c| c.timeline_start);
                                let mut ts = timeline_state_music.borrow_mut();
                                let proj_rc = ts.project.clone();
                                let mut proj = proj_rc.borrow_mut();
                                let cmd = crate::undo::SetTrackClipsCommand {
                                    track_id: result.track_id.clone(),
                                    old_clips,
                                    new_clips,
                                    label: "Generate music".to_string(),
                                };
                                ts.history.execute(Box::new(cmd), &mut proj);
                                proj.dirty = true;
                                ts.resolve_music_generation_overlay_success(&result.job_id);
                                drop(proj);
                                drop(ts);
                                on_project_changed_music();
                                if let Some(win) = window_weak_music.upgrade() {
                                    flash_window_status_title(
                                        &win,
                                        &project_for_music,
                                        "Music generation complete",
                                    );
                                }
                            } else {
                                error = Some("Target audio track no longer exists.".to_string());
                            }
                        }
                    } else {
                        error = Some(result.error.unwrap_or_else(|| "Unknown error".into()));
                    }

                    if let Some(err) = error {
                        timeline_state_music
                            .borrow_mut()
                            .mark_music_generation_overlay_failed(&result.job_id, err.clone());
                        log::error!("Music generation failed: {err}");
                        if let Some(win) = window_weak_music.upgrade() {
                            flash_window_status_title(
                                &win,
                                &project_for_music,
                                &format!("Music generation failed: {err}"),
                            );
                        }
                    }
                    if let Some(ref w) = *timeline_panel_cell_music.borrow() {
                        w.queue_draw();
                    }
                }
            }
            if proxy_active
                || prerender_active
                || syncing_audio
                || detecting_silence
                || detecting_scene_cuts
                || matching_audio
                || music_active
                || bg_active
                || tracking_active
                || stt_active
                || voice_enhance_active
            {
                status_label.set_visible(true);
                let mut parts = Vec::new();
                if syncing_audio {
                    parts.push("Syncing audio…".to_string());
                }
                if detecting_silence {
                    parts.push("Detecting silence\u{2026}".to_string());
                }
                if detecting_scene_cuts {
                    parts.push("Detecting scene cuts\u{2026}".to_string());
                }
                if matching_audio {
                    parts.push("Matching audio\u{2026}".to_string());
                }
                if music_active {
                    parts.push("Generating music\u{2026}".to_string());
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
                if tracking_active {
                    parts.push("Tracking motion…".to_string());
                }
                if stt_active {
                    parts.push("Generating subtitles…".to_string());
                }
                if voice_enhance_active {
                    parts.push(format!(
                        "Enhancing voice… {}/{}",
                        voice_enhance_progress.completed, voice_enhance_progress.total
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
                } else if tracking_active {
                    status_progress.set_visible(true);
                    let fraction = tracking_progress
                        .sample_fraction
                        .unwrap_or(0.0)
                        .clamp(0.0, 0.99);
                    status_progress.set_fraction(fraction);
                    status_progress.set_text(Some(&format!("{:.0}%", fraction * 100.0)));
                } else if matching_audio {
                    status_progress.set_visible(true);
                    status_progress.set_fraction(0.0);
                    status_progress.pulse();
                    status_progress.set_text(Some("Matching…"));
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
            log::info!("Server listening on stdio (JSON-RPC 2.0 / MCP 2024-11-05)");
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
        let workspace_layouts_state = workspace_layouts_state.clone();
        let proxy_cache = proxy_cache.clone();
        let bg_removal_cache = bg_removal_cache.clone();
        let frame_interp_cache = frame_interp_cache.clone();
        let tracking_cache_for_mcp = tracking_cache.clone();
        let tracking_job_owner_by_key_for_mcp = tracking_job_owner_by_key.clone();
        let tracking_job_key_by_clip_for_mcp = tracking_job_key_by_clip.clone();
        let on_close_preview = on_close_preview.clone();
        let source_marks = source_marks.clone();
        let on_source_selected = on_source_selected.clone();
        let on_project_changed = on_project_changed.clone();
        let capture_workspace_arrangement = capture_workspace_arrangement.clone();
        let apply_workspace_arrangement = apply_workspace_arrangement.clone();
        let sync_workspace_layout_controls = sync_workspace_layout_controls.clone();
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
        let stt_cache = stt_cache.clone();
        let music_gen_cache = music_gen_cache.clone();
        let main_stack_for_mcp = main_stack.clone();
        let window_weak = window.downgrade();
        let workspace_layout_pending_name_for_mcp = workspace_layout_pending_name.clone();
        MCP_MAIN_DISPATCH.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |cmd| {
                if let Some(win) = window_weak.upgrade() {
                    handle_mcp_command(
                        cmd,
                        &win,
                        &main_stack_for_mcp,
                        &project,
                        &library,
                        &player,
                        &prog_player,
                        &timeline_state,
                        &preferences_state,
                        &workspace_layouts_state,
                        &proxy_cache,
                        &bg_removal_cache,
                        &frame_interp_cache,
                        &stt_cache,
                        &music_gen_cache,
                        &tracking_cache_for_mcp,
                        &tracking_job_owner_by_key_for_mcp,
                        &tracking_job_key_by_clip_for_mcp,
                        &on_close_preview,
                        &source_marks,
                        &on_source_selected,
                        &on_project_changed_mcp_light,
                        &on_project_changed_mcp_full,
                        &capture_workspace_arrangement,
                        &apply_workspace_arrangement,
                        &workspace_layout_pending_name_for_mcp,
                        &sync_workspace_layout_controls,
                        &apply_preferences_state,
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
    // Also creates a versioned backup in $XDG_DATA_HOME/ultimateslice/backups/.
    {
        let project = project.clone();
        let library = library.clone();
        let window_weak = window.downgrade();
        let preferences_state = preferences_state.clone();
        glib::timeout_add_local(std::time::Duration::from_secs(60), move || {
            let is_dirty = project.borrow().dirty;
            if is_dirty {
                // Sync bin data before autosave.
                crate::model::media_library::sync_bins_to_project(
                    &library.borrow(),
                    &mut project.borrow_mut(),
                );
                let xml_result = {
                    let proj = project.borrow();
                    crate::fcpxml::writer::write_fcpxml(&proj)
                };
                if let Ok(ref xml) = xml_result {
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
                    // Versioned backup
                    let prefs = preferences_state.borrow();
                    if prefs.backup_enabled {
                        let proj_title = project.borrow().title.clone();
                        if let Err(e) = crate::project_versions::create_versioned_backup(
                            xml,
                            &proj_title,
                            prefs.backup_max_versions,
                        ) {
                            log::error!("Failed to write auto-backup: {e}");
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
                    if is_text_input_focused(&focused) {
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
                    if is_text_input_focused(&focused) {
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
                    if is_text_input_focused(&focused) {
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
                    if is_text_input_focused(&focused) {
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
    // ── Window-level Shift+P: toggle proxy playback ─────────────────────────
    {
        let toggle_proxy_quick = toggle_proxy_quick.clone();
        let preferences_state = preferences_state.clone();
        let key_ctrl = gtk4::EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        key_ctrl.connect_key_pressed(move |ctrl, key, _, mods| {
            use gtk4::gdk::{Key, ModifierType};
            if !mods.contains(ModifierType::SHIFT_MASK)
                || mods.contains(ModifierType::CONTROL_MASK)
                || mods.contains(ModifierType::ALT_MASK)
                || (key != Key::P && key != Key::p)
            {
                return glib::Propagation::Proceed;
            }
            if let Some(widget) = ctrl.widget() {
                if let Some(focused) = widget.root().and_then(|r| r.focus()) {
                    if is_text_input_focused(&focused) {
                        return glib::Propagation::Proceed;
                    }
                }
            }
            let enabled = !preferences_state.borrow().proxy_mode.is_enabled();
            toggle_proxy_quick(enabled);
            glib::Propagation::Stop
        });
        window.add_controller(key_ctrl);
    }
    // ── Window-level Ctrl+Shift+T: generate subtitles for selected clip ──
    {
        let stt_cache = stt_cache.clone();
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let inspector_view = inspector_view.clone();
        let key_ctrl = gtk4::EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        key_ctrl.connect_key_pressed(move |ctrl, key, _, mods| {
            use gtk4::gdk::{Key, ModifierType};
            if mods.contains(ModifierType::CONTROL_MASK)
                && mods.contains(ModifierType::SHIFT_MASK)
                && key == Key::T
            {
                // Skip if a text input is focused.
                if let Some(widget) = ctrl.widget() {
                    if let Some(focused) = widget.root().and_then(|r| r.focus()) {
                        if is_text_input_focused(&focused) {
                            return glib::Propagation::Proceed;
                        }
                    }
                }
                if !stt_cache.borrow().is_available() {
                    return glib::Propagation::Proceed;
                }
                let selected = timeline_state.borrow().selected_clip_id.clone();
                if let Some(ref clip_id) = selected {
                    let proj = project.borrow();
                    if let Some(clip) = proj.clip_ref(clip_id) {
                        if clip.subtitle_segments.is_empty() {
                            stt_cache.borrow_mut().request(
                                &clip.source_path,
                                clip.source_in,
                                clip.source_out,
                                "auto",
                            );
                            inspector_view.stt_generating.set(true);
                        }
                    }
                }
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
                    if is_text_input_focused(&focused) {
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
                    if is_text_input_focused(&focused) {
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

    let startup_workspace_arrangement = {
        let state = workspace_layouts_state.borrow();
        state.current.clone()
    };
    let startup_workspace_layout_name = {
        let state = workspace_layouts_state.borrow();
        state.active_layout.clone()
    };
    workspace_layouts_applying.set(true);
    {
        let startup_workspace_restore_pending = Rc::new(Cell::new(true));
        let apply_workspace_arrangement = apply_workspace_arrangement.clone();
        let workspace_layout_pending_name = workspace_layout_pending_name.clone();
        root_hpaned.connect_map(move |_| {
            if !startup_workspace_restore_pending.replace(false) {
                return;
            }
            *workspace_layout_pending_name.borrow_mut() = startup_workspace_layout_name.clone();
            apply_workspace_arrangement(startup_workspace_arrangement.clone());
        });
    }

    {
        let project = project.clone();
        let library = library.clone();
        let on_project_changed = on_project_changed.clone();
        let proxy_cache = proxy_cache.clone();
        let preferences_state = preferences_state.clone();
        let close_approved = Rc::new(Cell::new(false));
        let close_approved_for_signal = close_approved.clone();
        window.connect_close_request(move |w| {
            // Second pass through the handler — the deferred `win.close()`
            // scheduled by `on_continue` triggered a fresh close_request.
            // Cleanup already ran in on_continue, so just let the close
            // proceed.
            if close_approved_for_signal.get() {
                return glib::Propagation::Proceed;
            }
            let close_approved_for_continue = close_approved.clone();
            let proxy_cache_for_continue = proxy_cache.clone();
            let preferences_state_for_continue = preferences_state.clone();
            let weak = w.downgrade();
            let on_continue: Rc<dyn Fn()> = Rc::new(move || {
                close_approved_for_continue.set(true);
                let preserve_sidecar_proxies = {
                    let prefs = preferences_state_for_continue.borrow();
                    prefs.proxy_mode.is_enabled() && prefs.persist_proxies_next_to_original_media
                };
                proxy_cache_for_continue
                    .borrow_mut()
                    .cleanup_for_unload(preserve_sidecar_proxies);
                // Defer `win.close()` to the next main-loop iteration so the
                // original `close_request` handler can fully return Stop
                // before a fresh `close_request` is emitted. Otherwise
                // calling `win.close()` synchronously from inside the
                // close_request handler re-enters the handler recursively;
                // the inner invocation returns Proceed but the outer
                // invocation's Stop overrides it, leaving the window open
                // and forcing the user to click the close button a second
                // time.
                let weak = weak.clone();
                glib::idle_add_local_once(move || {
                    if let Some(win) = weak.upgrade() {
                        win.close();
                    }
                });
            });
            crate::ui::toolbar::confirm_unsaved_then(
                Some(w.clone().upcast::<gtk::Window>()),
                project.clone(),
                library.clone(),
                on_project_changed.clone(),
                on_continue,
            );
            glib::Propagation::Stop
        });
    }

    if let Some(path) = startup_project_path {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Project, String>>(1);
        let path_bg = std::path::PathBuf::from(&path);
        std::thread::spawn(move || {
            let result = crate::ui::project_loader::load_project_from_path(&path_bg)
                .map_err(|e| format!("Failed to open startup project: {e}"));
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
                    log::error!("{e}");
                    glib::ControlFlow::Break
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    timeline_state.borrow_mut().loading = false;
                    log::error!("Startup project open worker disconnected");
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
    main_stack: &gtk::Stack,
    project: &Rc<RefCell<Project>>,
    library: &Rc<RefCell<MediaLibrary>>,
    player: &Rc<RefCell<Player>>,
    prog_player: &Rc<RefCell<ProgramPlayer>>,
    timeline_state: &Rc<RefCell<TimelineState>>,
    preferences_state: &Rc<RefCell<crate::ui_state::PreferencesState>>,
    workspace_layouts_state: &Rc<RefCell<crate::ui_state::WorkspaceLayoutsState>>,
    proxy_cache: &Rc<RefCell<crate::media::proxy_cache::ProxyCache>>,
    bg_removal_cache: &Rc<RefCell<crate::media::bg_removal_cache::BgRemovalCache>>,
    frame_interp_cache: &Rc<RefCell<crate::media::frame_interp_cache::FrameInterpCache>>,
    stt_cache: &Rc<RefCell<crate::media::stt_cache::SttCache>>,
    music_gen_cache: &Rc<RefCell<crate::media::music_gen::MusicGenCache>>,
    tracking_cache: &Rc<RefCell<crate::media::tracking::TrackingCache>>,
    tracking_job_owner_by_key: &Rc<RefCell<HashMap<String, String>>>,
    tracking_job_key_by_clip: &Rc<RefCell<HashMap<String, String>>>,
    on_close_preview: &Rc<dyn Fn()>,
    source_marks: &Rc<RefCell<crate::model::media_library::SourceMarks>>,
    on_source_selected: &Rc<dyn Fn(String, u64)>,
    on_project_changed: &Rc<dyn Fn()>,
    on_project_changed_full: &Rc<dyn Fn()>,
    capture_workspace_arrangement: &Rc<dyn Fn() -> crate::ui_state::WorkspaceArrangement>,
    apply_workspace_arrangement: &Rc<dyn Fn(crate::ui_state::WorkspaceArrangement)>,
    workspace_layout_pending_name: &Rc<RefCell<Option<String>>>,
    sync_workspace_layout_controls: &Rc<dyn Fn()>,
    apply_preferences_state: &Rc<dyn Fn(crate::ui_state::PreferencesState)>,
    suppress_resume_on_next_reload: &Rc<Cell<bool>>,
    clear_media_browser_on_next_reload: &Rc<Cell<bool>>,
) {
    use crate::mcp::McpCommand;
    use serde_json::{json, Value};

    let sync_library_change = || {
        {
            let lib = library.borrow();
            let mut proj = project.borrow_mut();
            crate::model::media_library::sync_bins_to_project(&lib, &mut proj);
            proj.dirty = true;
        }
        on_project_changed_full();
    };

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
                                "anamorphic_desqueeze": c.anamorphic_desqueeze,
                                "opacity":          c.opacity,
                                "blend_mode":       c.blend_mode.label(),
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

        McpCommand::ListLadspaPlugins { reply } => {
            let reg = crate::media::ladspa_registry::LadspaRegistry::get_or_discover();
            let plugins: Vec<serde_json::Value> = reg
                .plugins
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "name": p.ladspa_name,
                        "display_name": p.display_name,
                        "description": p.description,
                        "category": p.category,
                        "gst_element_name": p.gst_element_name,
                        "params": p.params.iter().map(|param| serde_json::json!({
                            "name": param.name,
                            "display_name": param.display_name,
                            "default": param.default_value,
                            "min": param.min,
                            "max": param.max,
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect();
            reply
                .send(serde_json::json!({"plugins": plugins, "count": plugins.len()}))
                .ok();
        }

        McpCommand::AddClipLadspaEffect {
            clip_id,
            plugin_name,
            reply,
        } => {
            let reg = crate::media::ladspa_registry::LadspaRegistry::get_or_discover();
            if let Some(info) = reg.find_by_name(&plugin_name) {
                let effect = crate::model::clip::LadspaEffect::new(
                    &info.ladspa_name,
                    &info.gst_element_name,
                );
                let effect_id = effect.id.clone();
                let mut proj = project.borrow_mut();
                let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                    clip.ladspa_effects.push(effect);
                    true
                } else {
                    false
                };
                if found {
                    proj.dirty = true;
                }
                drop(proj);
                reply
                    .send(serde_json::json!({"success": found, "effect_id": effect_id}))
                    .ok();
                if found {
                    on_project_changed();
                }
            } else {
                reply
                    .send(serde_json::json!({"success": false, "error": format!("Plugin not found: {plugin_name}")}))
                    .ok();
            }
        }

        McpCommand::RemoveClipLadspaEffect {
            clip_id,
            effect_id,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                let before = clip.ladspa_effects.len();
                clip.ladspa_effects.retain(|e| e.id != effect_id);
                clip.ladspa_effects.len() < before
            } else {
                false
            };
            if found {
                proj.dirty = true;
            }
            drop(proj);
            reply.send(serde_json::json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipLadspaEffectParams {
            clip_id,
            effect_id,
            params,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                if let Some(effect) = clip.ladspa_effects.iter_mut().find(|e| e.id == effect_id) {
                    for (k, v) in &params {
                        effect.params.insert(k.clone(), *v);
                    }
                    true
                } else {
                    false
                }
            } else {
                false
            };
            if found {
                proj.dirty = true;
            }
            drop(proj);
            reply.send(serde_json::json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetTrackRole {
            track_id,
            role,
            reply,
        } => {
            let result = {
                let mut proj = project.borrow_mut();
                if let Some(track) = proj.track_mut(&track_id) {
                    let new_role = crate::model::track::AudioRole::from_str(&role);
                    track.audio_role = new_role;
                    let role_str = new_role.as_str().to_string();
                    drop(track);
                    proj.dirty = true;
                    Ok(role_str)
                } else {
                    Err(format!("Track id not found: {track_id}"))
                }
            };
            match result {
                Ok(applied_role) => {
                    reply
                        .send(serde_json::json!({"success": true, "track_id": track_id, "role": applied_role}))
                        .ok();
                    on_project_changed();
                }
                Err(message) => {
                    reply
                        .send(serde_json::json!({"success": false, "error": message}))
                        .ok();
                }
            }
        }

        McpCommand::SetTrackDuck {
            track_id,
            duck,
            reply,
        } => {
            let result = {
                let mut proj = project.borrow_mut();
                if let Some(track) = proj.track_mut(&track_id) {
                    track.duck = duck;
                    proj.dirty = true;
                    Ok(())
                } else {
                    Err(format!("Track id not found: {track_id}"))
                }
            };
            match result {
                Ok(()) => {
                    reply
                        .send(serde_json::json!({"success": true, "track_id": track_id, "duck": duck}))
                        .ok();
                    on_project_changed();
                }
                Err(message) => {
                    reply
                        .send(serde_json::json!({"success": false, "error": message}))
                        .ok();
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
                    "last_non_off_proxy_mode": prefs.remembered_proxy_mode().as_str(),
                    "persist_proxies_next_to_original_media": prefs.persist_proxies_next_to_original_media,
                    "show_timeline_preview": prefs.show_timeline_preview,
                    "show_track_audio_levels": prefs.show_track_audio_levels,
                    "gsk_renderer": prefs.gsk_renderer.as_str(),
                    "preview_quality": prefs.preview_quality.as_str(),
                    "experimental_preview_optimizations": prefs.experimental_preview_optimizations,
                    "realtime_preview": prefs.realtime_preview,
                    "background_prerender": prefs.background_prerender,
                    "prerender_preset": prefs.prerender_preset.as_str(),
                    "prerender_crf": prefs.prerender_crf,
                    "persist_prerenders_next_to_project_file": prefs.persist_prerenders_next_to_project_file,
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
            let mut new_state = preferences_state.borrow().clone();
            new_state.set_proxy_mode(parsed);
            apply_preferences_state(new_state.clone());
            reply
                .send(json!({
                    "success": true,
                    "proxy_mode": new_state.proxy_mode.as_str(),
                    "last_non_off_proxy_mode": new_state.remembered_proxy_mode().as_str()
                }))
                .ok();
        }

        McpCommand::SetProxySidecarPersistence { enabled, reply } => {
            let mut new_state = preferences_state.borrow().clone();
            new_state.persist_proxies_next_to_original_media = enabled;
            apply_preferences_state(new_state.clone());
            reply
                .send(json!({
                    "success": true,
                    "persist_proxies_next_to_original_media": enabled
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
            let mut new_state = preferences_state.borrow().clone();
            new_state.background_prerender = enabled;
            apply_preferences_state(new_state.clone());
            reply
                .send(json!({
                    "success": true,
                    "background_prerender": enabled
                }))
                .ok();
        }

        McpCommand::SetPrerenderQuality { preset, crf, reply } => {
            let parsed_preset = match preset.as_str() {
                "ultrafast" => crate::ui_state::PrerenderEncodingPreset::Ultrafast,
                "superfast" => crate::ui_state::PrerenderEncodingPreset::Superfast,
                "veryfast" => crate::ui_state::PrerenderEncodingPreset::Veryfast,
                "faster" => crate::ui_state::PrerenderEncodingPreset::Faster,
                "fast" => crate::ui_state::PrerenderEncodingPreset::Fast,
                "medium" => crate::ui_state::PrerenderEncodingPreset::Medium,
                _ => {
                    reply
                        .send(json!({
                            "success": false,
                            "error": "preset must be one of: ultrafast, superfast, veryfast, faster, fast, medium"
                        }))
                        .ok();
                    return;
                }
            };
            if crf > crate::ui_state::MAX_PRERENDER_CRF {
                reply
                    .send(json!({
                        "success": false,
                        "error": format!(
                            "crf must be between {} and {}",
                            crate::ui_state::MIN_PRERENDER_CRF,
                            crate::ui_state::MAX_PRERENDER_CRF
                        )
                    }))
                    .ok();
                return;
            }
            prog_player
                .borrow_mut()
                .set_prerender_quality(parsed_preset.clone(), crf);
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.set_prerender_quality(parsed_preset, crf);
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            reply
                .send(json!({
                    "success": true,
                    "prerender_preset": new_state.prerender_preset.as_str(),
                    "prerender_crf": new_state.prerender_crf
                }))
                .ok();
        }

        McpCommand::SetPrerenderProjectPersistence { enabled, reply } => {
            let mut new_state = preferences_state.borrow().clone();
            new_state.persist_prerenders_next_to_project_file = enabled;
            apply_preferences_state(new_state.clone());
            reply
                .send(json!({
                    "success": true,
                    "persist_prerenders_next_to_project_file": enabled
                }))
                .ok();
        }

        McpCommand::SetPreviewLuts { enabled, reply } => {
            let mut new_state = preferences_state.borrow().clone();
            new_state.preview_luts = enabled;
            apply_preferences_state(new_state.clone());
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
                lookup_source_placement_info(&lib.items, &proj, &source_path)
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
                        source_info.audio_channel_mode,
                        None,
                        source_info.is_animated_svg,
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

        McpCommand::ConvertLtcAudioToTimecode {
            clip_id,
            ltc_channel,
            frame_rate,
            reply,
        } => {
            let context = {
                let proj = project.borrow();
                let lib = library.borrow();
                resolve_ltc_conversion_context(&proj, &lib, &clip_id, frame_rate)
            };
            match context {
                Ok(context) => {
                    let prepared = crate::media::ltc::decode_ltc_from_clip(
                        &context.source_path,
                        context.source_in,
                        context.source_out,
                        ltc_channel,
                        &context.frame_rate,
                    )
                    .map(|decode| PreparedLtcConversion { context, decode });
                    match prepared {
                        Ok(prepared) => {
                            let applied = {
                                let mut proj = project.borrow_mut();
                                let mut lib = library.borrow_mut();
                                let mut marks = source_marks.borrow_mut();
                                apply_prepared_ltc_conversion(
                                    &mut proj,
                                    &mut lib,
                                    Some(&mut *marks),
                                    prepared,
                                )
                            };
                            let timecode_label = timecode::format_ns_as_timecode(
                                applied.source_timecode_base_ns,
                                &applied.frame_rate,
                            );
                            reply
                                .send(json!({
                                    "success": true,
                                    "clip_id": clip_id,
                                    "source_path": applied.source_path,
                                    "source_timecode_base_ns": applied.source_timecode_base_ns,
                                    "timecode": timecode_label,
                                    "resolved_ltc_channel": applied.resolved_channel.as_str(),
                                    "applied_audio_channel_mode": applied.applied_audio_channel_mode.map(|mode| mode.as_str()),
                                    "muted": applied.muted_clip_count > 0,
                                    "updated_clip_count": applied.updated_clip_count,
                                    "message": format_ltc_conversion_status(&applied),
                                }))
                                .ok();
                            on_project_changed();
                        }
                        Err(error) => {
                            reply.send(json!({"success": false, "error": error})).ok();
                        }
                    }
                }
                Err(error) => {
                    reply.send(json!({"success": false, "error": error})).ok();
                }
            }
        }

        McpCommand::SyncClipsByAudio {
            clip_ids,
            replace_audio,
            reply,
        } => {
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
                    let mut drift_corrections: HashMap<String, f64> = HashMap::new();
                    let mut all_confident = true;
                    for r in &sync_results {
                        let new_start = (anchor_timeline_start as i64 + r.offset_ns).max(0) as u64;
                        result_json.push(json!({
                            "clip_id": r.clip_id,
                            "offset_ns": r.offset_ns,
                            "confidence": r.confidence,
                            "new_timeline_start_ns": new_start,
                            "drift_speed": r.drift_speed,
                        }));
                        if r.confidence < 3.0 {
                            all_confident = false;
                        } else {
                            assignments.insert(r.clip_id.clone(), new_start);
                            if let Some(drift) = r.drift_speed {
                                drift_corrections.insert(r.clip_id.clone(), drift);
                            }
                        }
                    }
                    if all_confident && !assignments.is_empty() {
                        let mut proj = project.borrow_mut();
                        // Apply timeline position assignments and drift speed corrections.
                        for track in &mut proj.tracks {
                            for clip in &mut track.clips {
                                if let Some(&new_start) = assignments.get(&clip.id) {
                                    clip.timeline_start = new_start;
                                }
                                if let Some(&drift) = drift_corrections.get(&clip.id) {
                                    if (drift - 1.0).abs() > 1e-9 {
                                        clip.speed *= drift;
                                    }
                                }
                            }
                        }
                        // When replace_audio is set, link all clips and mute anchor's embedded audio.
                        if replace_audio && clip_ids.len() >= 2 {
                            let link_id = uuid::Uuid::new_v4().to_string();
                            let anchor_id = &clip_ids[0];
                            for track in &mut proj.tracks {
                                for clip in &mut track.clips {
                                    if clip_ids.contains(&clip.id) {
                                        clip.link_group_id = Some(link_id.clone());
                                    }
                                    // Mute anchor clip's embedded audio so external audio replaces it.
                                    if &clip.id == anchor_id
                                        && clip.kind == crate::model::clip::ClipKind::Video
                                    {
                                        clip.volume = 0.0;
                                    }
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
                            "replace_audio_applied": replace_audio && all_confident,
                            "results": result_json,
                        }))
                        .ok();
                }
            }
        }

        McpCommand::CopyClipColorGrade { clip_id, reply } => {
            let mut ts = timeline_state.borrow_mut();
            // Temporarily set selected clip for the copy operation
            let prev_selected = ts.selected_clip_id.clone();
            ts.selected_clip_id = Some(clip_id.clone());
            let ok = ts.copy_color_grade();
            ts.selected_clip_id = prev_selected;
            drop(ts);
            reply.send(json!({"success": ok})).ok();
        }

        McpCommand::PasteClipColorGrade { clip_id, reply } => {
            let mut ts = timeline_state.borrow_mut();
            let prev_selected = ts.selected_clip_id.clone();
            ts.selected_clip_id = Some(clip_id.clone());
            let ok = ts.paste_color_grade();
            ts.selected_clip_id = prev_selected;
            drop(ts);
            if ok {
                on_project_changed_full();
            }
            reply.send(json!({"success": ok})).ok();
        }

        McpCommand::MatchClipColors {
            source_clip_id,
            reference_clip_id,
            generate_lut,
            reply,
        } => {
            // Collect clip info from project.
            let clip_info: Option<(
                String,
                u64,
                u64,
                String, // source: path, in, out, track_id
                String,
                u64,
                u64, // ref: path, in, out
                Option<crate::media::color_match::ReferenceGrading>,
            )> = {
                let proj = project.borrow();
                let find_clip = |id: &str| -> Option<(String, u64, u64, String)> {
                    for track in &proj.tracks {
                        if let Some(c) = track.clips.iter().find(|c| c.id == id) {
                            return Some((
                                c.source_path.clone(),
                                c.source_in,
                                c.source_out,
                                track.id.clone(),
                            ));
                        }
                    }
                    None
                };
                let ref_grading = proj
                    .tracks
                    .iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == reference_clip_id)
                    .map(crate::media::color_match::ReferenceGrading::from_clip);
                let src = find_clip(&source_clip_id);
                let reff = find_clip(&reference_clip_id);
                match (src, reff) {
                    (Some(s), Some(r)) => Some((s.0, s.1, s.2, s.3, r.0, r.1, r.2, ref_grading)),
                    _ => None,
                }
            };

            let Some((
                src_path,
                src_in,
                src_out,
                src_track_id,
                ref_path,
                ref_in,
                ref_out,
                ref_grading,
            )) = clip_info
            else {
                reply
                    .send(json!({"success": false, "error": "Could not find source and/or reference clip"}))
                    .ok();
                return;
            };

            // Capture old values before modification.
            let old_values = {
                let proj = project.borrow();
                proj.tracks
                    .iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == source_clip_id)
                    .map(|c| {
                        (
                            c.brightness,
                            c.contrast,
                            c.saturation,
                            c.temperature,
                            c.tint,
                            c.exposure,
                            c.black_point,
                            c.shadows,
                            c.midtones,
                            c.highlights,
                            c.highlights_warmth,
                            c.highlights_tint,
                            c.midtones_warmth,
                            c.midtones_tint,
                            c.shadows_warmth,
                            c.shadows_tint,
                            c.lut_paths.clone(),
                        )
                    })
            };
            let Some(old) = old_values else {
                reply
                    .send(json!({"success": false, "error": "Source clip not found"}))
                    .ok();
                return;
            };

            let params = crate::media::color_match::MatchColorParams {
                source_path: src_path,
                source_in_ns: src_in,
                source_out_ns: src_out,
                reference_path: ref_path,
                reference_in_ns: ref_in,
                reference_out_ns: ref_out,
                sample_count: 8,
                generate_lut,
                lut_output_dir: None,
                reference_grading: ref_grading,
            };

            match crate::media::color_match::run_match_color(&params) {
                Ok(outcome) => {
                    let r = &outcome.slider_result;

                    // Build and execute undo command.
                    let cmd = crate::undo::MatchColorCommand {
                        clip_id: source_clip_id.clone(),
                        track_id: src_track_id.clone(),
                        old_brightness: old.0,
                        old_contrast: old.1,
                        old_saturation: old.2,
                        old_temperature: old.3,
                        old_tint: old.4,
                        old_exposure: old.5,
                        old_black_point: old.6,
                        old_shadows: old.7,
                        old_midtones: old.8,
                        old_highlights: old.9,
                        old_highlights_warmth: old.10,
                        old_highlights_tint: old.11,
                        old_midtones_warmth: old.12,
                        old_midtones_tint: old.13,
                        old_shadows_warmth: old.14,
                        old_shadows_tint: old.15,
                        old_lut_paths: old.16.clone(),
                        new_brightness: r.brightness,
                        new_contrast: r.contrast,
                        new_saturation: r.saturation,
                        new_temperature: r.temperature,
                        new_tint: r.tint,
                        new_exposure: r.exposure,
                        new_black_point: r.black_point,
                        new_shadows: r.shadows,
                        new_midtones: r.midtones,
                        new_highlights: r.highlights,
                        new_highlights_warmth: r.highlights_warmth,
                        new_highlights_tint: r.highlights_tint,
                        new_midtones_warmth: r.midtones_warmth,
                        new_midtones_tint: r.midtones_tint,
                        new_shadows_warmth: r.shadows_warmth,
                        new_shadows_tint: r.shadows_tint,
                        new_lut_paths: {
                            let mut paths = old.16.clone();
                            if let Some(ref lp) = outcome.lut_path {
                                paths.push(lp.clone());
                            }
                            paths
                        },
                    };

                    {
                        let mut ts = timeline_state.borrow_mut();
                        let mut proj = project.borrow_mut();
                        ts.history.execute(Box::new(cmd), &mut proj);
                    }

                    // Also assign the generated LUT if any.
                    if let Some(ref lut_path) = outcome.lut_path {
                        let mut proj = project.borrow_mut();
                        if let Some(clip) = proj.clip_mut(&source_clip_id) {
                            clip.lut_paths.push(lut_path.clone());
                        }
                        proj.dirty = true;
                    }

                    on_project_changed_full();

                    reply
                        .send(json!({
                            "success": true,
                            "applied": {
                                "brightness": r.brightness,
                                "contrast": r.contrast,
                                "saturation": r.saturation,
                                "temperature": r.temperature,
                                "tint": r.tint,
                                "exposure": r.exposure,
                            },
                            "lut_path": outcome.lut_path,
                            "source_stats": {
                                "mean_l": outcome.source_stats.mean_l,
                                "std_l": outcome.source_stats.std_l,
                                "mean_a": outcome.source_stats.mean_a,
                                "mean_b": outcome.source_stats.mean_b,
                            },
                            "reference_stats": {
                                "mean_l": outcome.reference_stats.mean_l,
                                "std_l": outcome.reference_stats.std_l,
                                "mean_a": outcome.reference_stats.mean_a,
                                "mean_b": outcome.reference_stats.mean_b,
                            },
                        }))
                        .ok();
                }
                Err(e) => {
                    reply
                        .send(json!({"success": false, "error": format!("{e}")}))
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
                    track.clips[idx].clamp_source_out();
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

        McpCommand::SetClipSpeed {
            clip_id,
            speed,
            slow_motion_interp,
            reply,
        } => {
            let speed = speed.clamp(0.05, 16.0);
            let mut found = false;
            let mut requested_ai = false;
            {
                let mut proj = project.borrow_mut();
                if let Some(clip) = proj.clip_mut(&clip_id) {
                    clip.speed = speed;
                    if let Some(ref mode) = slow_motion_interp {
                        clip.slow_motion_interp =
                            crate::model::clip::SlowMotionInterp::from_xml_str(mode);
                    }
                    requested_ai = clip.slow_motion_interp
                        == crate::model::clip::SlowMotionInterp::Ai
                        && clip.has_slow_motion();
                    proj.dirty = true;
                    found = true;
                }
            }
            if found && requested_ai {
                let proj = project.borrow();
                let clip = proj.clip_ref(&clip_id).cloned();
                drop(proj);
                if let Some(clip) = clip {
                    frame_interp_cache.borrow_mut().request_for_clip(&clip);
                }
            }
            reply
                .send(json!({
                    "success": found,
                    "ai_queued": requested_ai,
                }))
                .ok();
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
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                let new_in = (clip.source_in as i64 + delta_ns).max(0) as u64;
                let mut new_out =
                    (clip.source_out as i64 + delta_ns).max(new_in as i64 + 1_000_000) as u64;
                if let Some(max) = clip.max_source_out() {
                    if new_out > max {
                        new_out = max;
                    }
                }
                clip.source_in = new_in;
                clip.source_out = new_out;
                true
            } else {
                false
            };
            if found {
                proj.dirty = true;
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
                            track.clips[li].clamp_source_out();
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
            blur,
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
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.brightness = brightness as f32;
                clip.contrast = contrast as f32;
                clip.saturation = saturation as f32;
                clip.temperature = temperature as f32;
                clip.tint = tint as f32;
                clip.denoise = denoise as f32;
                clip.sharpness = sharpness as f32;
                clip.blur = blur as f32;
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
                true
            } else {
                false
            };
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
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.color_label = parsed;
                proj.dirty = true;
                true
            } else {
                false
            };
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
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
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
                true
            } else {
                false
            };
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        #[cfg(feature = "ai-inference")]
        McpCommand::GenerateSamMask {
            clip_id,
            frame_ns,
            box_x1,
            box_y1,
            box_x2,
            box_y2,
            point_x,
            point_y,
            tolerance_px,
            reply,
        } => {
            // Step 1 — resolve the clip in the project (brief borrow).
            //
            // We capture source_path + source_in + source_out here so
            // we can drop the project borrow before the slow
            // inference work starts. If the clip disappears during
            // inference (project reload etc.) we'll detect that on
            // step 6 when we try to re-borrow to append the mask.
            let clip_info = {
                let proj = project.borrow();
                proj.tracks.iter().find_map(|t| {
                    t.clips.iter().find(|c| c.id == clip_id).map(|c| {
                        (
                            c.source_path.clone(),
                            c.source_in,
                            c.source_out,
                        )
                    })
                })
            };
            let Some((source_path, source_in, source_out)) = clip_info else {
                reply
                    .send(serde_json::json!({
                        "success": false,
                        "error": "Clip not found"
                    }))
                    .ok();
                // Can't early-return from the dispatch match arm
                // (we're inside a single big `match cmd { ... }`);
                // falling through to end of the arm is fine.
                return;
            };

            // Step 2 — locate the installed SAM model.
            let sam_paths = match crate::media::sam_cache::find_sam_model_paths() {
                Some(p) => p,
                None => {
                    reply.send(serde_json::json!({
                        "success": false,
                        "error": "SAM 3 model not installed. See Preferences → Models → Segment Anything 3.1 for install instructions."
                    })).ok();
                    return;
                }
            };

            // Step 3 — decide which frame to segment. Default is the
            // clip's source_in (first visible frame); callers can
            // override via frame_ns, clamped into the clip's
            // source-range window.
            let target_frame_ns = frame_ns.unwrap_or(source_in).clamp(source_in, source_out);

            let source_path_buf = std::path::PathBuf::from(&source_path);
            let (rgb, src_w, src_h) =
                match crate::media::sam_cache::decode_single_frame(
                    &source_path_buf,
                    target_frame_ns,
                ) {
                    Ok(v) => v,
                    Err(e) => {
                        reply
                            .send(serde_json::json!({
                                "success": false,
                                "error": format!("Frame decode failed: {e}")
                            }))
                            .ok();
                        return;
                    }
                };

            // Step 4 — build the prompt. Box prompt wins if all four
            // box_* fields are set; otherwise fall back to the
            // emulated-point path; otherwise error out.
            use crate::media::sam_cache::BoxPrompt;
            let prompt_result: Result<BoxPrompt, &'static str> =
                match (box_x1, box_y1, box_x2, box_y2) {
                    (Some(x1), Some(y1), Some(x2), Some(y2)) => {
                        // Normalized clip-local → source-pixel.
                        let sx1 = (x1 * src_w as f64) as f32;
                        let sy1 = (y1 * src_h as f64) as f32;
                        let sx2 = (x2 * src_w as f64) as f32;
                        let sy2 = (y2 * src_h as f64) as f32;
                        Ok(BoxPrompt::from_corners(sx1, sy1, sx2, sy2))
                    }
                    _ => match (point_x, point_y) {
                        (Some(px), Some(py)) => {
                            // Normalized click → source-pixel →
                            // 8-px synthetic box. SAM 3's decoder
                            // doesn't have a native point interface
                            // so we approximate with a small
                            // exemplar region.
                            let cx = (px * src_w as f64) as f32;
                            let cy = (py * src_h as f64) as f32;
                            Ok(BoxPrompt::point_emulation(cx, cy, 4.0))
                        }
                        _ => Err(
                            "Missing prompt: provide either all four box_{x1,y1,x2,y2} or both point_x/point_y",
                        ),
                    },
                };
            let prompt = match prompt_result {
                Ok(p) => p,
                Err(msg) => {
                    reply
                        .send(serde_json::json!({
                            "success": false,
                            "error": msg,
                        }))
                        .ok();
                    return;
                }
            };

            // Step 5 — load sessions and run inference. Blocks the
            // main thread for the duration of SAM inference (seconds
            // on GPU, tens of seconds on CPU). Phase 2b will move
            // this to a background thread when there's a UI button
            // that needs to stay responsive; the MCP path is
            // synchronous automation traffic where blocking is
            // acceptable.
            let mut sessions = match crate::media::sam_cache::SamSessions::load(&sam_paths) {
                Ok(s) => s,
                Err(e) => {
                    reply
                        .send(serde_json::json!({
                            "success": false,
                            "error": format!("SAM session load failed: {e}")
                        }))
                        .ok();
                    return;
                }
            };
            let result = match crate::media::sam_cache::segment_with_box(
                &mut sessions,
                &rgb,
                src_w,
                src_h,
                prompt,
            ) {
                Ok(r) => r,
                Err(e) => {
                    reply
                        .send(serde_json::json!({
                            "success": false,
                            "error": format!("SAM inference failed: {e}")
                        }))
                        .ok();
                    return;
                }
            };

            // Step 6 — convert the binary mask into a bezier polygon
            // and append it to the clip. The default tolerance of
            // 2 px gives reasonably tight polygons with moderate
            // point counts; callers can override.
            let tolerance = tolerance_px.unwrap_or(2.0).max(0.0);
            let bezier_points = match crate::media::mask_contour::mask_to_bezier_path(
                &result.mask,
                result.src_w,
                result.src_h,
                tolerance,
            ) {
                Some(p) if p.len() >= 3 => p,
                _ => {
                    reply
                        .send(serde_json::json!({
                            "success": false,
                            "error": "SAM produced an empty or degenerate mask (no closed contour found)"
                        }))
                        .ok();
                    return;
                }
            };

            let point_count = bezier_points.len();
            let score = result.score;

            // Re-borrow the project to append the new mask. If the
            // clip got deleted or the project got replaced during
            // inference, we report that as a soft failure.
            let appended: Option<String> = {
                let mut proj = project.borrow_mut();
                let mut out: Option<String> = None;
                for track in proj.tracks.iter_mut() {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                        let mask =
                            crate::model::clip::ClipMask::new_path(bezier_points.clone());
                        let mask_id = mask.id.clone();
                        clip.masks.push(mask);
                        out = Some(mask_id);
                        break;
                    }
                }
                if out.is_some() {
                    proj.dirty = true;
                }
                out
            };

            match appended {
                Some(mask_id) => {
                    on_project_changed();
                    reply
                        .send(serde_json::json!({
                            "success": true,
                            "mask_id": mask_id,
                            "score": score,
                            "point_count": point_count,
                        }))
                        .ok();
                }
                None => {
                    reply
                        .send(serde_json::json!({
                            "success": false,
                            "error": "Clip disappeared during SAM inference"
                        }))
                        .ok();
                }
            }
        }

        #[cfg(not(feature = "ai-inference"))]
        McpCommand::GenerateSamMask { reply, .. } => {
            reply
                .send(serde_json::json!({
                    "success": false,
                    "error": "generate_sam_mask requires the ai-inference Cargo feature"
                }))
                .ok();
        }

        McpCommand::SetClipMask {
            clip_id,
            enabled,
            shape,
            center_x,
            center_y,
            width,
            height,
            rotation,
            feather,
            expansion,
            invert,
            path,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                // Create mask[0] if absent
                if clip.masks.is_empty() {
                    clip.masks.push(crate::model::clip::ClipMask::new(
                        crate::model::clip::MaskShape::Rectangle,
                    ));
                }
                if let Some(mask) = clip.masks.first_mut() {
                    if let Some(v) = enabled {
                        mask.enabled = v;
                    }
                    if let Some(ref s) = shape {
                        mask.shape = match s.as_str() {
                            "ellipse" => crate::model::clip::MaskShape::Ellipse,
                            "path" => crate::model::clip::MaskShape::Path,
                            _ => crate::model::clip::MaskShape::Rectangle,
                        };
                    }
                    // Handle path data for bezier path masks
                    if let Some(ref s) = shape {
                        if s == "path" {
                            if let Some(ref path_val) = path {
                                if let Ok(points) =
                                    serde_json::from_value::<Vec<crate::model::clip::BezierPoint>>(
                                        path_val.clone(),
                                    )
                                {
                                    mask.path = Some(crate::model::clip::MaskPath { points });
                                }
                            }
                            if mask.path.is_none() {
                                mask.path = Some(crate::model::clip::default_diamond_path());
                            }
                        }
                    }
                    if let Some(v) = center_x {
                        mask.center_x = v.clamp(0.0, 1.0);
                    }
                    if let Some(v) = center_y {
                        mask.center_y = v.clamp(0.0, 1.0);
                    }
                    if let Some(v) = width {
                        mask.width = v.clamp(0.01, 0.5);
                    }
                    if let Some(v) = height {
                        mask.height = v.clamp(0.01, 0.5);
                    }
                    if let Some(v) = rotation {
                        mask.rotation = v.clamp(
                            crate::model::transform_bounds::ROTATE_MIN_DEG,
                            crate::model::transform_bounds::ROTATE_MAX_DEG,
                        );
                    }
                    if let Some(v) = feather {
                        mask.feather = v.clamp(0.0, 0.5);
                    }
                    if let Some(v) = expansion {
                        mask.expansion = v.clamp(-0.5, 0.5);
                    }
                    if let Some(v) = invert {
                        mask.invert = v;
                    }
                }
                proj.dirty = true;
                true
            } else {
                false
            };
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
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                if let Some(v) = enabled {
                    clip.bg_removal_enabled = v;
                }
                if let Some(v) = threshold {
                    clip.bg_removal_threshold = v;
                }
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipHslQualifier {
            clip_id,
            qualifier,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.hsl_qualifier = qualifier.clone();
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipMotionBlur {
            clip_id,
            enabled,
            shutter_angle,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let result: Option<(bool, f64)> = if let Some(clip) = proj.clip_mut(&clip_id) {
                if let Some(v) = enabled {
                    clip.motion_blur_enabled = v;
                }
                if let Some(v) = shutter_angle {
                    clip.motion_blur_shutter_angle = v.clamp(0.0, 720.0);
                }
                Some((clip.motion_blur_enabled, clip.motion_blur_shutter_angle))
            } else {
                None
            };
            if result.is_some() {
                proj.dirty = true;
            }
            drop(proj);
            let response = match result {
                Some((en, sh)) => json!({
                    "success": true,
                    "motion_blur_enabled": en,
                    "motion_blur_shutter_angle": sh,
                }),
                None => json!({"success": false}),
            };
            let found = result.is_some();
            reply.send(response).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipLut {
            clip_id,
            lut_paths,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.lut_paths = lut_paths.clone();
                proj.dirty = true;
                true
            } else {
                false
            };
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
            anamorphic_desqueeze,
            reply,
        } => {
            use crate::model::transform_bounds::{
                POSITION_MAX, POSITION_MIN, ROTATE_MAX_DEG_I32, ROTATE_MIN_DEG_I32, SCALE_MAX,
                SCALE_MIN,
            };
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.scale = scale.clamp(SCALE_MIN, SCALE_MAX);
                clip.position_x = position_x.clamp(POSITION_MIN, POSITION_MAX);
                clip.position_y = position_y.clamp(POSITION_MIN, POSITION_MAX);
                if let Some(rot) = rotate {
                    clip.rotate = rot.clamp(ROTATE_MIN_DEG_I32, ROTATE_MAX_DEG_I32);
                }
                if let Some(a) = anamorphic_desqueeze {
                    clip.anamorphic_desqueeze = a;
                }
                proj.dirty = true;
                true
            } else {
                false
            };
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
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.opacity = opacity.clamp(0.0, 1.0);
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipVoiceIsolation {
            clip_id,
            voice_isolation,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.voice_isolation = voice_isolation.clamp(0.0, 1.0) as f32;
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipVoiceEnhance {
            clip_id,
            enabled,
            strength,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let result = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.voice_enhance = enabled;
                if let Some(s) = strength {
                    clip.voice_enhance_strength = (s as f32).clamp(0.0, 1.0);
                }
                let snapshot = (clip.voice_enhance, clip.voice_enhance_strength);
                proj.dirty = true;
                Some(snapshot)
            } else {
                None
            };
            drop(proj);
            match result {
                Some((en, st)) => {
                    reply
                        .send(json!({
                            "success": true,
                            "enabled": en,
                            "strength": st as f64,
                        }))
                        .ok();
                    on_project_changed();
                }
                None => {
                    reply.send(json!({"success": false})).ok();
                }
            }
        }

        McpCommand::SetClipSubtitleVisible {
            clip_id,
            visible,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.subtitle_visible = visible;
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            reply
                .send(json!({"success": found, "visible": visible}))
                .ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetVoiceIsolationSource {
            clip_id,
            source,
            reply,
        } => {
            let new_source = crate::model::clip::VoiceIsolationSource::from_str(&source);
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.voice_isolation_source = new_source;
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            reply
                .send(json!({"success": found, "source": new_source.as_str()}))
                .ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetVoiceIsolationSilenceParams {
            clip_id,
            threshold_db,
            min_ms,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let updated = if let Some(clip) = proj.clip_mut(&clip_id) {
                if let Some(t) = threshold_db {
                    clip.voice_isolation_silence_threshold_db = (t as f32).clamp(-60.0, -10.0);
                }
                if let Some(m) = min_ms {
                    clip.voice_isolation_silence_min_ms = m.clamp(50, 2000);
                }
                // Param change invalidates the cached analysis.
                clip.voice_isolation_speech_intervals.clear();
                Some((
                    clip.voice_isolation_silence_threshold_db,
                    clip.voice_isolation_silence_min_ms,
                ))
            } else {
                None
            };
            if updated.is_some() {
                proj.dirty = true;
            }
            drop(proj);
            match updated {
                Some((t, m)) => {
                    reply
                        .send(json!({
                            "success": true,
                            "threshold_db": t,
                            "min_ms": m
                        }))
                        .ok();
                    on_project_changed();
                }
                None => {
                    reply.send(json!({"success": false})).ok();
                }
            }
        }

        McpCommand::SuggestVoiceIsolationThreshold { clip_id, reply } => {
            let info = {
                let proj = project.borrow();
                proj.clip_ref(&clip_id)
                    .map(|c| (c.source_path.clone(), c.source_in, c.source_out))
            };
            match info {
                None => {
                    reply
                        .send(json!({"success": false, "error": "clip not found"}))
                        .ok();
                }
                Some((source_path, source_in, source_out)) => {
                    match crate::media::export::suggest_silence_threshold_db(
                        &source_path,
                        source_in,
                        source_out,
                    ) {
                        Ok(db) => {
                            reply
                                .send(json!({"success": true, "threshold_db": db}))
                                .ok();
                        }
                        Err(e) => {
                            reply
                                .send(json!({"success": false, "error": e.to_string()}))
                                .ok();
                        }
                    }
                }
            }
        }

        McpCommand::AnalyzeVoiceIsolationSilence { clip_id, reply } => {
            let info = {
                let proj = project.borrow();
                proj.clip_ref(&clip_id).map(|c| {
                    (
                        c.source_path.clone(),
                        c.source_in,
                        c.source_out,
                        c.voice_isolation_silence_threshold_db,
                        c.voice_isolation_silence_min_ms,
                    )
                })
            };
            match info {
                None => {
                    reply
                        .send(json!({"success": false, "error": "clip not found"}))
                        .ok();
                }
                Some((source_path, source_in, source_out, threshold_db, min_ms)) => {
                    let min_duration = (min_ms as f64) / 1000.0;
                    match crate::media::export::detect_silence(
                        &source_path,
                        source_in,
                        source_out,
                        threshold_db as f64,
                        min_duration,
                    ) {
                        Ok(silences) => {
                            let clip_duration_ns = source_out.saturating_sub(source_in);
                            let new_intervals = crate::media::export::invert_silences_to_speech(
                                &silences,
                                clip_duration_ns,
                            );
                            let count = new_intervals.len();
                            let intervals_json: Vec<_> =
                                new_intervals.iter().map(|(s, e)| json!([s, e])).collect();
                            {
                                let mut proj = project.borrow_mut();
                                if let Some(clip) = proj.clip_mut(&clip_id) {
                                    clip.voice_isolation_speech_intervals = new_intervals;
                                    proj.dirty = true;
                                }
                            }
                            reply
                                .send(json!({
                                    "success": true,
                                    "count": count,
                                    "intervals_ns": intervals_json
                                }))
                                .ok();
                            on_project_changed();
                        }
                        Err(e) => {
                            reply
                                .send(json!({"success": false, "error": e.to_string()}))
                                .ok();
                        }
                    }
                }
            }
        }

        McpCommand::SetClipEq {
            clip_id,
            low_freq,
            low_gain,
            low_q,
            mid_freq,
            mid_gain,
            mid_q,
            high_freq,
            high_gain,
            high_q,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let mut result_bands = crate::model::clip::default_eq_bands();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                if let Some(v) = low_freq {
                    clip.eq_bands[0].freq = v.clamp(20.0, 20000.0);
                }
                if let Some(v) = low_gain {
                    clip.eq_bands[0].gain = v.clamp(-24.0, 12.0);
                }
                if let Some(v) = low_q {
                    clip.eq_bands[0].q = v.clamp(0.1, 10.0);
                }
                if let Some(v) = mid_freq {
                    clip.eq_bands[1].freq = v.clamp(20.0, 20000.0);
                }
                if let Some(v) = mid_gain {
                    clip.eq_bands[1].gain = v.clamp(-24.0, 12.0);
                }
                if let Some(v) = mid_q {
                    clip.eq_bands[1].q = v.clamp(0.1, 10.0);
                }
                if let Some(v) = high_freq {
                    clip.eq_bands[2].freq = v.clamp(20.0, 20000.0);
                }
                if let Some(v) = high_gain {
                    clip.eq_bands[2].gain = v.clamp(-24.0, 12.0);
                }
                if let Some(v) = high_q {
                    clip.eq_bands[2].q = v.clamp(0.1, 10.0);
                }
                result_bands = clip.eq_bands;
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            if found {
                prog_player
                    .borrow_mut()
                    .update_eq_for_clip(&clip_id, result_bands);
            }
            reply.send(json!({
                "success": found,
                "eq_bands": {
                    "low": { "freq": result_bands[0].freq, "gain": result_bands[0].gain, "q": result_bands[0].q },
                    "mid": { "freq": result_bands[1].freq, "gain": result_bands[1].gain, "q": result_bands[1].q },
                    "high": { "freq": result_bands[2].freq, "gain": result_bands[2].gain, "q": result_bands[2].q }
                }
            })).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::AnalyzeProjectLoudness { reply } => {
            // Snapshot the project so the render/analyze can run on the
            // main thread without holding a borrow across the ffmpeg
            // subprocess (MCP dispatch already runs on the GTK loop,
            // which is acceptable for a blocking tool call — matches
            // export_mp4 and normalize_clip_audio patterns).
            let project_snapshot = project.borrow().clone();
            let current_gain = project_snapshot.master_gain_db;
            let result = crate::media::export::analyze_project_loudness(&project_snapshot);
            let (pref_preset, pref_target) = {
                let p = preferences_state.borrow();
                (p.loudness_target_preset.clone(), p.loudness_target_lufs)
            };
            match result {
                Ok(report) => {
                    let target_lufs = crate::ui_state::loudness_target_preset_to_lufs(
                        &pref_preset,
                    )
                    .unwrap_or(pref_target);
                    let delta = target_lufs - report.integrated_lufs;
                    reply
                        .send(json!({
                            "success": true,
                            "integrated_lufs": report.integrated_lufs,
                            "short_term_max_lufs": report.short_term_max_lufs,
                            "momentary_max_lufs": report.momentary_max_lufs,
                            "loudness_range_lu": report.loudness_range_lu,
                            "true_peak_dbtp": report.true_peak_dbtp,
                            "threshold_lufs": report.threshold_lufs,
                            "current_master_gain_db": current_gain,
                            "target_preset": pref_preset,
                            "target_lufs": target_lufs,
                            "delta_db": delta,
                        }))
                        .ok();
                }
                Err(e) => {
                    reply
                        .send(json!({"success": false, "error": e.to_string()}))
                        .ok();
                }
            }
        }

        McpCommand::SetProjectMasterGainDb {
            master_gain_db,
            reply,
        } => {
            let clamped = master_gain_db.clamp(-24.0, 24.0);
            let old_db = project.borrow().master_gain_db;
            if (clamped - old_db).abs() > 1e-9 {
                let cmd: Box<dyn crate::undo::EditCommand> = Box::new(
                    crate::undo::SetProjectMasterGainCommand {
                        old_db,
                        new_db: clamped,
                    },
                );
                {
                    let mut proj = project.borrow_mut();
                    timeline_state.borrow_mut().history.execute(cmd, &mut proj);
                }
                prog_player.borrow_mut().set_master_gain_db(clamped);
                // Note: the Loudness popover's current-gain label is
                // refreshed on next analyze / open. We can't reach the
                // popover view from the MCP dispatch scope without
                // lifting the cell further up, which Phase 1 doesn't
                // warrant.
                on_project_changed();
            }
            reply
                .send(json!({"success": true, "master_gain_db": clamped}))
                .ok();
        }

        McpCommand::NormalizeClipAudio {
            clip_id,
            mode,
            target_level,
            reply,
        } => {
            // Extract clip info from the model.
            let clip_info = {
                let proj = project.borrow();
                proj.clip_ref(&clip_id).map(|clip| {
                    (
                        clip.source_path.clone(),
                        clip.source_in,
                        clip.source_out,
                        clip.volume,
                        clip.measured_loudness_lufs,
                    )
                })
            };
            if let Some((source_path, source_in, source_out, old_volume, _old_measured)) = clip_info
            {
                // Run analysis synchronously (blocks GTK main loop for a few seconds,
                // acceptable for MCP tool calls — same pattern as export_mp4).
                let result = match mode.as_str() {
                    "peak" => {
                        crate::media::export::analyze_peak_db(&source_path, source_in, source_out)
                            .map(|peak| {
                                let gain =
                                    crate::media::export::compute_peak_gain(peak, target_level);
                                (peak, gain)
                            })
                    }
                    _ => crate::media::export::analyze_loudness_lufs(
                        &source_path,
                        source_in,
                        source_out,
                    )
                    .map(|lufs| {
                        let gain = crate::media::export::compute_lufs_gain(lufs, target_level);
                        (lufs, gain)
                    }),
                };
                match result {
                    Ok((measured, gain)) => {
                        let new_volume = (old_volume as f64 * gain).clamp(0.0, 4.0) as f32;
                        let measured_lufs = if mode == "lufs" { Some(measured) } else { None };
                        {
                            let mut proj = project.borrow_mut();
                            if let Some(clip) = proj.clip_mut(&clip_id) {
                                clip.volume = new_volume;
                                if let Some(lufs) = measured_lufs {
                                    clip.measured_loudness_lufs = Some(lufs);
                                }
                            }
                            proj.dirty = true;
                        }
                        reply
                            .send(serde_json::json!({
                                "success": true,
                                "mode": mode,
                                "measured": measured,
                                "target": target_level,
                                "gain_linear": gain,
                                "old_volume": old_volume,
                                "new_volume": new_volume,
                            }))
                            .ok();
                        on_project_changed();
                    }
                    Err(e) => {
                        reply
                            .send(serde_json::json!({
                                "success": false,
                                "error": e.to_string(),
                            }))
                            .ok();
                    }
                }
            } else {
                reply
                    .send(serde_json::json!({"success": false, "error": "Clip not found"}))
                    .ok();
            }
        }

        McpCommand::MatchClipAudio {
            source_clip_id,
            source_start_ns,
            source_end_ns,
            source_channel_mode,
            reference_clip_id,
            reference_start_ns,
            reference_end_ns,
            reference_channel_mode,
            reply,
        } => {
            let clip_info = {
                let proj = project.borrow();
                let source = collect_audio_match_clip_info(&proj, &source_clip_id)
                    .ok_or_else(|| "Source clip not found.".to_string());
                let reference = collect_audio_match_clip_info(&proj, &reference_clip_id)
                    .ok_or_else(|| "Reference clip not found.".to_string());
                match (source, reference) {
                    (Ok(source), Ok(reference)) => Ok((source, reference)),
                    (Err(e), _) | (_, Err(e)) => Err(e),
                }
            };
            match clip_info.and_then(|(source, reference)| {
                let source_region = if source_start_ns.is_some() || source_end_ns.is_some() {
                    Some(crate::media::audio_match::AnalysisRegionNs {
                        start_ns: source_start_ns.unwrap_or(0),
                        end_ns: source_end_ns.unwrap_or(source.duration_ns),
                    })
                } else {
                    None
                };
                let reference_region = if reference_start_ns.is_some() || reference_end_ns.is_some()
                {
                    Some(crate::media::audio_match::AnalysisRegionNs {
                        start_ns: reference_start_ns.unwrap_or(0),
                        end_ns: reference_end_ns.unwrap_or(reference.duration_ns),
                    })
                } else {
                    None
                };
                run_audio_match_for_clips(
                    &source_clip_id,
                    &source,
                    source_region,
                    source_channel_mode,
                    &reference_clip_id,
                    &reference,
                    reference_region,
                    reference_channel_mode,
                )
            }) {
                Ok(prepared) => {
                    {
                        let mut proj = project.borrow_mut();
                        let cmd = crate::undo::MatchClipAudioCommand {
                            clip_id: prepared.clip_id.clone(),
                            old_volume: prepared.old_volume,
                            new_volume: prepared.new_volume,
                            old_measured_loudness: prepared.old_measured_loudness,
                            new_measured_loudness: prepared.new_measured_loudness,
                            old_eq_bands: prepared.old_eq_bands,
                            new_eq_bands: prepared.new_eq_bands,
                            old_match_eq_bands: prepared.old_match_eq_bands.clone(),
                            new_match_eq_bands: prepared.new_match_eq_bands.clone(),
                        };
                        let mut ts = timeline_state.borrow_mut();
                        ts.history.execute(Box::new(cmd), &mut proj);
                    }
                    {
                        let mut pp = prog_player.borrow_mut();
                        pp.update_match_eq_for_clip(
                            &prepared.clip_id,
                            prepared.new_match_eq_bands.clone(),
                        );
                    }
                    reply
                        .send(serde_json::json!({
                            "success": true,
                            "source_clip_id": source_clip_id,
                            "reference_clip_id": reference_clip_id,
                            "source_range_ns": {
                                "start": prepared.source_region.start_ns,
                                "end": prepared.source_region.end_ns,
                            },
                            "source_channel_mode": prepared.source_channel_mode.as_str(),
                            "reference_range_ns": {
                                "start": prepared.reference_region.start_ns,
                                "end": prepared.reference_region.end_ns,
                            },
                            "reference_channel_mode": prepared.reference_channel_mode.as_str(),
                            "source_loudness_lufs": prepared.source_loudness_lufs,
                            "reference_loudness_lufs": prepared.reference_loudness_lufs,
                            "gain_linear": prepared.volume_gain,
                            "old_volume": prepared.old_volume,
                            "new_volume": prepared.new_volume,
                            "eq_bands": prepared.new_eq_bands.iter().map(|band| serde_json::json!({
                                "freq": band.freq,
                                "gain": band.gain,
                                "q": band.q,
                            })).collect::<Vec<_>>(),
                            "match_eq_bands": prepared.new_match_eq_bands.iter().map(|band| serde_json::json!({
                                "freq": band.freq,
                                "gain": band.gain,
                                "q": band.q,
                            })).collect::<Vec<_>>(),
                            "source_profile_db": {
                                "low": prepared.source_profile.low_db,
                                "mid": prepared.source_profile.mid_db,
                                "high": prepared.source_profile.high_db,
                            },
                            "reference_profile_db": {
                                "low": prepared.reference_profile.low_db,
                                "mid": prepared.reference_profile.mid_db,
                                "high": prepared.reference_profile.high_db,
                            }
                        }))
                        .ok();
                    on_project_changed();
                }
                Err(error) => {
                    reply
                        .send(serde_json::json!({
                            "success": false,
                            "error": error,
                        }))
                        .ok();
                }
            }
        }

        McpCommand::ClearMatchEq { clip_id, reply } => {
            let old_match_eq_bands = {
                let proj = project.borrow();
                proj.clip_ref(&clip_id).map(|c| c.match_eq_bands.clone())
            };
            match old_match_eq_bands {
                Some(old_bands) => {
                    {
                        let mut proj = project.borrow_mut();
                        let cmd = crate::undo::ClearMatchEqCommand {
                            clip_id: clip_id.clone(),
                            old_match_eq_bands: old_bands,
                        };
                        let mut ts = timeline_state.borrow_mut();
                        ts.history.execute(Box::new(cmd), &mut proj);
                    }
                    {
                        let mut pp = prog_player.borrow_mut();
                        pp.update_match_eq_for_clip(&clip_id, Vec::new());
                    }
                    on_project_changed();
                    reply
                        .send(serde_json::json!({
                            "success": true,
                            "clip_id": clip_id,
                        }))
                        .ok();
                }
                None => {
                    reply
                        .send(serde_json::json!({
                            "success": false,
                            "error": "Clip not found",
                        }))
                        .ok();
                }
            }
        }

        McpCommand::DetectSceneCuts {
            clip_id,
            track_id,
            threshold,
            reply,
        } => {
            let clip_info = {
                let proj = project.borrow();
                proj.tracks.iter().find(|t| t.id == track_id).and_then(|t| {
                    t.clips
                        .iter()
                        .find(|c| c.id == clip_id)
                        .map(|c| (c.source_path.clone(), c.source_in, c.source_out))
                })
            };
            if let Some((source_path, source_in, source_out)) = clip_info {
                let cuts = crate::media::export::detect_scene_cuts(
                    &source_path,
                    source_in,
                    source_out,
                    threshold,
                )
                .unwrap_or_default();
                let n = cuts.len();
                if !cuts.is_empty() {
                    apply_scene_cut_results(
                        &clip_id,
                        &track_id,
                        &cuts,
                        project,
                        timeline_state,
                        on_project_changed,
                        Some(window),
                    );
                }
                reply
                    .send(serde_json::json!({
                        "success": true,
                        "cuts_detected": n,
                    }))
                    .ok();
            } else {
                reply
                    .send(serde_json::json!({"success": false, "error": "Clip or track not found"}))
                    .ok();
            }
        }

        McpCommand::GenerateMusic {
            prompt,
            duration_secs,
            track_index,
            timeline_start_ns,
            reference_audio_path,
            reply,
        } => {
            let music_cache = music_gen_cache.borrow();
            if !music_cache.is_available() {
                reply
                    .send(serde_json::json!({
                        "success": false,
                        "error": "MusicGen ONNX models not found. Download musicgen-small models to ~/.local/share/ultimateslice/models/musicgen-small/"
                    }))
                    .ok();
            } else {
                drop(music_cache);
                // Find or default audio track.
                let track_id = {
                    let proj = project.borrow();
                    let audio_tracks: Vec<_> =
                        proj.tracks.iter().filter(|t| t.is_audio()).collect();
                    let track = if let Some(idx) = track_index {
                        proj.tracks.get(idx).map(|t| t.id.clone())
                    } else {
                        audio_tracks.first().map(|t| t.id.clone())
                    };
                    track
                };
                if let Some(track_id) = track_id {
                    // If a reference audio file was supplied, analyze it
                    // synchronously here (the MCP request is already
                    // serialized) and append the derived hint to the
                    // prompt.  Failures degrade gracefully — the original
                    // prompt is used and a warning is logged so the tool
                    // never fails just because the reference was unreadable.
                    let final_prompt = if let Some(ref ref_path) = reference_audio_path {
                        match crate::media::audio_features::analyze_audio_file(
                            ref_path,
                            0,
                            u64::MAX,
                        ) {
                            Ok(features) => {
                                let hint = crate::media::audio_features::features_to_prompt_hint(
                                    &features,
                                );
                                if hint.is_empty() {
                                    prompt.clone()
                                } else {
                                    let augmented = format!("{}, {}", prompt.trim(), hint);
                                    log::info!(
                                        "generate_music: reference analysis OK for {}: \
                                             augmented prompt = {:?}",
                                        ref_path,
                                        augmented
                                    );
                                    augmented
                                }
                            }
                            Err(e) => {
                                log::warn!(
                                    "generate_music: reference analysis failed for {}: {}; \
                                         falling back to original prompt",
                                    ref_path,
                                    e
                                );
                                prompt.clone()
                            }
                        }
                    } else {
                        prompt.clone()
                    };

                    let playhead_ns = timeline_state.borrow().playhead_ns;
                    let start_ns = timeline_start_ns.unwrap_or(playhead_ns);
                    let job_id = uuid::Uuid::new_v4().to_string();
                    let job = crate::media::music_gen::MusicGenJob {
                        job_id: job_id.clone(),
                        prompt: final_prompt,
                        duration_secs,
                        output_path: std::path::PathBuf::new(), // will be set by cache
                        track_id,
                        timeline_start_ns: start_ns,
                        reference_audio_path: reference_audio_path
                            .as_ref()
                            .map(std::path::PathBuf::from),
                    };
                    music_gen_cache.borrow_mut().request(job);
                    reply
                        .send(serde_json::json!({
                            "success": true,
                            "job_id": job_id,
                            "message": "Music generation started. Poll list_clips to see the clip when ready."
                        }))
                        .ok();
                } else {
                    reply
                        .send(
                            serde_json::json!({"success": false, "error": "No audio track found"}),
                        )
                        .ok();
                }
            }
        }

        McpCommand::RecordVoiceover {
            duration_ns,
            track_index,
            reply,
        } => {
            if duration_ns == 0 {
                reply
                    .send(serde_json::json!({"success": false, "error": "duration_ns must be > 0"}))
                    .ok();
            } else {
                let playhead_ns = timeline_state.borrow().playhead_ns;
                // Find or create target audio track.
                let track_id = {
                    let mut proj = project.borrow_mut();
                    if let Some(idx) = track_index {
                        proj.tracks.get(idx).map(|t| t.id.clone())
                    } else {
                        proj.tracks
                            .iter()
                            .find(|t| t.is_audio())
                            .map(|t| t.id.clone())
                            .or_else(|| {
                                let new_track = crate::model::track::Track::new_audio("Audio 1");
                                let id = new_track.id.clone();
                                proj.tracks.push(new_track);
                                Some(id)
                            })
                    }
                };
                if track_id.is_none() {
                    reply
                        .send(serde_json::json!({"success": false, "error": "Invalid track_index"}))
                        .ok();
                } else {
                    let track_id = track_id.unwrap();
                    // Record synchronously (blocks MCP thread for duration_ns).
                    let mut rec = crate::media::voiceover::VoiceoverRecorder::new();
                    match rec.start_recording(playhead_ns, None, true) {
                        Ok(file_path) => {
                            let dur_ms = duration_ns / 1_000_000;
                            std::thread::sleep(std::time::Duration::from_millis(dur_ms));
                            match rec.stop_recording() {
                                Ok((path, actual_dur_ns, start_ns)) => {
                                    let clip = crate::model::clip::Clip::new(
                                        &path,
                                        actual_dur_ns,
                                        start_ns,
                                        crate::model::clip::ClipKind::Audio,
                                    );
                                    let clip_id = clip.id.clone();
                                    {
                                        let mut proj = project.borrow_mut();
                                        if let Some(track) =
                                            proj.tracks.iter_mut().find(|t| t.id == track_id)
                                        {
                                            track.add_clip(clip);
                                        }
                                        proj.dirty = true;
                                    }
                                    on_project_changed();
                                    reply
                                        .send(serde_json::json!({
                                            "success": true,
                                            "clip_id": clip_id,
                                            "file_path": path,
                                            "duration_ns": actual_dur_ns,
                                            "timeline_start_ns": start_ns,
                                        }))
                                        .ok();
                                }
                                Err(e) => {
                                    reply.send(serde_json::json!({"success": false, "error": e.to_string()})).ok();
                                }
                            }
                        }
                        Err(e) => {
                            reply
                                .send(serde_json::json!({"success": false, "error": e.to_string()}))
                                .ok();
                        }
                    }
                } // else (track_id found)
            }
        }

        McpCommand::SetClipBlendMode {
            clip_id,
            blend_mode,
            reply,
        } => {
            let parsed = match blend_mode.as_str() {
                "normal" => Some(crate::model::clip::BlendMode::Normal),
                "multiply" => Some(crate::model::clip::BlendMode::Multiply),
                "screen" => Some(crate::model::clip::BlendMode::Screen),
                "overlay" => Some(crate::model::clip::BlendMode::Overlay),
                "add" => Some(crate::model::clip::BlendMode::Add),
                "difference" => Some(crate::model::clip::BlendMode::Difference),
                "soft_light" => Some(crate::model::clip::BlendMode::SoftLight),
                _ => None,
            };
            let Some(parsed) = parsed else {
                reply
                    .send(json!({"success": false, "error": "blend_mode must be one of: normal, multiply, screen, overlay, add, difference, soft_light"}))
                    .ok();
                return;
            };
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.blend_mode = parsed;
                true
            } else {
                false
            };
            if found {
                proj.dirty = true;
            }
            drop(proj);
            reply
                .send(json!({"success": found, "clip_id": clip_id, "blend_mode": blend_mode}))
                .ok();
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
            bezier_controls,
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
                if let Some(clip) = proj.clip_mut(&clip_id) {
                    keyframe_time_ns =
                        Some(clip.upsert_phase1_keyframe_at_timeline_ns_with_interp(
                            property,
                            timeline_pos_ns,
                            value,
                            interp,
                        ));
                    if let (Some(local_ns), Some((x1, y1, x2, y2))) =
                        (keyframe_time_ns, bezier_controls)
                    {
                        let keyframes = clip.keyframes_for_phase1_property_mut(property);
                        if let Some(kf) = keyframes.iter_mut().find(|kf| kf.time_ns == local_ns) {
                            kf.bezier_controls = Some(crate::model::clip::BezierControls {
                                x1: x1.clamp(0.0, 1.0),
                                y1: y1.clamp(0.0, 1.0),
                                x2: x2.clamp(0.0, 1.0),
                                y2: y2.clamp(0.0, 1.0),
                            });
                        }
                    }
                    proj.dirty = true;
                    found = true;
                }
            }
            reply
                .send(json!({
                    "success": found,
                    "clip_id": clip_id,
                    "property": property.as_str(),
                    "timeline_pos_ns": timeline_pos_ns,
                    "clip_local_time_ns": keyframe_time_ns,
                    "bezier_controls": bezier_controls.map(|(x1, y1, x2, y2)| json!({
                        "x1": x1.clamp(0.0, 1.0),
                        "y1": y1.clamp(0.0, 1.0),
                        "x2": x2.clamp(0.0, 1.0),
                        "y2": y2.clamp(0.0, 1.0),
                    }))
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
                if let Some(clip) = proj.clip_mut(&clip_id) {
                    removed = clip.remove_phase1_keyframe_at_timeline_ns(property, timeline_pos_ns);
                    if removed {
                        proj.dirty = true;
                    }
                    found = true;
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
            // Sync bin data before save.
            crate::model::media_library::sync_bins_to_project(
                &library.borrow(),
                &mut project.borrow_mut(),
            );
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

        McpCommand::SaveEdl { path, reply } => {
            let result = {
                let proj = project.borrow();
                let edl_content = crate::edl::writer::write_edl(&proj);
                std::fs::write(&path, edl_content).map_err(|e| anyhow::anyhow!(e))
            };
            match result {
                Ok(_) => {
                    let _ = reply.send(json!({"success": true, "path": path}));
                }
                Err(e) => {
                    let _ = reply.send(json!({"success": false, "error": e.to_string()}));
                }
            }
        }

        McpCommand::SaveOtio {
            path,
            path_mode,
            reply,
        } => {
            let Some(path_mode) = crate::otio::writer::OtioMediaPathMode::from_str(&path_mode)
            else {
                let _ = reply.send(json!({
                    "success": false,
                    "error": "path_mode must be one of: absolute, relative"
                }));
                return;
            };
            let result = {
                let proj = project.borrow();
                crate::otio::writer::write_otio_to_path(
                    &proj,
                    std::path::Path::new(&path),
                    path_mode,
                )
                .and_then(|json| std::fs::write(&path, json).map_err(|e| anyhow::anyhow!(e)))
            };
            match result {
                Ok(_) => {
                    let _ = reply.send(json!({
                        "success": true,
                        "path": path,
                        "path_mode": path_mode.as_str()
                    }));
                }
                Err(e) => {
                    let _ = reply.send(json!({"success": false, "error": e.to_string()}));
                }
            }
        }

        McpCommand::SaveProjectWithMedia { path, reply } => {
            crate::model::media_library::sync_bins_to_project(
                &library.borrow(),
                &mut project.borrow_mut(),
            );
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

        McpCommand::CollectProjectFiles {
            directory_path,
            mode,
            use_collected_locations_on_next_save,
            reply,
        } => {
            let proj_snapshot = project.borrow().clone();
            let library_snapshot = library.borrow().items.clone();
            let result = crate::fcpxml::writer::collect_files_with_manifest(
                &proj_snapshot,
                &library_snapshot,
                std::path::Path::new(&directory_path),
                mode,
                |_| {},
            );
            match result {
                Ok(manifest) => {
                    let summary = manifest.result.clone();
                    let apply_summary = if use_collected_locations_on_next_save {
                        apply_collected_files_manifest_to_project_state(
                            project,
                            library,
                            source_marks,
                            on_source_selected,
                            on_project_changed_full,
                            &manifest,
                        )
                    } else {
                        crate::fcpxml::writer::ApplyCollectedFilesResult {
                            project_media_references_updated: 0,
                            project_lut_references_updated: 0,
                            library_items_updated: 0,
                        }
                    };
                    reply
                        .send(json!({
                            "success": true,
                            "directory_path": summary.destination_dir.to_string_lossy(),
                            "mode": mode.as_str(),
                            "use_collected_locations_on_next_save": use_collected_locations_on_next_save,
                            "project_paths_updated": apply_summary.updated_any(),
                            "project_media_references_updated": apply_summary.project_media_references_updated,
                            "project_lut_references_updated": apply_summary.project_lut_references_updated,
                            "library_items_updated": apply_summary.library_items_updated,
                            "media_files": summary.media_files_copied,
                            "lut_files": summary.lut_files_copied,
                            "total_files": summary.total_files_copied()
                        }))
                        .ok();
                }
                Err(e) => {
                    reply
                        .send(json!({"success": false, "error": e.to_string()}))
                        .ok();
                }
            }
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
            let main_stack = main_stack.clone();
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
                        main_stack.set_visible_child_name("editor");
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

        McpCommand::OpenOtio { path, reply } => {
            let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Project, String>>(1);
            let path_bg = std::path::PathBuf::from(&path);
            std::thread::spawn(move || {
                let result = crate::ui::project_loader::load_project_from_path(&path_bg);
                let _ = tx.send(result);
            });
            timeline_state.borrow_mut().loading = true;
            let project = project.clone();
            let timeline_state = timeline_state.clone();
            let main_stack = main_stack.clone();
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
                        main_stack.set_visible_child_name("editor");
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
                        reply
                            .send(
                                json!({"success": false, "error": "open_otio worker disconnected"}),
                            )
                            .ok();
                        glib::ControlFlow::Break
                    }
                }
            });
        }

        McpCommand::ExportMp4 {
            path,
            audio_channel_layout,
            reply,
        } => {
            let proj = project.borrow().clone();
            let bg_paths = bg_removal_cache.borrow().paths.clone();
            let interp_paths = frame_interp_cache.borrow().snapshot_paths_by_clip_id(&proj);
            let layout = crate::media::export::AudioChannelLayout::from_str(
                &audio_channel_layout,
            );
            std::thread::spawn(move || {
                let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);
                let proj_worker = proj.clone();
                let path_worker = path.clone();
                std::thread::spawn(move || {
                    let (tx, _rx) = std::sync::mpsc::channel();
                    let options = crate::media::export::ExportOptions {
                        audio_channel_layout: layout,
                        ..crate::media::export::ExportOptions::default()
                    };
                    let result = crate::media::export::export_project(
                        &proj_worker,
                        &path_worker,
                        options,
                        None,
                        &bg_paths,
                        &interp_paths,
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
                            crate::media::export::Container::Gif => "gif",
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
                        "audio_bitrate_kbps": options.audio_bitrate_kbps,
                        "audio_channel_layout": options.audio_channel_layout.as_str(),
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
            audio_channel_layout,
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
                "gif" => crate::media::export::Container::Gif,
                _ => {
                    reply
                        .send(json!({"success": false, "error": "container must be one of: mp4, mov, webm, mkv, gif"}))
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
            let layout = crate::media::export::AudioChannelLayout::from_str(
                &audio_channel_layout,
            );
            let options = crate::media::export::ExportOptions {
                video_codec,
                container,
                output_width,
                output_height,
                crf,
                audio_codec,
                audio_bitrate_kbps,
                gif_fps: None,
                audio_channel_layout: layout,
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

        McpCommand::ListWorkspaceLayouts { reply } => {
            let state = workspace_layouts_state.borrow().clone();
            let current = serde_json::to_value(&state.current).unwrap_or(json!(null));
            let layouts = serde_json::to_value(&state.layouts).unwrap_or(json!([]));
            reply
                .send(json!({
                    "current": current,
                    "layouts": layouts,
                    "active_layout": state.active_layout
                }))
                .ok();
        }

        McpCommand::SaveWorkspaceLayout { name, reply } => {
            let arrangement = capture_workspace_arrangement();
            let result = {
                let mut state = workspace_layouts_state.borrow_mut();
                state.set_current_arrangement(arrangement.clone());
                state.upsert_layout(crate::ui_state::WorkspaceLayout { name, arrangement })
            };
            match result {
                Ok(()) => {
                    let state = workspace_layouts_state.borrow();
                    crate::ui_state::save_workspace_layouts_state(&state);
                    drop(state);
                    sync_workspace_layout_controls();
                    reply
                        .send(json!({
                            "success": true,
                            "name": workspace_layouts_state.borrow().active_layout.clone()
                        }))
                        .ok();
                }
                Err(error) => {
                    reply.send(json!({"success": false, "error": error})).ok();
                }
            }
        }

        McpCommand::ApplyWorkspaceLayout { name, reply } => {
            let arrangement = {
                let state = workspace_layouts_state.borrow();
                state
                    .get_layout(&name)
                    .map(|layout| layout.arrangement.clone())
            };
            let Some(arrangement) = arrangement else {
                reply
                    .send(json!({"success": false, "error": format!("Workspace layout not found: {name}")}))
                    .ok();
                return;
            };
            *workspace_layout_pending_name.borrow_mut() = Some(name.clone());
            apply_workspace_arrangement(arrangement);
            reply.send(json!({"success": true, "name": name})).ok();
        }

        McpCommand::RenameWorkspaceLayout {
            old_name,
            new_name,
            reply,
        } => {
            let result = {
                let mut state = workspace_layouts_state.borrow_mut();
                state.rename_layout(&old_name, &new_name)
            };
            match result {
                Ok(saved_name) => {
                    let state = workspace_layouts_state.borrow();
                    crate::ui_state::save_workspace_layouts_state(&state);
                    drop(state);
                    sync_workspace_layout_controls();
                    reply
                        .send(json!({"success": true, "name": saved_name}))
                        .ok();
                }
                Err(error) => {
                    reply.send(json!({"success": false, "error": error})).ok();
                }
            }
        }

        McpCommand::DeleteWorkspaceLayout { name, reply } => {
            let deleted = {
                let mut state = workspace_layouts_state.borrow_mut();
                state.delete_layout(&name)
            };
            if deleted {
                let state = workspace_layouts_state.borrow();
                crate::ui_state::save_workspace_layouts_state(&state);
                drop(state);
                sync_workspace_layout_controls();
            }
            reply.send(json!({"success": deleted, "name": name})).ok();
        }

        McpCommand::ResetWorkspaceLayout { reply } => {
            *workspace_layout_pending_name.borrow_mut() = None;
            apply_workspace_arrangement(crate::ui_state::WorkspaceArrangement::default());
            reply.send(json!({"success": true})).ok();
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
            let interp_paths = frame_interp_cache.borrow().snapshot_paths_by_clip_id(&proj);
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
                        &interp_paths,
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
            fn library_clip_kind_id(kind: &ClipKind) -> &'static str {
                match kind {
                    ClipKind::Video => "video",
                    ClipKind::Audio => "audio",
                    ClipKind::Image => "image",
                    ClipKind::Title => "title",
                    ClipKind::Adjustment => "adjustment",
                    ClipKind::Compound => "compound",
                    ClipKind::Multicam => "multicam",
                    ClipKind::Audition => "audition",
                }
            }

            let lib = library.borrow();
            let items: Vec<_> = lib
                .items
                .iter()
                .map(|item| {
                    json!({
                        "id":          item.id,
                        "library_key": item.library_key(),
                        "label":       item.label,
                        "source_path": item.source_path,
                        "duration_ns": item.duration_ns,
                        "duration_s":  item.duration_ns as f64 / 1_000_000_000.0,
                        "is_audio_only": item.is_audio_only,
                        "has_audio": item.has_audio,
                        "is_image": item.is_image,
                        "is_animated_svg": item.is_animated_svg,
                        "video_width": item.video_width,
                        "video_height": item.video_height,
                        "frame_rate_num": item.frame_rate_num,
                        "frame_rate_den": item.frame_rate_den,
                        "codec_summary": item.codec_summary,
                        "file_size_bytes": item.file_size_bytes,
                        "clip_kind": item.clip_kind.as_ref().map(library_clip_kind_id),
                        "title_text": item.title_text,
                        "is_missing": item.is_missing,
                        "bin_id": item.bin_id,
                        "rating": media_rating_id(item.rating),
                        "keyword_ranges": item.keyword_ranges.iter().map(|range| json!({
                            "id": range.id,
                            "label": range.label,
                            "start_ns": range.start_ns,
                            "end_ns": range.end_ns,
                            "start_s": range.start_ns as f64 / 1_000_000_000.0,
                            "end_s": range.end_ns as f64 / 1_000_000_000.0,
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect();
            reply.send(json!(items)).ok();
        }

        McpCommand::ImportMedia { path, reply } => {
            let metadata = crate::media::probe_cache::probe_media_metadata(&path);
            let duration_ns = metadata.duration_ns.unwrap_or(10 * 1_000_000_000);
            let audio_only = metadata.is_audio_only;
            let has_audio = metadata.has_audio;
            let source_timecode_base_ns = metadata.source_timecode_base_ns.or_else(|| {
                let lib = library.borrow();
                let proj = project.borrow();
                lookup_source_timecode_base_ns(&lib.items, &proj, &path)
            });
            let mut item = MediaItem::new(path.clone(), duration_ns);
            item.is_audio_only = audio_only;
            item.has_audio = has_audio;
            item.source_timecode_base_ns = source_timecode_base_ns;
            item.is_image = metadata.is_image;
            item.is_animated_svg = metadata.is_animated_svg;
            item.video_width = metadata.video_width;
            item.video_height = metadata.video_height;
            item.frame_rate_num = metadata.frame_rate_num;
            item.frame_rate_den = metadata.frame_rate_den;
            item.codec_summary = metadata.codec_summary.clone();
            item.file_size_bytes = metadata.file_size_bytes;
            let label = item.label.clone();
            library.borrow_mut().items.push(item);
            {
                let proj = project.borrow();
                let mut lib = library.borrow_mut();
                let mut st = timeline_state.borrow_mut();
                refresh_media_availability_state(&proj, lib.items.as_mut_slice(), &mut st);
            }
            reply
                .send(json!({
                    "success": true,
                    "label": label,
                    "duration_ns": duration_ns,
                    "is_audio_only": audio_only,
                    "has_audio": has_audio,
                    "source_timecode_base_ns": source_timecode_base_ns,
                    "video_width": metadata.video_width,
                    "video_height": metadata.video_height,
                    "frame_rate_num": metadata.frame_rate_num,
                    "frame_rate_den": metadata.frame_rate_den,
                    "codec_summary": metadata.codec_summary,
                    "file_size_bytes": metadata.file_size_bytes,
                    "is_missing": !crate::model::media_library::source_path_exists(&path)
                }))
                .ok();
            sync_library_change();
        }

        McpCommand::RelinkMedia { root_path, reply } => {
            let root = std::path::PathBuf::from(&root_path);
            if !root.is_dir() {
                reply
                    .send(json!({"success": false, "error": "root_path must be an existing directory"}))
                    .ok();
                return;
            }
            let summary = {
                let mut proj = project.borrow_mut();
                let mut lib = library.borrow_mut();
                relink_missing_media_under_root(&mut proj, lib.items.as_mut_slice(), &root)
            };
            {
                let proj = project.borrow();
                let mut lib = library.borrow_mut();
                let mut st = timeline_state.borrow_mut();
                refresh_media_availability_state(&proj, lib.items.as_mut_slice(), &mut st);
            }
            reply
                .send(json!({
                    "success": true,
                    "root_path": root_path,
                    "scanned_files": summary.scanned_files,
                    "updated_clip_count": summary.updated_clip_count,
                    "updated_library_count": summary.updated_library_count,
                    "remapped": summary.remapped.iter().map(|(old_path, new_path)| json!({
                        "old_path": old_path,
                        "new_path": new_path
                    })).collect::<Vec<_>>(),
                    "unresolved": summary.unresolved,
                }))
                .ok();
            if summary.updated_clip_count > 0 || summary.updated_library_count > 0 {
                sync_library_change();
            }
        }

        McpCommand::CreateBin {
            name,
            parent_id,
            reply,
        } => {
            use crate::model::media_library::MediaBin;
            let mut lib = library.borrow_mut();
            // Enforce max depth of 2
            if let Some(ref pid) = parent_id {
                let parent_depth = lib
                    .bins
                    .iter()
                    .find(|b| &b.id == pid)
                    .map(|b| b.depth(&lib.bins))
                    .unwrap_or(0);
                if parent_depth >= 2 {
                    reply
                        .send(json!({"error": "Maximum bin nesting depth (2) reached"}))
                        .ok();
                    return;
                }
                if !lib.bins.iter().any(|b| &b.id == pid) {
                    reply.send(json!({"error": "Parent bin not found"})).ok();
                    return;
                }
            }
            let bin = MediaBin::new(&name, parent_id.clone());
            let id = bin.id.clone();
            lib.bins.push(bin);
            drop(lib);
            reply
                .send(json!({"success": true, "id": id, "name": name, "parent_id": parent_id}))
                .ok();
            sync_library_change();
        }

        McpCommand::DeleteBin { bin_id, reply } => {
            let mut lib = library.borrow_mut();
            let bin = lib.bins.iter().find(|b| b.id == bin_id);
            if bin.is_none() {
                reply.send(json!({"error": "Bin not found"})).ok();
                return;
            }
            let parent_id = bin.unwrap().parent_id.clone();
            // Move items to parent/root
            for item in lib.items.iter_mut() {
                if item.bin_id.as_deref() == Some(&bin_id) {
                    item.bin_id = parent_id.clone();
                }
            }
            // Reparent child bins
            let child_ids: Vec<String> = lib
                .bins
                .iter()
                .filter(|b| b.parent_id.as_deref() == Some(&bin_id))
                .map(|b| b.id.clone())
                .collect();
            for cid in child_ids {
                if let Some(cb) = lib.bins.iter_mut().find(|b| b.id == cid) {
                    cb.parent_id = parent_id.clone();
                }
            }
            lib.bins.retain(|b| b.id != bin_id);
            drop(lib);
            reply.send(json!({"success": true})).ok();
            sync_library_change();
        }

        McpCommand::RenameBin {
            bin_id,
            name,
            reply,
        } => {
            let mut lib = library.borrow_mut();
            if let Some(bin) = lib.bins.iter_mut().find(|b| b.id == bin_id) {
                bin.name = name.clone();
                drop(lib);
                reply
                    .send(json!({"success": true, "bin_id": bin_id, "name": name}))
                    .ok();
                sync_library_change();
            } else {
                reply.send(json!({"error": "Bin not found"})).ok();
            }
        }

        McpCommand::ListBins { reply } => {
            let lib = library.borrow();
            let bins: Vec<_> = lib
                .bins
                .iter()
                .map(|b| {
                    let item_count = lib
                        .items
                        .iter()
                        .filter(|i| i.bin_id.as_deref() == Some(&b.id))
                        .count();
                    json!({
                        "id": b.id,
                        "name": b.name,
                        "parent_id": b.parent_id,
                        "item_count": item_count,
                    })
                })
                .collect();
            reply.send(json!(bins)).ok();
        }

        McpCommand::MoveToBin {
            source_paths,
            bin_id,
            reply,
        } => {
            let mut lib = library.borrow_mut();
            // Validate bin exists if specified
            if let Some(ref bid) = bin_id {
                if !lib.bins.iter().any(|b| &b.id == bid) {
                    reply.send(json!({"error": "Target bin not found"})).ok();
                    return;
                }
            }
            let mut moved = 0usize;
            for item in lib.items.iter_mut() {
                if source_paths.contains(&item.source_path) {
                    item.bin_id = bin_id.clone();
                    moved += 1;
                }
            }
            drop(lib);
            reply
                .send(json!({"success": true, "moved_count": moved}))
                .ok();
            sync_library_change();
        }

        McpCommand::ListCollections { reply } => {
            let lib = library.borrow();
            let collections: Vec<_> = lib
                .collections
                .iter()
                .map(|collection| {
                    json!({
                        "id": collection.id,
                        "name": collection.name,
                        "criteria": {
                            "search_text": collection.criteria.search_text,
                            "kind": media_kind_filter_id(collection.criteria.kind),
                            "resolution": resolution_filter_id(collection.criteria.resolution),
                            "frame_rate": frame_rate_filter_id(collection.criteria.frame_rate),
                            "rating": media_rating_filter_id(collection.criteria.rating),
                        },
                        "item_count": lib.items_in_collection(&collection.id).len(),
                    })
                })
                .collect();
            reply.send(json!(collections)).ok();
        }

        McpCommand::CreateCollection {
            name,
            search_text,
            kind,
            resolution,
            frame_rate,
            rating,
            reply,
        } => {
            let criteria = match collection_criteria_from_mcp(
                search_text,
                kind,
                resolution,
                frame_rate,
                rating,
            ) {
                Ok(criteria) => criteria,
                Err(error) => {
                    reply.send(json!({"error": error})).ok();
                    return;
                }
            };
            let mut lib = library.borrow_mut();
            let collection = MediaCollection::new(name.clone(), criteria);
            let id = collection.id.clone();
            lib.collections.push(collection);
            drop(lib);
            reply
                .send(json!({"success": true, "id": id, "name": name}))
                .ok();
            sync_library_change();
        }

        McpCommand::UpdateCollection {
            collection_id,
            name,
            search_text,
            kind,
            resolution,
            frame_rate,
            rating,
            reply,
        } => {
            let mut lib = library.borrow_mut();
            let Some(collection) = lib
                .collections
                .iter_mut()
                .find(|collection| collection.id == collection_id)
            else {
                reply.send(json!({"error": "Collection not found"})).ok();
                return;
            };
            if let Some(name) = name {
                let trimmed = name.trim();
                if trimmed.is_empty() {
                    reply
                        .send(json!({"error": "Collection name cannot be empty"}))
                        .ok();
                    return;
                }
                collection.name = trimmed.to_string();
            }
            let mut criteria = collection.criteria.clone();
            if let Some(search_text) = search_text {
                criteria.search_text = search_text;
            }
            if let Some(kind) = kind {
                let Some(parsed) = parse_media_kind_filter(Some(kind.as_str())) else {
                    reply.send(json!({"error": "invalid kind filter"})).ok();
                    return;
                };
                criteria.kind = parsed;
            }
            if let Some(resolution) = resolution {
                let Some(parsed) = parse_resolution_filter(Some(resolution.as_str())) else {
                    reply
                        .send(json!({"error": "invalid resolution filter"}))
                        .ok();
                    return;
                };
                criteria.resolution = parsed;
            }
            if let Some(frame_rate) = frame_rate {
                let Some(parsed) = parse_frame_rate_filter(Some(frame_rate.as_str())) else {
                    reply
                        .send(json!({"error": "invalid frame_rate filter"}))
                        .ok();
                    return;
                };
                criteria.frame_rate = parsed;
            }
            if let Some(rating) = rating {
                let Some(parsed) = parse_media_rating_filter(Some(rating.as_str())) else {
                    reply.send(json!({"error": "invalid rating filter"})).ok();
                    return;
                };
                criteria.rating = parsed;
            }
            collection.criteria = criteria;
            let name = collection.name.clone();
            drop(lib);
            reply
                .send(json!({"success": true, "collection_id": collection_id, "name": name}))
                .ok();
            sync_library_change();
        }

        McpCommand::DeleteCollection {
            collection_id,
            reply,
        } => {
            let mut lib = library.borrow_mut();
            let initial_len = lib.collections.len();
            lib.collections
                .retain(|collection| collection.id != collection_id);
            if lib.collections.len() == initial_len {
                reply.send(json!({"error": "Collection not found"})).ok();
                return;
            }
            drop(lib);
            reply.send(json!({"success": true})).ok();
            sync_library_change();
        }

        McpCommand::SetMediaRating {
            library_key,
            rating,
            reply,
        } => {
            let parsed_rating = match rating.as_str() {
                "none" => MediaRating::None,
                "favorite" => MediaRating::Favorite,
                "reject" => MediaRating::Reject,
                _ => {
                    reply
                        .send(json!({"success": false, "error": "rating must be one of: none, favorite, reject"}))
                        .ok();
                    return;
                }
            };
            let updated = {
                let mut lib = library.borrow_mut();
                lib.items
                    .iter_mut()
                    .find(|item| item.matches_library_key(&library_key))
                    .map(|item| {
                        item.rating = parsed_rating;
                    })
                    .is_some()
            };
            if !updated {
                reply
                    .send(json!({"success": false, "error": "Library item not found"}))
                    .ok();
                return;
            }
            reply
                .send(json!({
                    "success": true,
                    "library_key": library_key,
                    "rating": media_rating_id(parsed_rating),
                }))
                .ok();
            sync_library_change();
        }

        McpCommand::AddMediaKeywordRange {
            library_key,
            label,
            start_ns,
            end_ns,
            reply,
        } => {
            let trimmed_label = label.trim().to_string();
            if trimmed_label.is_empty() {
                reply
                    .send(json!({"success": false, "error": "label cannot be empty"}))
                    .ok();
                return;
            }
            if end_ns <= start_ns {
                reply
                    .send(
                        json!({"success": false, "error": "end_ns must be greater than start_ns"}),
                    )
                    .ok();
                return;
            }
            let added_range = {
                let mut lib = library.borrow_mut();
                lib.items
                    .iter_mut()
                    .find(|item| item.matches_library_key(&library_key))
                    .map(|item| {
                        let range = MediaKeywordRange::new(trimmed_label.clone(), start_ns, end_ns);
                        item.keyword_ranges.push(range.clone());
                        range
                    })
            };
            let Some(added_range) = added_range else {
                reply
                    .send(json!({"success": false, "error": "Library item not found"}))
                    .ok();
                return;
            };
            reply
                .send(json!({
                    "success": true,
                    "library_key": library_key,
                    "range": {
                        "id": added_range.id,
                        "label": added_range.label,
                        "start_ns": added_range.start_ns,
                        "end_ns": added_range.end_ns,
                    }
                }))
                .ok();
            sync_library_change();
        }

        McpCommand::UpdateMediaKeywordRange {
            library_key,
            range_id,
            label,
            start_ns,
            end_ns,
            reply,
        } => {
            let trimmed_label = label.trim().to_string();
            if trimmed_label.is_empty() {
                reply
                    .send(json!({"success": false, "error": "label cannot be empty"}))
                    .ok();
                return;
            }
            if end_ns <= start_ns {
                reply
                    .send(
                        json!({"success": false, "error": "end_ns must be greater than start_ns"}),
                    )
                    .ok();
                return;
            }
            let updated = {
                let mut lib = library.borrow_mut();
                lib.items
                    .iter_mut()
                    .find(|item| item.matches_library_key(&library_key))
                    .and_then(|item| {
                        item.keyword_ranges
                            .iter_mut()
                            .find(|range| range.id == range_id)
                    })
                    .map(|range| {
                        range.label = trimmed_label.clone();
                        range.start_ns = start_ns;
                        range.end_ns = end_ns;
                    })
                    .is_some()
            };
            if !updated {
                reply
                    .send(json!({"success": false, "error": "Keyword range not found"}))
                    .ok();
                return;
            }
            reply
                .send(json!({
                    "success": true,
                    "library_key": library_key,
                    "range_id": range_id,
                    "label": trimmed_label,
                    "start_ns": start_ns,
                    "end_ns": end_ns,
                }))
                .ok();
            sync_library_change();
        }

        McpCommand::DeleteMediaKeywordRange {
            library_key,
            range_id,
            reply,
        } => {
            let deleted = {
                let mut lib = library.borrow_mut();
                if let Some(item) = lib
                    .items
                    .iter_mut()
                    .find(|item| item.matches_library_key(&library_key))
                {
                    let before = item.keyword_ranges.len();
                    item.keyword_ranges.retain(|range| range.id != range_id);
                    item.keyword_ranges.len() != before
                } else {
                    false
                }
            };
            if !deleted {
                reply
                    .send(json!({"success": false, "error": "Keyword range not found"}))
                    .ok();
                return;
            }
            reply
                .send(json!({
                    "success": true,
                    "library_key": library_key,
                    "range_id": range_id,
                }))
                .ok();
            sync_library_change();
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
            alignment,
            reply,
        } => {
            let candidate = {
                let proj = project.borrow();
                let Some(track) = proj.tracks.get(track_index) else {
                    reply.send(json!({"error":"Track index out of range","track_count":proj.tracks.len()})).ok();
                    return;
                };
                let Some(transition_alignment) = TransitionAlignment::from_str(&alignment) else {
                    reply
                        .send(json!({
                            "error":"Unsupported transition alignment",
                            "supported_alignments":["end_on_cut", "center_on_cut", "start_on_cut"]
                        }))
                        .ok();
                    return;
                };
                let Some(clip) = track.clips.get(clip_index) else {
                    reply.send(json!({"error":"clip_index must reference a clip with a following clip","clip_count":track.clips.len()})).ok();
                    return;
                };
                let validated = match validate_track_transition_request(
                    track,
                    clip_index,
                    &kind,
                    duration_ns,
                    transition_alignment,
                ) {
                    Ok(validated) => validated,
                    Err(err) => {
                        reply
                            .send(json!({
                                "error": err.to_string(),
                                "supported_kinds": supported_transition_kinds(),
                                "supported_alignments":["end_on_cut", "center_on_cut", "start_on_cut"]
                            }))
                            .ok();
                        return;
                    }
                };
                Some((
                    track.id.clone(),
                    clip.id.clone(),
                    clip.outgoing_transition.clone(),
                    validated,
                ))
            };
            let Some((track_id, clip_id, old_transition, validated)) = candidate else {
                return;
            };
            let cmd = crate::undo::SetClipTransitionCommand {
                clip_id,
                track_id,
                old_transition,
                new_transition: validated.transition.clone(),
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
                    "kind": validated.transition.kind,
                    "duration_ns": validated.transition.duration_ns,
                    "alignment": validated.transition.alignment.as_str(),
                    "max_duration_ns": validated.max_duration_ns
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
            main_stack.set_visible_child_name("editor");
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
                lookup_source_placement_info(&lib.items, &proj, &source_path)
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
                    source_info.audio_channel_mode,
                    None,
                    source_info.is_animated_svg,
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
                lookup_source_placement_info(&lib.items, &proj, &source_path)
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
                    source_info.audio_channel_mode,
                    None,
                    source_info.is_animated_svg,
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
                .items
                .iter()
                .find(|i| i.source_path == path)
                .cloned();
            match item {
                Some(media_item) => {
                    if media_item.has_backing_file() {
                        on_source_selected(media_item.source_path.clone(), media_item.duration_ns);
                        reply
                            .send(json!({
                                "ok": true,
                                "label": media_item.label,
                                "duration_ns": media_item.duration_ns,
                            }))
                            .ok();
                    } else {
                        reply
                            .send(json!({
                                "ok": false,
                                "error": "Library item has no source media to preview",
                            }))
                            .ok();
                    }
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

        McpCommand::MatchFrame { clip_id, reply } => {
            let effective_id = clip_id.or_else(|| timeline_state.borrow().selected_clip_id.clone());
            let playhead_ns = timeline_state.borrow().playhead_ns;
            match effective_id {
                None => {
                    reply
                        .send(json!({"ok": false, "error": "No clip selected and no clip_id provided"}))
                        .ok();
                }
                Some(cid) => {
                    let clip_info = {
                        let proj = project.borrow();
                        proj.tracks
                            .iter()
                            .flat_map(|t| t.clips.iter())
                            .find(|c| c.id == cid)
                            .map(|c| {
                                (
                                    c.source_path.clone(),
                                    c.source_in,
                                    c.source_out,
                                    c.timeline_start,
                                )
                            })
                    };
                    match clip_info {
                        None => {
                            reply
                                .send(
                                    json!({"ok": false, "error": format!("Clip not found: {cid}")}),
                                )
                                .ok();
                        }
                        Some((source_path, source_in, source_out, timeline_start)) => {
                            if source_path.is_empty() {
                                reply
                                    .send(json!({"ok": false, "error": "Clip has no source media (title/adjustment)"}))
                                    .ok();
                            } else {
                                let duration_ns = library
                                    .borrow()
                                    .items
                                    .iter()
                                    .find(|item| item.source_path == source_path)
                                    .map(|item| item.duration_ns)
                                    .unwrap_or(source_out);
                                on_source_selected(source_path.clone(), duration_ns);
                                let source_pos = (source_in
                                    + playhead_ns.saturating_sub(timeline_start))
                                .min(source_out)
                                .max(source_in);
                                let _ = player.borrow().seek(source_pos);
                                reply
                                    .send(json!({
                                        "ok": true,
                                        "source_path": source_path,
                                        "source_pos_ns": source_pos,
                                        "source_in_ns": source_in,
                                        "source_out_ns": source_out,
                                        "duration_ns": duration_ns,
                                    }))
                                    .ok();
                            }
                        }
                    }
                }
            }
        }

        McpCommand::ListBackups { reply } => {
            let backups = crate::project_versions::list_backup_files();
            let list: Vec<serde_json::Value> = backups
                .iter()
                .map(|entry| {
                    json!({
                        "path": entry.path.to_string_lossy(),
                        "name": entry.name,
                        "size_bytes": entry.size_bytes,
                    })
                })
                .collect();
            reply
                .send(json!({ "ok": true, "backups": list, "count": list.len() }))
                .ok();
        }

        McpCommand::ListProjectSnapshots { reply } => {
            let snapshots = {
                let proj = project.borrow();
                crate::project_versions::list_project_snapshots_for_project(&proj)
            };
            let list: Vec<serde_json::Value> = snapshots
                .iter()
                .map(|entry| {
                    json!({
                        "id": entry.metadata.id,
                        "name": entry.metadata.snapshot_name,
                        "project_title": entry.metadata.project_title,
                        "project_file_path": entry.metadata.project_file_path,
                        "created_at_unix_secs": entry.metadata.created_at_unix_secs,
                        "created_at": crate::project_versions::format_snapshot_timestamp(entry.metadata.created_at_unix_secs),
                        "path": entry.snapshot_path.to_string_lossy(),
                        "size_bytes": entry.size_bytes,
                    })
                })
                .collect();
            reply
                .send(json!({ "ok": true, "snapshots": list, "count": list.len() }))
                .ok();
        }

        McpCommand::CreateProjectSnapshot { name, reply } => {
            crate::model::media_library::sync_bins_to_project(
                &library.borrow(),
                &mut project.borrow_mut(),
            );
            let result = {
                let proj = project.borrow();
                crate::project_versions::write_snapshot_project_xml(&proj).and_then(|xml| {
                    crate::project_versions::create_project_snapshot(&proj, &xml, &name)
                })
            };
            match result {
                Ok(entry) => {
                    reply
                        .send(json!({
                            "ok": true,
                            "snapshot": {
                                "id": entry.metadata.id,
                                "name": entry.metadata.snapshot_name,
                                "project_title": entry.metadata.project_title,
                                "project_file_path": entry.metadata.project_file_path,
                                "created_at_unix_secs": entry.metadata.created_at_unix_secs,
                                "created_at": crate::project_versions::format_snapshot_timestamp(entry.metadata.created_at_unix_secs),
                                "path": entry.snapshot_path.to_string_lossy(),
                                "size_bytes": entry.size_bytes,
                            }
                        }))
                        .ok();
                }
                Err(e) => {
                    reply.send(json!({"ok": false, "error": e})).ok();
                }
            }
        }

        McpCommand::RestoreProjectSnapshot { snapshot_id, reply } => {
            let preserved_file_path = project.borrow().file_path.clone();
            let snapshot_id_for_worker = snapshot_id.clone();
            let (tx, rx) = std::sync::mpsc::sync_channel::<
                Result<(crate::project_versions::ProjectSnapshotEntry, Project), String>,
            >(1);
            std::thread::spawn(move || {
                let result =
                    crate::project_versions::load_project_snapshot(&snapshot_id_for_worker);
                let _ = tx.send(result);
            });
            timeline_state.borrow_mut().loading = true;
            let project = project.clone();
            let timeline_state = timeline_state.clone();
            let main_stack = main_stack.clone();
            let on_project_changed = on_project_changed_full.clone();
            let suppress_resume_on_next_reload = suppress_resume_on_next_reload.clone();
            let clear_media_browser_on_next_reload = clear_media_browser_on_next_reload.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(10), move || {
                match rx.try_recv() {
                    Ok(Ok((entry, mut new_proj))) => {
                        let snapshot_name = entry.metadata.snapshot_name.clone();
                        new_proj.file_path = preserved_file_path.clone();
                        new_proj.dirty = true;
                        *project.borrow_mut() = new_proj;
                        timeline_state.borrow_mut().loading = false;
                        main_stack.set_visible_child_name("editor");
                        suppress_resume_on_next_reload.set(true);
                        clear_media_browser_on_next_reload.set(true);
                        let on_project_changed = on_project_changed.clone();
                        glib::timeout_add_local_once(
                            std::time::Duration::from_millis(0),
                            move || {
                                on_project_changed();
                            },
                        );
                        reply
                            .send(json!({
                                "ok": true,
                                "snapshot_id": entry.metadata.id,
                                "snapshot_name": snapshot_name,
                                "project_file_path": entry.metadata.project_file_path,
                                "dirty": true,
                            }))
                            .ok();
                        glib::ControlFlow::Break
                    }
                    Ok(Err(e)) => {
                        timeline_state.borrow_mut().loading = false;
                        reply.send(json!({"ok": false, "error": e})).ok();
                        glib::ControlFlow::Break
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        timeline_state.borrow_mut().loading = false;
                        reply.send(json!({"ok": false, "error": "restore_project_snapshot worker disconnected"})).ok();
                        glib::ControlFlow::Break
                    }
                }
            });
        }

        McpCommand::DeleteProjectSnapshot { snapshot_id, reply } => {
            match crate::project_versions::delete_project_snapshot(&snapshot_id) {
                Ok(()) => {
                    reply
                        .send(json!({"ok": true, "snapshot_id": snapshot_id}))
                        .ok();
                }
                Err(e) => {
                    reply.send(json!({"ok": false, "error": e})).ok();
                }
            }
        }

        McpCommand::SetClipStabilization {
            clip_id,
            enabled,
            smoothing,
            reply,
        } => {
            let mut found = false;
            {
                let mut proj = project.borrow_mut();
                if let Some(clip) = proj.clip_mut(&clip_id) {
                    clip.vidstab_enabled = enabled;
                    clip.vidstab_smoothing = (smoothing as f32).clamp(0.0, 1.0);
                    proj.dirty = true;
                    found = true;
                }
            }
            if found {
                on_project_changed();
                reply
                    .send(json!({
                        "ok": true,
                        "clip_id": clip_id,
                        "vidstab_enabled": enabled,
                        "vidstab_smoothing": smoothing,
                    }))
                    .ok();
            } else {
                reply
                    .send(json!({"ok": false, "error": format!("Clip not found: {clip_id}")}))
                    .ok();
            }
        }

        McpCommand::SetClipAutoCropTrack {
            clip_id,
            center_x,
            center_y,
            width,
            height,
            padding,
            reply,
        } => {
            // Ensure a motion tracker with the requested region exists on
            // the clip. If no tracker matches the region, create one so
            // the binding has somewhere to resolve its samples from.
            let tracker_id = {
                let mut proj = project.borrow_mut();
                let Some(clip) = proj.clip_mut(&clip_id) else {
                    reply
                        .send(json!({
                            "ok": false,
                            "error": format!("Clip not found: {clip_id}")
                        }))
                        .ok();
                    return;
                };
                if let Err(message) = clip_supports_tracking_analysis(clip) {
                    reply
                        .send(json!({"ok": false, "error": message}))
                        .ok();
                    return;
                }
                let new_region = crate::model::clip::TrackingRegion {
                    center_x,
                    center_y,
                    width,
                    height,
                    rotation_deg: 0.0,
                };
                // Prefer the first existing tracker; update its region to
                // match so the caller's region is authoritative.
                let tracker_id = if let Some(tracker) = clip.motion_trackers.first_mut() {
                    tracker.analysis_region = new_region;
                    tracker.samples.clear();
                    tracker.id.clone()
                } else {
                    let mut tracker =
                        crate::model::clip::MotionTracker::new("Auto-crop tracker".to_string());
                    tracker.analysis_region = new_region;
                    let id = tracker.id.clone();
                    clip.motion_trackers.push(tracker);
                    id
                };
                proj.dirty = true;
                tracker_id
            };

            let (outcome, command) = run_auto_crop_track_for_clip(
                project,
                tracking_cache,
                tracking_job_owner_by_key,
                tracking_job_key_by_clip,
                &clip_id,
                &tracker_id,
                padding.unwrap_or(AUTO_CROP_DEFAULT_PADDING),
            );
            if let Some(cmd) = command {
                let mut st = timeline_state.borrow_mut();
                let mut proj = project.borrow_mut();
                st.history.execute(cmd, &mut proj);
            }
            on_project_changed();
            match outcome {
                AutoCropOutcome::Ok { message } => {
                    let proj = project.borrow();
                    let binding = proj
                        .clip_ref(&clip_id)
                        .and_then(|c| c.tracking_binding.clone());
                    reply
                        .send(json!({
                            "ok": true,
                            "clip_id": clip_id,
                            "tracker_id": tracker_id,
                            "status": "ready",
                            "message": message,
                            "scale_multiplier": binding.as_ref().map(|b| b.scale_multiplier),
                            "offset_x": binding.as_ref().map(|b| b.offset_x),
                            "offset_y": binding.as_ref().map(|b| b.offset_y),
                        }))
                        .ok();
                }
                AutoCropOutcome::Queued { message } => {
                    let proj = project.borrow();
                    let binding = proj
                        .clip_ref(&clip_id)
                        .and_then(|c| c.tracking_binding.clone());
                    reply
                        .send(json!({
                            "ok": true,
                            "clip_id": clip_id,
                            "tracker_id": tracker_id,
                            "status": "queued",
                            "message": message,
                            "scale_multiplier": binding.as_ref().map(|b| b.scale_multiplier),
                            "offset_x": binding.as_ref().map(|b| b.offset_x),
                            "offset_y": binding.as_ref().map(|b| b.offset_y),
                        }))
                        .ok();
                }
                AutoCropOutcome::Err { message } => {
                    reply.send(json!({"ok": false, "error": message})).ok();
                }
            }
        }

        McpCommand::ListFrei0rPlugins { reply } => {
            let registry = crate::media::frei0r_registry::Frei0rRegistry::get_or_discover();
            let plugins: Vec<Value> = registry
                .plugins
                .iter()
                .map(|p| {
                    let params: Vec<Value> = p
                        .params
                        .iter()
                        .map(|pr| {
                            let mut obj = json!({
                                "name": pr.name,
                                "display_name": pr.display_name,
                                "type": format!("{:?}", pr.param_type),
                                "default": pr.default_value,
                                "min": pr.min,
                                "max": pr.max,
                            });
                            if let Some(ref ev) = pr.enum_values {
                                obj["enum_values"] = json!(ev);
                            }
                            if let Some(ref ds) = pr.default_string {
                                obj["default_string"] = json!(ds);
                            }
                            obj
                        })
                        .collect();
                    json!({
                        "name": p.frei0r_name,
                        "display_name": p.display_name,
                        "gst_element_name": p.gst_element_name,
                        "description": p.description,
                        "category": p.category,
                        "params": params,
                    })
                })
                .collect();
            reply
                .send(json!({"plugins": plugins, "count": plugins.len()}))
                .ok();
        }

        McpCommand::ListClipFrei0rEffects { clip_id, reply } => {
            let proj = project.borrow();
            let mut found = false;
            let mut effects_json = Vec::new();
            if let Some(clip) = proj.clip_ref(&clip_id) {
                found = true;
                for e in &clip.frei0r_effects {
                    effects_json.push(json!({
                        "id": e.id,
                        "plugin_name": e.plugin_name,
                        "enabled": e.enabled,
                        "params": e.params,
                        "string_params": e.string_params,
                    }));
                }
            }
            if found {
                reply.send(json!({"effects": effects_json})).ok();
            } else {
                reply.send(json!({"error": "Clip not found"})).ok();
            }
        }

        McpCommand::AddClipFrei0rEffect {
            clip_id,
            plugin_name,
            params,
            string_params,
            reply,
        } => {
            let effect_id = uuid::Uuid::new_v4().to_string();
            let mut default_params = std::collections::HashMap::new();
            let mut default_string_params = std::collections::HashMap::new();
            // Populate defaults from registry.
            let registry = crate::media::frei0r_registry::Frei0rRegistry::get_or_discover();
            if let Some(info) = registry.find_by_name(&plugin_name) {
                for p in &info.params {
                    if p.param_type == crate::media::frei0r_registry::Frei0rParamType::String {
                        if let Some(ref s) = p.default_string {
                            default_string_params.insert(p.name.clone(), s.clone());
                        }
                    } else {
                        default_params.insert(p.name.clone(), p.default_value);
                    }
                }
            }
            // Override with user-supplied params.
            if let Some(user_params) = params {
                for (k, v) in user_params {
                    default_params.insert(k, v);
                }
            }
            if let Some(user_string_params) = string_params {
                for (k, v) in user_string_params {
                    default_string_params.insert(k, v);
                }
            }
            let effect = crate::model::clip::Frei0rEffect {
                id: effect_id.clone(),
                plugin_name: plugin_name.clone(),
                enabled: true,
                params: default_params,
                string_params: default_string_params,
            };
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.frei0r_effects.push(effect);
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            if found {
                on_project_changed_full();
                reply
                    .send(json!({"success": true, "effect_id": effect_id}))
                    .ok();
            } else {
                reply.send(json!({"error": "Clip not found"})).ok();
            }
        }

        McpCommand::RemoveClipFrei0rEffect {
            clip_id,
            effect_id,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                if let Some(pos) = clip.frei0r_effects.iter().position(|e| e.id == effect_id) {
                    clip.frei0r_effects.remove(pos);
                    proj.dirty = true;
                    true
                } else {
                    false
                }
            } else {
                false
            };
            drop(proj);
            if found {
                on_project_changed_full();
            }
            reply.send(json!({"success": found})).ok();
        }

        McpCommand::SetClipFrei0rEffectParams {
            clip_id,
            effect_id,
            params,
            string_params,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                if let Some(effect) = clip.frei0r_effects.iter_mut().find(|e| e.id == effect_id) {
                    for (k, v) in params {
                        effect.params.insert(k, v);
                    }
                    if let Some(sp) = string_params {
                        for (k, v) in sp {
                            effect.string_params.insert(k, v);
                        }
                    }
                    proj.dirty = true;
                    true
                } else {
                    false
                }
            } else {
                false
            };
            drop(proj);
            if found {
                on_project_changed_full();
            }
            reply.send(json!({"success": found})).ok();
        }

        McpCommand::ReorderClipFrei0rEffects {
            clip_id,
            effect_ids,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                // Build new order from effect_ids.
                let mut reordered = Vec::with_capacity(effect_ids.len());
                for eid in &effect_ids {
                    if let Some(pos) = clip.frei0r_effects.iter().position(|e| &e.id == eid) {
                        reordered.push(clip.frei0r_effects[pos].clone());
                    }
                }
                // Append any effects not mentioned (safety net).
                for e in &clip.frei0r_effects {
                    if !effect_ids.contains(&e.id) {
                        reordered.push(e.clone());
                    }
                }
                clip.frei0r_effects = reordered;
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            if found {
                on_project_changed_full();
            }
            reply.send(json!({"success": found})).ok();
        }

        McpCommand::AddTitleClip {
            template_id,
            track_index,
            timeline_start_ns,
            duration_ns,
            title_text,
            reply,
        } => {
            let template = crate::ui::title_templates::find_template(&template_id);
            if template.is_none() {
                reply
                    .send(json!({"error": format!("Unknown template: {template_id}")}))
                    .ok();
            } else {
                let template = template.unwrap();
                let playhead = timeline_state.borrow().playhead_ns;
                let start = timeline_start_ns.unwrap_or(playhead);
                let mut clip = crate::ui::title_templates::create_title_clip(template, start);
                if let Some(dur) = duration_ns {
                    clip.source_out = dur;
                }
                if let Some(text) = title_text {
                    clip.title_text = text.clone();
                    clip.label = text;
                }
                let clip_id = clip.id.clone();
                let mut proj = project.borrow_mut();
                let ti = track_index
                    .unwrap_or_else(|| proj.tracks.iter().position(|t| t.is_video()).unwrap_or(0));
                if ti < proj.tracks.len() {
                    let magnetic_mode = timeline_state.borrow().magnetic_mode;
                    let change = insert_clip_at_playhead_on_track(
                        &mut proj.tracks[ti],
                        clip,
                        start,
                        magnetic_mode,
                    );
                    let cmd = crate::undo::SetTrackClipsCommand {
                        track_id: change.track_id,
                        old_clips: change.old_clips,
                        new_clips: change.new_clips,
                        label: "Add title clip (MCP)".to_string(),
                    };
                    drop(proj);
                    {
                        let mut st = timeline_state.borrow_mut();
                        let mut proj = project.borrow_mut();
                        st.history.execute(Box::new(cmd), &mut proj);
                    }
                    on_project_changed_full();
                    reply.send(json!({"clip_id": clip_id})).ok();
                } else {
                    drop(proj);
                    reply
                        .send(json!({"error": "track_index out of range"}))
                        .ok();
                }
            }
        }

        McpCommand::AddAdjustmentLayer {
            track_index,
            timeline_start_ns,
            duration_ns,
            reply,
        } => {
            let clip = crate::model::clip::Clip::new_adjustment(timeline_start_ns, duration_ns);
            let clip_id = clip.id.clone();
            let proj_ref = project.borrow();
            if track_index < proj_ref.tracks.len() {
                let track_id = proj_ref.tracks[track_index].id.clone();
                drop(proj_ref);
                let cmd = crate::undo::AddAdjustmentLayerCommand { clip, track_id };
                {
                    let mut st = timeline_state.borrow_mut();
                    let mut proj = project.borrow_mut();
                    st.history.execute(Box::new(cmd), &mut proj);
                }
                on_project_changed_full();
                reply.send(json!({"clip_id": clip_id})).ok();
            } else {
                drop(proj_ref);
                reply
                    .send(json!({"error": "track_index out of range"}))
                    .ok();
            }
        }

        McpCommand::SetClipTitleStyle {
            clip_id,
            title_text,
            title_font,
            title_color,
            title_x,
            title_y,
            title_outline_width,
            title_outline_color,
            title_shadow,
            title_shadow_color,
            title_shadow_offset_x,
            title_shadow_offset_y,
            title_bg_box,
            title_bg_box_color,
            title_bg_box_padding,
            title_clip_bg_color,
            title_secondary_text,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj
                .tracks
                .iter_mut()
                .flat_map(|t| t.clips.iter_mut())
                .find(|c| c.id == clip_id)
            {
                if let Some(v) = title_text {
                    clip.title_text = v;
                }
                if let Some(v) = title_font {
                    clip.title_font = v;
                }
                if let Some(v) = title_color {
                    clip.title_color = v;
                }
                if let Some(v) = title_x {
                    clip.title_x = v;
                }
                if let Some(v) = title_y {
                    clip.title_y = v;
                }
                if let Some(v) = title_outline_width {
                    clip.title_outline_width = v;
                }
                if let Some(v) = title_outline_color {
                    clip.title_outline_color = v;
                }
                if let Some(v) = title_shadow {
                    clip.title_shadow = v;
                }
                if let Some(v) = title_shadow_color {
                    clip.title_shadow_color = v;
                }
                if let Some(v) = title_shadow_offset_x {
                    clip.title_shadow_offset_x = v;
                }
                if let Some(v) = title_shadow_offset_y {
                    clip.title_shadow_offset_y = v;
                }
                if let Some(v) = title_bg_box {
                    clip.title_bg_box = v;
                }
                if let Some(v) = title_bg_box_color {
                    clip.title_bg_box_color = v;
                }
                if let Some(v) = title_bg_box_padding {
                    clip.title_bg_box_padding = v;
                }
                if let Some(v) = title_clip_bg_color {
                    clip.title_clip_bg_color = v;
                }
                if let Some(v) = title_secondary_text {
                    clip.title_secondary_text = v;
                }
                proj.dirty = true;
                true
            } else {
                false
            };
            // Lightweight live update: read the final clip values while we
            // still hold borrow_mut, then drop the borrow and push to player.
            let title_vals = if found {
                proj.tracks
                    .iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == clip_id)
                    .map(|clip| {
                        (
                            clip.title_text.clone(),
                            clip.title_font.clone(),
                            clip.title_color,
                            clip.title_x,
                            clip.title_y,
                            clip.title_outline_width,
                            clip.title_outline_color,
                            clip.title_shadow,
                            clip.title_bg_box,
                        )
                    })
            } else {
                None
            };
            drop(proj);
            if let Some((text, font, color, x, y, ow, oc, shadow, bg)) = title_vals {
                let pp = prog_player.borrow();
                pp.update_current_title(&text, &font, color, x, y);
                pp.update_current_title_style(ow, oc, shadow, bg);
                pp.flush_compositor_for_title_update();
            }
            reply.send(json!({"success": found})).ok();
        }
        McpCommand::AddToExportQueue {
            output_path,
            preset_name,
            reply,
        } => {
            if output_path.is_empty() {
                reply
                    .send(json!({"success": false, "error": "output_path is required"}))
                    .ok();
                return;
            }
            let preset = if let Some(name) = preset_name {
                let state = crate::ui_state::load_export_presets_state();
                if let Some(p) = state
                    .presets
                    .iter()
                    .find(|p| p.name.eq_ignore_ascii_case(&name))
                {
                    p.clone()
                } else {
                    reply.send(json!({"success": false, "error": format!("preset '{name}' not found")})).ok();
                    return;
                }
            } else {
                let state = crate::ui_state::load_export_presets_state();
                let last = state.last_used_preset.clone();
                state
                    .presets
                    .into_iter()
                    .find(|p| Some(&p.name) == last.as_ref())
                    .or_else(|| {
                        let defaults = crate::ui_state::load_export_presets_state().presets;
                        defaults.into_iter().next()
                    })
                    .unwrap_or_else(|| crate::ui_state::ExportPreset {
                        name: "default".to_string(),
                        video_codec: crate::ui_state::ExportVideoCodec::H264,
                        container: crate::ui_state::ExportContainer::Mp4,
                        output_width: 1920,
                        output_height: 1080,
                        crf: 23,
                        audio_codec: crate::ui_state::ExportAudioCodec::Aac,
                        audio_bitrate_kbps: 192,
                        gif_fps: None,
                        audio_channel_layout:
                            crate::ui_state::ExportAudioChannelLayout::Stereo,
                    })
            };
            let job = crate::ui_state::ExportQueueJob::new(&output_path, preset);
            let job_id = job.id.clone();
            let job_label = job.label.clone();
            let mut queue = crate::ui_state::load_export_queue_state();
            queue.jobs.push(job);
            crate::ui_state::save_export_queue_state(&queue);
            reply
                .send(json!({"success": true, "id": job_id, "label": job_label}))
                .ok();
        }
        McpCommand::ListExportQueue { reply } => {
            let queue = crate::ui_state::load_export_queue_state();
            let jobs: Vec<serde_json::Value> = queue
                .jobs
                .iter()
                .map(|j| {
                    json!({
                        "id": j.id,
                        "label": j.label,
                        "output_path": j.output_path,
                        "status": format!("{:?}", j.status).to_lowercase(),
                        "error": j.error
                    })
                })
                .collect();
            let count = jobs.len();
            reply.send(json!({"jobs": jobs, "count": count})).ok();
        }
        McpCommand::ClearExportQueue {
            status_filter,
            reply,
        } => {
            let mut queue = crate::ui_state::load_export_queue_state();
            let filter = status_filter.as_deref().unwrap_or("all");
            let before = queue.jobs.len();
            queue.jobs.retain(|j| match filter {
                "done" => j.status != crate::ui_state::ExportQueueJobStatus::Done,
                "error" => j.status != crate::ui_state::ExportQueueJobStatus::Error,
                _ => false, // "all" removes everything, so retain nothing
            });
            crate::ui_state::save_export_queue_state(&queue);
            let removed = before - queue.jobs.len();
            reply
                .send(json!({"success": true, "removed": removed}))
                .ok();
        }
        McpCommand::RunExportQueue { reply } => {
            let queue = crate::ui_state::load_export_queue_state();
            let pending: Vec<crate::ui_state::ExportQueueJob> = queue
                .jobs
                .iter()
                .filter(|j| j.status == crate::ui_state::ExportQueueJobStatus::Pending)
                .cloned()
                .collect();
            if pending.is_empty() {
                reply
                    .send(json!({"success": true, "message": "No pending jobs.", "results": []}))
                    .ok();
                return;
            }
            let proj_snapshot = project.borrow().clone();
            let bg_paths = bg_removal_cache.borrow().paths.clone();
            let interp_paths = frame_interp_cache
                .borrow()
                .snapshot_paths_by_clip_id(&proj_snapshot);
            std::thread::spawn(move || {
                let mut results = vec![];
                for job in &pending {
                    {
                        let mut q = crate::ui_state::load_export_queue_state();
                        if let Some(j) = q.jobs.iter_mut().find(|j| j.id == job.id) {
                            j.status = crate::ui_state::ExportQueueJobStatus::Running;
                        }
                        crate::ui_state::save_export_queue_state(&q);
                    }
                    let opts = job.options.to_export_options();
                    let (ptx, _prx) =
                        std::sync::mpsc::channel::<crate::media::export::ExportProgress>();
                    let export_result = crate::media::export::export_project(
                        &proj_snapshot,
                        &job.output_path,
                        opts,
                        None,
                        &bg_paths,
                        &interp_paths,
                        ptx,
                    );
                    let (new_status, err_msg) = match export_result {
                        Ok(()) => (crate::ui_state::ExportQueueJobStatus::Done, None),
                        Err(e) => (
                            crate::ui_state::ExportQueueJobStatus::Error,
                            Some(e.to_string()),
                        ),
                    };
                    {
                        let mut q = crate::ui_state::load_export_queue_state();
                        if let Some(j) = q.jobs.iter_mut().find(|j| j.id == job.id) {
                            j.status = new_status.clone();
                            j.error = err_msg.clone();
                        }
                        crate::ui_state::save_export_queue_state(&q);
                    }
                    results.push(json!({
                        "id": job.id,
                        "label": job.label,
                        "output_path": job.output_path,
                        "status": format!("{:?}", new_status).to_lowercase(),
                        "error": err_msg
                    }));
                }
                reply
                    .send(json!({"success": true, "results": results}))
                    .ok();
            });
        }
        McpCommand::CreateCompoundClip { clip_ids, reply } => {
            if clip_ids.len() < 2 {
                reply
                    .send(json!({"error": "At least 2 clip IDs required"}))
                    .ok();
                return;
            }
            // Select the specified clips in the timeline state, then create compound
            {
                let mut st = timeline_state.borrow_mut();
                st.set_selected_clip_ids(clip_ids.iter().cloned().collect());
                let changed = st.create_compound_from_selection();
                let proj_cb = st.on_project_changed.clone();
                drop(st);
                if changed {
                    if let Some(cb) = proj_cb {
                        cb();
                    }
                    // Find the compound clip ID (most recently added compound clip)
                    let proj = project.borrow();
                    let compound_id = proj
                        .tracks
                        .iter()
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.is_compound())
                        .map(|c| c.id.clone())
                        .unwrap_or_default();
                    reply
                        .send(json!({"success": true, "compound_clip_id": compound_id}))
                        .ok();
                } else {
                    reply
                        .send(json!({"error": "Failed to create compound clip"}))
                        .ok();
                }
            }
        }
        McpCommand::BreakApartCompoundClip { clip_id, reply } => {
            let mut st = timeline_state.borrow_mut();
            st.set_selected_clip_ids([clip_id.clone()].into_iter().collect());
            let changed = st.break_apart_compound();
            let proj_cb = st.on_project_changed.clone();
            drop(st);
            if changed {
                if let Some(cb) = proj_cb {
                    cb();
                }
                reply.send(json!({"success": true})).ok();
            } else {
                reply
                    .send(json!({"error": "Failed to break apart compound clip (not a compound clip or not found)"}))
                    .ok();
            }
        }
        McpCommand::CreateMulticamClip { clip_ids, reply } => {
            if clip_ids.len() < 2 {
                reply
                    .send(json!({"error": "At least 2 clip IDs required"}))
                    .ok();
                return;
            }
            // Collect clip info for audio sync
            let clip_infos: Vec<(String, String, u64, u64, u64, String)> = {
                let proj = project.borrow();
                clip_ids
                    .iter()
                    .filter_map(|id| {
                        proj.tracks
                            .iter()
                            .flat_map(|t| t.clips.iter().map(move |c| (t.id.clone(), c)))
                            .find(|(_, c)| &c.id == id)
                            .map(|(track_id, c)| {
                                (
                                    c.id.clone(),
                                    c.source_path.clone(),
                                    c.source_in,
                                    c.source_out,
                                    c.timeline_start,
                                    track_id,
                                )
                            })
                    })
                    .collect()
            };
            if clip_infos.len() < 2 {
                reply
                    .send(json!({"error": "Could not find 2+ clips with the provided IDs"}))
                    .ok();
                return;
            }
            // Run audio sync synchronously (MCP is blocking)
            let sync_clips: Vec<(String, String, u64, u64)> = clip_infos
                .iter()
                .map(|(id, path, src_in, src_out, _, _)| {
                    (id.clone(), path.clone(), *src_in, *src_out)
                })
                .collect();
            let sync_results = crate::media::audio_sync::sync_clips_by_audio(&sync_clips);
            let anchor_id = clip_infos[0].0.clone();
            let anchor_start = clip_infos[0].4;
            // Build multicam angles from sync results
            let mut angles: Vec<crate::model::clip::MulticamAngle> = Vec::new();
            for (i, (id, path, src_in, src_out, _tl_start, _track_id)) in
                clip_infos.iter().enumerate()
            {
                let offset_ns = if *id == anchor_id {
                    0i64
                } else {
                    sync_results
                        .iter()
                        .find(|r| r.clip_id == *id)
                        .map(|r| r.offset_ns)
                        .unwrap_or(0)
                };
                let label = format!("Angle {}", i + 1);
                let synced_in = (*src_in as i64 + offset_ns).max(0) as u64;
                let synced_out = *src_out;
                angles.push(crate::model::clip::MulticamAngle {
                    id: uuid::Uuid::new_v4().to_string(),
                    label,
                    source_path: path.clone(),
                    source_in: synced_in,
                    source_out: synced_out,
                    sync_offset_ns: offset_ns,
                    source_timecode_base_ns: None,
                    media_duration_ns: None,
                    volume: if i == 0 { 1.0 } else { 0.0 },
                    muted: i != 0,
                });
            }
            if angles.len() >= 2 {
                let multicam = crate::model::clip::Clip::new_multicam(anchor_start, angles);
                let multicam_id = multicam.id.clone();
                let selected_ids: std::collections::HashSet<String> =
                    clip_infos.iter().map(|(id, ..)| id.clone()).collect();
                let mut proj = project.borrow_mut();
                let mut changes = Vec::new();
                let mut placement_track_id: Option<String> = None;
                for track in &proj.tracks {
                    if track.clips.iter().any(|c| selected_ids.contains(&c.id)) {
                        let old_clips = track.clips.clone();
                        let mut new_clips: Vec<crate::model::clip::Clip> = track
                            .clips
                            .iter()
                            .filter(|c| !selected_ids.contains(&c.id))
                            .cloned()
                            .collect();
                        if placement_track_id.is_none() {
                            new_clips.push(multicam.clone());
                            new_clips.sort_by_key(|c| c.timeline_start);
                            placement_track_id = Some(track.id.clone());
                        }
                        changes.push(crate::undo::TrackClipsChange {
                            track_id: track.id.clone(),
                            old_clips,
                            new_clips,
                        });
                    }
                }
                let cmd = Box::new(crate::undo::SetMultipleTracksClipsCommand {
                    changes,
                    label: "Create Multicam Clip".to_string(),
                });
                {
                    let mut st = timeline_state.borrow_mut();
                    st.history.execute(cmd, &mut proj);
                }
                drop(proj);
                on_project_changed();
                reply
                    .send(json!({"success": true, "multicam_clip_id": multicam_id}))
                    .ok();
            } else {
                reply
                    .send(json!({"error": "Failed to create multicam clip (audio sync produced fewer than 2 angles)"}))
                    .ok();
            }
        }
        McpCommand::AddAngleSwitch {
            clip_id,
            position_ns,
            angle_index,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let clip = proj
                .tracks
                .iter_mut()
                .flat_map(|t| t.clips.iter_mut())
                .find(|c| c.id == clip_id);
            if let Some(clip) = clip {
                if clip.kind != crate::model::clip::ClipKind::Multicam {
                    reply
                        .send(json!({"error": "Clip is not a multicam clip"}))
                        .ok();
                } else {
                    let num_angles = clip.multicam_angles.as_ref().map(|a| a.len()).unwrap_or(0);
                    if angle_index >= num_angles {
                        reply
                            .send(json!({"error": format!("angle_index {} out of range (clip has {} angles)", angle_index, num_angles)}))
                            .ok();
                    } else {
                        clip.insert_angle_switch(position_ns, angle_index);
                        drop(proj);
                        on_project_changed();
                        reply.send(json!({"success": true})).ok();
                    }
                }
            } else {
                reply
                    .send(json!({"error": format!("Clip not found: {clip_id}")}))
                    .ok();
            }
        }
        McpCommand::ListMulticamAngles { clip_id, reply } => {
            let proj = project.borrow();
            let clip = proj
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id);
            if let Some(clip) = clip {
                if clip.kind != crate::model::clip::ClipKind::Multicam {
                    reply
                        .send(json!({"error": "Clip is not a multicam clip"}))
                        .ok();
                } else {
                    let angles: Vec<serde_json::Value> = clip
                        .multicam_angles
                        .as_ref()
                        .map(|a| {
                            a.iter()
                                .enumerate()
                                .map(|(i, angle)| {
                                    json!({
                                        "index": i,
                                        "id": angle.id,
                                        "label": angle.label,
                                        "source_path": angle.source_path,
                                        "source_in": angle.source_in,
                                        "source_out": angle.source_out,
                                        "sync_offset_ns": angle.sync_offset_ns,
                                        "volume": angle.volume,
                                        "muted": angle.muted,
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let switches: Vec<serde_json::Value> = clip
                        .multicam_switches
                        .as_ref()
                        .map(|s| {
                            s.iter()
                                .map(|sw| {
                                    json!({
                                        "position_ns": sw.position_ns,
                                        "angle_index": sw.angle_index,
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    reply
                        .send(json!({
                            "clip_id": clip_id,
                            "angles": angles,
                            "switches": switches,
                        }))
                        .ok();
                }
            } else {
                reply
                    .send(json!({"error": format!("Clip not found: {clip_id}")}))
                    .ok();
            }
        }
        McpCommand::SetMulticamAngleAudio {
            clip_id,
            angle_index,
            volume,
            muted,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            if let Some(clip) = proj.clip_mut(&clip_id) {
                if !clip.is_multicam() {
                    reply
                        .send(json!({"error": "Clip is not a multicam clip"}))
                        .ok();
                    return;
                }
                if let Some(ref mut angles) = clip.multicam_angles {
                    if angle_index >= angles.len() {
                        reply.send(json!({"error": format!("Angle index {} out of range (0..{})", angle_index, angles.len())})).ok();
                        return;
                    }
                    if let Some(v) = volume {
                        angles[angle_index].volume = v.clamp(0.0, 2.0);
                    }
                    if let Some(m) = muted {
                        angles[angle_index].muted = m;
                    }
                    proj.dirty = true;
                    drop(proj);
                    on_project_changed();
                    reply.send(json!({"success": true})).ok();
                } else {
                    reply
                        .send(json!({"error": "Multicam clip has no angles"}))
                        .ok();
                }
            } else {
                reply
                    .send(json!({"error": format!("Clip not found: {clip_id}")}))
                    .ok();
            }
        }
        // ── Audition / clip-versions commands ─────────────────────────────
        McpCommand::CreateAuditionClip {
            clip_ids,
            active_index,
            reply,
        } => {
            if clip_ids.len() < 2 {
                reply
                    .send(json!({"error": "At least 2 clip IDs required"}))
                    .ok();
                return;
            }
            // Collect (clip, track_id) pairs.
            let hits: Vec<(crate::model::clip::Clip, String)> = {
                let proj = project.borrow();
                clip_ids
                    .iter()
                    .filter_map(|id| {
                        proj.tracks
                            .iter()
                            .flat_map(|t| t.clips.iter().map(move |c| (t.id.clone(), c)))
                            .find(|(_, c)| &c.id == id)
                            .map(|(tid, c)| (c.clone(), tid))
                    })
                    .collect()
            };
            if hits.len() < 2 {
                reply
                    .send(json!({"error": "Could not find 2+ clips with the provided IDs"}))
                    .ok();
                return;
            }
            // All clips must be on the same track.
            let first_track = hits[0].1.clone();
            if hits.iter().any(|(_, t)| t != &first_track) {
                reply
                    .send(json!({"error": "All audition takes must come from the same track"}))
                    .ok();
                return;
            }
            // Build takes from the original clips, in caller-provided order.
            let active_index = active_index.min(hits.len() - 1);
            let anchor_start = hits[active_index].0.timeline_start;
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
            let audition = crate::model::clip::Clip::new_audition(anchor_start, takes, active_index);
            let audition_id = audition.id.clone();
            let selected_ids: std::collections::HashSet<String> = hits
                .iter()
                .map(|(c, _)| c.id.clone())
                .collect();
            let mut proj = project.borrow_mut();
            let mut changes = Vec::new();
            for track in &proj.tracks {
                if track.id != first_track {
                    continue;
                }
                let old_clips = track.clips.clone();
                let mut new_clips: Vec<crate::model::clip::Clip> = track
                    .clips
                    .iter()
                    .filter(|c| !selected_ids.contains(&c.id))
                    .cloned()
                    .collect();
                new_clips.push(audition.clone());
                new_clips.sort_by_key(|c| c.timeline_start);
                changes.push(crate::undo::TrackClipsChange {
                    track_id: track.id.clone(),
                    old_clips,
                    new_clips,
                });
            }
            let cmd = Box::new(crate::undo::SetMultipleTracksClipsCommand {
                changes,
                label: "Create Audition".to_string(),
            });
            {
                let mut st = timeline_state.borrow_mut();
                st.history.execute(cmd, &mut proj);
            }
            drop(proj);
            on_project_changed();
            reply
                .send(json!({"success": true, "audition_clip_id": audition_id}))
                .ok();
        }
        McpCommand::AddAuditionTake {
            audition_clip_id,
            source_path,
            source_in_ns,
            source_out_ns,
            label,
            reply,
        } => {
            let exists_and_is_audition = {
                let proj = project.borrow();
                proj.clip_ref(&audition_clip_id)
                    .map(|c| c.is_audition())
                    .unwrap_or(false)
            };
            if !exists_and_is_audition {
                reply
                    .send(json!({"error": "Clip is not an audition clip"}))
                    .ok();
                return;
            }
            let take = crate::model::clip::AuditionTake {
                id: uuid::Uuid::new_v4().to_string(),
                label: label.unwrap_or_else(|| {
                    std::path::Path::new(&source_path)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("Take")
                        .to_string()
                }),
                source_path,
                source_in: source_in_ns,
                source_out: source_out_ns,
                source_timecode_base_ns: None,
                media_duration_ns: None,
            };
            let take_id = take.id.clone();
            let cmd = Box::new(crate::undo::AddAuditionTakeCommand {
                clip_id: audition_clip_id,
                take,
            });
            {
                let mut proj = project.borrow_mut();
                let mut st = timeline_state.borrow_mut();
                st.history.execute(cmd, &mut proj);
            }
            on_project_changed();
            reply
                .send(json!({"success": true, "take_id": take_id}))
                .ok();
        }
        McpCommand::RemoveAuditionTake {
            audition_clip_id,
            take_index,
            reply,
        } => {
            let (is_audition, active_index, num_takes) = {
                let proj = project.borrow();
                proj.clip_ref(&audition_clip_id)
                    .map(|c| {
                        (
                            c.is_audition(),
                            c.audition_active_take_index,
                            c.audition_takes.as_ref().map(|t| t.len()).unwrap_or(0),
                        )
                    })
                    .unwrap_or((false, 0, 0))
            };
            if !is_audition {
                reply
                    .send(json!({"error": "Clip is not an audition clip"}))
                    .ok();
                return;
            }
            if take_index >= num_takes {
                reply
                    .send(json!({"error": format!("take_index {take_index} out of range (clip has {num_takes} takes)")}))
                    .ok();
                return;
            }
            if take_index == active_index {
                reply
                    .send(json!({"error": "Cannot remove the active take. Switch to a different take first."}))
                    .ok();
                return;
            }
            let cmd = Box::new(crate::undo::RemoveAuditionTakeCommand {
                clip_id: audition_clip_id,
                take_index,
                removed: std::cell::RefCell::new(None),
            });
            {
                let mut proj = project.borrow_mut();
                let mut st = timeline_state.borrow_mut();
                st.history.execute(cmd, &mut proj);
            }
            on_project_changed();
            reply.send(json!({"success": true})).ok();
        }
        McpCommand::SetActiveAuditionTake {
            audition_clip_id,
            take_index,
            reply,
        } => {
            let (snapshot, num_takes) = {
                let proj = project.borrow();
                let clip = proj.clip_ref(&audition_clip_id);
                let snap = clip.cloned();
                let n = snap
                    .as_ref()
                    .and_then(|c| c.audition_takes.as_ref().map(|t| t.len()))
                    .unwrap_or(0);
                (snap, n)
            };
            if !snapshot.as_ref().map(|c| c.is_audition()).unwrap_or(false) {
                reply
                    .send(json!({"error": "Clip is not an audition clip"}))
                    .ok();
                return;
            }
            if take_index >= num_takes {
                reply
                    .send(json!({"error": format!("take_index {take_index} out of range (clip has {num_takes} takes)")}))
                    .ok();
                return;
            }
            let cmd = Box::new(crate::undo::SetActiveAuditionTakeCommand {
                clip_id: audition_clip_id,
                new_index: take_index,
                before_snapshot: snapshot,
            });
            {
                let mut proj = project.borrow_mut();
                let mut st = timeline_state.borrow_mut();
                st.history.execute(cmd, &mut proj);
            }
            on_project_changed();
            reply.send(json!({"success": true})).ok();
        }
        McpCommand::ListAuditionTakes {
            audition_clip_id,
            reply,
        } => {
            let proj = project.borrow();
            let Some(clip) = proj.clip_ref(&audition_clip_id) else {
                reply
                    .send(json!({"error": format!("Clip not found: {audition_clip_id}")}))
                    .ok();
                return;
            };
            if !clip.is_audition() {
                reply
                    .send(json!({"error": "Clip is not an audition clip"}))
                    .ok();
                return;
            }
            let takes: Vec<serde_json::Value> = clip
                .audition_takes
                .as_ref()
                .map(|takes| {
                    takes
                        .iter()
                        .enumerate()
                        .map(|(i, t)| {
                            json!({
                                "index": i,
                                "id": t.id,
                                "label": t.label,
                                "source_path": t.source_path,
                                "source_in_ns": t.source_in,
                                "source_out_ns": t.source_out,
                                "source_timecode_base_ns": t.source_timecode_base_ns,
                                "media_duration_ns": t.media_duration_ns,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            reply
                .send(json!({
                    "clip_id": audition_clip_id,
                    "active_take_index": clip.audition_active_take_index,
                    "takes": takes,
                }))
                .ok();
        }
        McpCommand::FinalizeAudition {
            audition_clip_id,
            reply,
        } => {
            let snapshot = project.borrow().clip_ref(&audition_clip_id).cloned();
            if !snapshot.as_ref().map(|c| c.is_audition()).unwrap_or(false) {
                reply
                    .send(json!({"error": "Clip is not an audition clip"}))
                    .ok();
                return;
            }
            let cmd = Box::new(crate::undo::FinalizeAuditionCommand {
                clip_id: audition_clip_id,
                before_snapshot: snapshot,
            });
            {
                let mut proj = project.borrow_mut();
                let mut st = timeline_state.borrow_mut();
                st.history.execute(cmd, &mut proj);
            }
            on_project_changed();
            reply.send(json!({"success": true})).ok();
        }
        // ── Subtitle / STT commands ────────────────────────────────────────
        McpCommand::GenerateSubtitles {
            clip_id,
            language,
            reply,
        } => {
            let proj = project.borrow();
            let clip_info = proj
                .clip_ref(&clip_id)
                .map(|c| (c.source_path.clone(), c.source_in, c.source_out));
            drop(proj);
            if let Some((source_path, source_in, source_out)) = clip_info {
                stt_cache
                    .borrow_mut()
                    .request(&source_path, source_in, source_out, &language);
                reply
                    .send(json!({"success": true, "status": "queued"}))
                    .ok();
            } else {
                reply
                    .send(json!({"error": format!("Clip not found: {clip_id}")}))
                    .ok();
            }
        }
        McpCommand::GetClipSubtitles { clip_id, reply } => {
            let proj = project.borrow();
            if let Some(clip) = proj.clip_ref(&clip_id) {
                reply
                    .send(json!({
                        "clip_id": clip_id,
                        "language": &clip.subtitles_language,
                        "segments": clip.subtitle_segments.iter().map(|s| json!({
                            "id": s.id,
                            "start_ns": s.start_ns,
                            "end_ns": s.end_ns,
                            "text": s.text,
                            "words": s.words.iter().map(|w| json!({
                                "start_ns": w.start_ns,
                                "end_ns": w.end_ns,
                                "text": w.text,
                            })).collect::<Vec<_>>(),
                        })).collect::<Vec<_>>(),
                    }))
                    .ok();
            } else {
                reply
                    .send(json!({"error": format!("Clip not found: {clip_id}")}))
                    .ok();
            }
        }
        McpCommand::EditSubtitleText {
            clip_id,
            segment_id,
            text,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            if let Some(clip) = proj.clip_mut(&clip_id) {
                if let Some(seg) = clip
                    .subtitle_segments
                    .iter_mut()
                    .find(|s| s.id == segment_id)
                {
                    seg.text = text;
                    proj.dirty = true;
                    drop(proj);
                    on_project_changed();
                    reply.send(json!({"success": true})).ok();
                } else {
                    reply
                        .send(json!({"error": format!("Segment not found: {segment_id}")}))
                        .ok();
                }
            } else {
                reply
                    .send(json!({"error": format!("Clip not found: {clip_id}")}))
                    .ok();
            }
        }
        McpCommand::EditSubtitleTiming {
            clip_id,
            segment_id,
            start_ns,
            end_ns,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            if let Some(clip) = proj.clip_mut(&clip_id) {
                if let Some(seg) = clip
                    .subtitle_segments
                    .iter_mut()
                    .find(|s| s.id == segment_id)
                {
                    seg.start_ns = start_ns;
                    seg.end_ns = end_ns;
                    proj.dirty = true;
                    drop(proj);
                    on_project_changed();
                    reply.send(json!({"success": true})).ok();
                } else {
                    reply
                        .send(json!({"error": format!("Segment not found: {segment_id}")}))
                        .ok();
                }
            } else {
                reply
                    .send(json!({"error": format!("Clip not found: {clip_id}")}))
                    .ok();
            }
        }
        McpCommand::ClearSubtitles { clip_id, reply } => {
            let mut proj = project.borrow_mut();
            if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.subtitle_segments.clear();
                proj.dirty = true;
                drop(proj);
                on_project_changed();
                reply.send(json!({"success": true})).ok();
            } else {
                reply
                    .send(json!({"error": format!("Clip not found: {clip_id}")}))
                    .ok();
            }
        }
        McpCommand::DeleteTranscriptRange {
            clip_id,
            start_word_index,
            end_word_index,
            reply,
        } => {
            // Resolve word indices to clip-local time bounds inside a scoped
            // borrow of the project, then call the same TimelineState helper
            // the UI uses so the edit is one undo entry and walks compound
            // tracks correctly.
            let resolved: Result<(u64, u64), String> = {
                let proj = project.borrow();
                if let Some(clip) = proj.clip_ref(&clip_id) {
                    if end_word_index <= start_word_index {
                        Err(format!(
                            "end_word_index ({end_word_index}) must be > start_word_index ({start_word_index})"
                        ))
                    } else {
                        // Flatten clip words in segment-then-word order.
                        let mut flat: Vec<(u64, u64)> = Vec::new();
                        for seg in &clip.subtitle_segments {
                            for w in &seg.words {
                                flat.push((w.start_ns, w.end_ns));
                            }
                        }
                        let last_idx = end_word_index.saturating_sub(1) as usize;
                        if (start_word_index as usize) >= flat.len() || last_idx >= flat.len() {
                            Err(format!(
                                "Word index out of range: clip has {} word(s)",
                                flat.len()
                            ))
                        } else {
                            let start_ns = flat[start_word_index as usize].0;
                            let end_ns = flat[last_idx].1;
                            Ok((start_ns, end_ns))
                        }
                    }
                } else {
                    Err(format!("Clip not found: {clip_id}"))
                }
            };

            match resolved {
                Ok((word_start_ns, word_end_ns)) => {
                    let changed = timeline_state
                        .borrow_mut()
                        .delete_transcript_word_range(&clip_id, word_start_ns, word_end_ns);
                    if changed {
                        on_project_changed();
                        reply
                            .send(json!({
                                "success": true,
                                "deleted_word_count": end_word_index - start_word_index,
                            }))
                            .ok();
                    } else {
                        reply
                            .send(json!({"error": "No change applied (zero-length range or clip not found)"}))
                            .ok();
                    }
                }
                Err(msg) => {
                    reply.send(json!({"error": msg})).ok();
                }
            }
        }
        McpCommand::SetSubtitleStyle {
            clip_id,
            font,
            color,
            outline_color,
            outline_width,
            bg_box,
            bg_box_color,
            highlight_mode,
            highlight_color,
            bold,
            italic,
            underline,
            shadow,
            highlight_bold,
            highlight_color_flag,
            highlight_underline,
            highlight_stroke,
            highlight_italic,
            highlight_background,
            highlight_shadow,
            bg_highlight_color,
            highlight_stroke_color,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            if let Some(clip) = proj.clip_mut(&clip_id) {
                if let Some(f) = font {
                    clip.subtitle_font = f;
                }
                if let Some(c) = color {
                    clip.subtitle_color = c;
                }
                if let Some(c) = outline_color {
                    clip.subtitle_outline_color = c;
                }
                if let Some(w) = outline_width {
                    clip.subtitle_outline_width = w;
                }
                if let Some(b) = bg_box {
                    clip.subtitle_bg_box = b;
                }
                if let Some(c) = bg_box_color {
                    clip.subtitle_bg_box_color = c;
                }
                // Legacy highlight_mode support: convert to flags
                if let Some(mode) = highlight_mode {
                    clip.subtitle_highlight_mode = match mode.as_str() {
                        "bold" => crate::model::clip::SubtitleHighlightMode::Bold,
                        "color" => crate::model::clip::SubtitleHighlightMode::Color,
                        "underline" => crate::model::clip::SubtitleHighlightMode::Underline,
                        "stroke" => crate::model::clip::SubtitleHighlightMode::Stroke,
                        _ => crate::model::clip::SubtitleHighlightMode::None,
                    };
                    // Also set flags from legacy mode for consistency
                    clip.subtitle_highlight_flags =
                        crate::model::clip::SubtitleHighlightFlags::from_legacy(
                            clip.subtitle_highlight_mode,
                        );
                }
                if let Some(c) = highlight_color {
                    clip.subtitle_highlight_color = c;
                }
                // New base style fields
                if let Some(v) = bold {
                    clip.subtitle_bold = v;
                }
                if let Some(v) = italic {
                    clip.subtitle_italic = v;
                }
                if let Some(v) = underline {
                    clip.subtitle_underline = v;
                }
                if let Some(v) = shadow {
                    clip.subtitle_shadow = v;
                }
                // New highlight flag fields
                if let Some(v) = highlight_bold {
                    clip.subtitle_highlight_flags.bold = v;
                }
                if let Some(v) = highlight_color_flag {
                    clip.subtitle_highlight_flags.color = v;
                }
                if let Some(v) = highlight_underline {
                    clip.subtitle_highlight_flags.underline = v;
                }
                if let Some(v) = highlight_stroke {
                    clip.subtitle_highlight_flags.stroke = v;
                }
                if let Some(v) = highlight_italic {
                    clip.subtitle_highlight_flags.italic = v;
                }
                if let Some(v) = highlight_background {
                    clip.subtitle_highlight_flags.background = v;
                }
                if let Some(v) = highlight_shadow {
                    clip.subtitle_highlight_flags.shadow = v;
                }
                if let Some(c) = bg_highlight_color {
                    clip.subtitle_bg_highlight_color = c;
                }
                if let Some(c) = highlight_stroke_color {
                    clip.subtitle_highlight_stroke_color = c;
                }
                proj.dirty = true;
                drop(proj);
                on_project_changed();
                reply.send(json!({"success": true})).ok();
            } else {
                reply
                    .send(json!({"error": format!("Clip not found: {clip_id}")}))
                    .ok();
            }
        }
        McpCommand::ExportSrt { path, reply } => {
            let proj = project.borrow();
            match crate::media::export::export_srt(&proj, &path) {
                Ok(()) => reply.send(json!({"success": true, "path": path})).ok(),
                Err(e) => reply
                    .send(json!({"error": format!("SRT export failed: {e}")}))
                    .ok(),
            };
        }
    }
}
