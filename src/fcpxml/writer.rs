use crate::model::{
    media_library::MediaItem,
    project::{FrameRate, Project},
};
use anyhow::Result;
use quick_xml::events::{BytesEnd, BytesStart, Event};
use quick_xml::Writer;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy)]
struct WriterOptions {
    strict_dtd: bool,
}

/// Per-media-file export info probed at export time.
struct MediaExportInfo {
    format_id: String,
    fps: FrameRate,
    timecode_ns: Option<u64>,
    width: u32,
    height: u32,
    is_audio_only: bool,
    /// Duration in nanoseconds (probed from media file). Used for audio-only assets.
    duration_ns: Option<u64>,
}

/// Context built from probing media files before writing strict FCPXML.
struct ExportContext {
    /// Map from source_path to per-file info.
    media: HashMap<String, MediaExportInfo>,
    /// Additional format elements beyond r1: (format_id, fps, width, height).
    extra_formats: Vec<(String, FrameRate, u32, u32)>,
    /// Format ID for the FFVideoFormatRateUndefined audio format, if needed.
    audio_format_id: Option<String>,
}

/// Serialize a `Project` to FCPXML format.
pub fn write_fcpxml(project: &Project) -> Result<String> {
    write_fcpxml_with_options(project, WriterOptions { strict_dtd: false })
}

/// Serialize a `Project` to strict DTD-safe FCPXML for distribution workflows.
pub fn write_fcpxml_strict(project: &Project) -> Result<String> {
    write_fcpxml_with_options(project, WriterOptions { strict_dtd: true })
}

pub fn use_strict_fcpxml_for_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("fcpxml"))
        .unwrap_or(false)
}

pub fn write_fcpxml_for_path(project: &Project, path: &Path) -> Result<String> {
    if use_strict_fcpxml_for_path(path) {
        write_fcpxml_strict(project)
    } else {
        write_fcpxml(project)
    }
}

fn fcpxml_transition_name_for_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "cross_dissolve" => Some("Cross Dissolve"),
        "fade_to_black" => Some("Fade To Black"),
        "wipe_right" => Some("Wipe Right"),
        "wipe_left" => Some("Wipe Left"),
        _ => None,
    }
}

fn write_fcpxml_with_options(project: &Project, options: WriterOptions) -> Result<String> {
    let emit_vendor_extensions = !options.strict_dtd;
    let strip_unknown_fields = options.strict_dtd;
    if !options.strict_dtd {
        if let Some(original) = &project.source_fcpxml {
            if !project.dirty {
                return Ok(original.clone());
            }
            // The patch path only updates a subset of clip fields (transform,
            // color, title, keyframes).  Many fields — blend mode, speed,
            // reverse, freeze-frame, chroma key, LUT, grading extensions,
            // frei0r effects, transitions, audio, etc. — are NOT patched,
            // causing silent data loss.  Always use the full rewrite for
            // dirty projects to ensure all fields are persisted correctly.
            // (The non-dirty case above still returns the original verbatim.)
        }
    }

    let mut writer = Writer::new_with_indent(Cursor::new(Vec::new()), b' ', 4);

    // XML declaration
    writer.write_event(Event::Decl(quick_xml::events::BytesDecl::new(
        "1.0",
        Some("UTF-8"),
        None,
    )))?;
    writer.get_mut().write_all(b"\n<!DOCTYPE fcpxml>\n")?;

    // <fcpxml version="1.14">
    let mut fcpxml = BytesStart::new("fcpxml");
    fcpxml.push_attribute(("version", crate::fcpxml::FCPXML_EXPORT_VERSION));
    if emit_vendor_extensions {
        fcpxml.push_attribute(("xmlns:us", "urn:ultimateslice"));
    }
    if !strip_unknown_fields {
        for (k, v) in &project.fcpxml_unknown_root.attrs {
            if !is_writer_managed_fcpxml_attr(k) {
                fcpxml.push_attribute((k.as_str(), v.as_str()));
            }
        }
    }
    writer.write_event(Event::Start(fcpxml))?;

    // Build export context for strict FCPXML — probes media for native fps/timecode.
    let export_ctx = if options.strict_dtd {
        Some(build_export_context(project))
    } else {
        None
    };

    // Build a map from source path → shared asset ID so that multiple clips
    // referencing the same source file share a single <asset> element.
    let asset_id_by_source: HashMap<String, String> = {
        let mut map = HashMap::new();
        for track in project.video_tracks().chain(project.audio_tracks()) {
            for clip in &track.clips {
                let src = clip
                    .fcpxml_original_source_path
                    .as_deref()
                    .unwrap_or(&clip.source_path)
                    .to_string();
                map.entry(src)
                    .or_insert_with(|| format!("a_{}", sanitize_id(&clip.id)));
            }
        }
        map
    };

    // <resources>
    write_resources(
        project,
        &mut writer,
        options,
        export_ctx.as_ref(),
        &asset_id_by_source,
    )?;

    // <library>
    let mut library = BytesStart::new("library");
    if !strip_unknown_fields {
        for (k, v) in &project.fcpxml_unknown_library.attrs {
            if !is_writer_managed_library_attr(k) {
                library.push_attribute((k.as_str(), v.as_str()));
            }
        }
    }
    writer.write_event(Event::Start(library))?;

    let mut event = BytesStart::new("event");
    if !strip_unknown_fields {
        for (k, v) in &project.fcpxml_unknown_event.attrs {
            if !is_writer_managed_event_attr(k) {
                event.push_attribute((k.as_str(), v.as_str()));
            }
        }
        // Emit bin persistence attributes when present.
        if let Some(ref bins_json) = project.parsed_bins_json {
            if bins_json != "[]" {
                event.push_attribute(("us:bins", bins_json.as_str()));
            }
        }
        if let Some(ref media_bins_json) = project.parsed_media_bins_json {
            if media_bins_json != "{}" {
                event.push_attribute(("us:media-bins", media_bins_json.as_str()));
            }
        }
        if let Some(ref collections_json) = project.parsed_collections_json {
            if collections_json != "[]" {
                event.push_attribute(("us:smart-collections", collections_json.as_str()));
            }
        }
    }
    writer.write_event(Event::Start(event))?;

    // <project name="...">
    let mut proj_elem = BytesStart::new("project");
    proj_elem.push_attribute(("name", project.title.as_str()));
    if !strip_unknown_fields {
        for (k, v) in &project.fcpxml_unknown_project.attrs {
            if !is_writer_managed_project_attr(k) {
                proj_elem.push_attribute((k.as_str(), v.as_str()));
            }
        }
    }
    writer.write_event(Event::Start(proj_elem))?;

    // <sequence>
    let _fps = format!(
        "{}/{}",
        project.frame_rate.numerator, project.frame_rate.denominator
    );
    let duration_str = ns_to_fcpxml_time(project.duration(), &project.frame_rate);
    let format_ref = "r1";

    let mut seq = BytesStart::new("sequence");
    seq.push_attribute(("duration", duration_str.as_str()));
    seq.push_attribute(("format", format_ref));
    seq.push_attribute(("tcFormat", "NDF"));
    if options.strict_dtd {
        seq.push_attribute(("audioLayout", "stereo"));
        seq.push_attribute(("audioRate", "48k"));
    }
    if !strip_unknown_fields {
        for (k, v) in &project.fcpxml_unknown_sequence.attrs {
            if !is_writer_managed_sequence_attr(k) {
                seq.push_attribute((k.as_str(), v.as_str()));
            }
        }
    }
    writer.write_event(Event::Start(seq))?;

    // <spine>
    let mut spine = BytesStart::new("spine");
    if !strip_unknown_fields {
        for (k, v) in &project.fcpxml_unknown_spine.attrs {
            if !is_writer_managed_spine_attr(k) {
                spine.push_attribute((k.as_str(), v.as_str()));
            }
        }
    }
    writer.write_event(Event::Start(spine))?;

    if options.strict_dtd {
        // Strict mode: nest connected clips inside primary clips per FCPXML spec.
        // Connected clips (lane ≠ 0) must be children of the primary storyline
        // clip they connect to, not flat siblings in the <spine>.
        struct ConnectedTrackInfo {
            track_idx: usize,
            lane: i32,
        }
        let mut primary_track_idx: Option<usize> = None;
        let mut connected_tracks: Vec<ConnectedTrackInfo> = Vec::new();
        let mut video_kind_idx = 0usize;
        let mut audio_kind_idx = 0usize;
        for (track_idx, track) in project.tracks.iter().enumerate() {
            match track.kind {
                crate::model::track::TrackKind::Video => {
                    if video_kind_idx == 0 {
                        primary_track_idx = Some(track_idx);
                    } else {
                        connected_tracks.push(ConnectedTrackInfo {
                            track_idx,
                            lane: video_kind_idx as i32,
                        });
                    }
                    video_kind_idx += 1;
                }
                crate::model::track::TrackKind::Audio => {
                    connected_tracks.push(ConnectedTrackInfo {
                        track_idx,
                        lane: -((audio_kind_idx as i32) + 1),
                    });
                    audio_kind_idx += 1;
                }
            }
        }

        // Helper: write an asset-clip open tag with standard attributes.
        // `parent_clip` is set when writing connected clips to convert offset
        // from timeline space into the parent's source time space.
        let write_asset_clip_start = |writer: &mut Writer<Cursor<Vec<u8>>>,
                                      clip: &crate::model::clip::Clip,
                                      lane: Option<i32>,
                                      parent_clip: Option<&crate::model::clip::Clip>|
         -> Result<u64> {
            let clip_source = clip
                .fcpxml_original_source_path
                .as_deref()
                .unwrap_or(&clip.source_path);
            let asset_ref = asset_id_by_source
                .get(clip_source)
                .cloned()
                .unwrap_or_else(|| format!("a_{}", sanitize_id(&clip.id)));

            // Look up probed media info for this clip and its parent.
            let clip_media = export_ctx
                .as_ref()
                .and_then(|ctx| ctx.media.get(clip_source));
            let clip_is_audio_only = clip_media.map(|m| m.is_audio_only).unwrap_or(false);
            let clip_has_video = clip_media
                .map(|m| m.width > 0 && m.height > 0)
                .unwrap_or(clip.kind != crate::model::clip::ClipKind::Audio);
            // Audio-only clips use the FFVideoFormatRateUndefined format.
            // Their start/duration should use the audio time base (48kHz),
            // not the video frame rate.
            static AUDIO_FPS: FrameRate = FrameRate {
                numerator: 48000,
                denominator: 1,
            };
            let clip_fps = if clip_has_video {
                clip_media.map(|m| &m.fps).unwrap_or(&project.frame_rate)
            } else if clip_is_audio_only {
                &AUDIO_FPS
            } else {
                &project.frame_rate
            };
            let clip_format = if clip_is_audio_only {
                clip_media
                    .map(|m| m.format_id.as_str())
                    .unwrap_or(format_ref)
            } else if clip_has_video {
                clip_media
                    .map(|m| m.format_id.as_str())
                    .unwrap_or(format_ref)
            } else {
                format_ref
            };
            let clip_tc = if clip_has_video || clip_is_audio_only {
                clip_media.and_then(|m| m.timecode_ns)
            } else {
                None
            };

            let offset = if let Some(parent) = parent_clip {
                // Connected clip: offset in parent's source time space.
                let parent_source = parent
                    .fcpxml_original_source_path
                    .as_deref()
                    .unwrap_or(&parent.source_path);
                let parent_media = export_ctx
                    .as_ref()
                    .and_then(|ctx| ctx.media.get(parent_source));
                let parent_fps = parent_media.map(|m| &m.fps).unwrap_or(&project.frame_rate);
                let parent_tc = parent_media.and_then(|m| m.timecode_ns);
                let parent_source_start = parent_tc
                    .or(parent.source_timecode_base_ns)
                    .map(|tc| {
                        // Compute source_in in the media's time base
                        let tc_frames = (tc * parent_fps.numerator as u64
                            + parent_fps.denominator as u64 * 500_000_000)
                            / (parent_fps.denominator as u64 * 1_000_000_000);
                        let in_frames = (parent.source_in * parent_fps.numerator as u64
                            + parent_fps.denominator as u64 * 500_000_000)
                            / (parent_fps.denominator as u64 * 1_000_000_000);
                        (tc_frames + in_frames) * parent_fps.denominator as u64 * 1_000_000_000
                            / parent_fps.numerator as u64
                    })
                    .unwrap_or(
                        parent
                            .source_timecode_start_ns()
                            .unwrap_or(parent.source_in),
                    );
                let delta = clip.timeline_start.saturating_sub(parent.timeline_start);
                ns_to_fcpxml_time(parent_source_start + delta, parent_fps)
            } else {
                ns_to_fcpxml_time(clip.timeline_start, &project.frame_rate)
            };
            let duration = if clip_is_audio_only {
                ns_to_fcpxml_time(clip.duration(), clip_fps)
            } else {
                ns_to_fcpxml_time(clip.duration(), &project.frame_rate)
            };

            // Asset-clip start: position in the asset's source timeline.
            let source_start_ns = if clip_is_audio_only {
                // Audio-only: BWF timecode + source_in, in 48kHz time base.
                clip_tc
                    .map(|tc| {
                        let tc_samples = (tc * clip_fps.numerator as u64
                            + clip_fps.denominator as u64 * 500_000_000)
                            / (clip_fps.denominator as u64 * 1_000_000_000);
                        let in_samples = (clip.source_in * clip_fps.numerator as u64
                            + clip_fps.denominator as u64 * 500_000_000)
                            / (clip_fps.denominator as u64 * 1_000_000_000);
                        (tc_samples + in_samples) * clip_fps.denominator as u64 * 1_000_000_000
                            / clip_fps.numerator as u64
                    })
                    .unwrap_or(clip.source_in)
            } else {
                clip_tc
                    .or(clip.source_timecode_base_ns)
                    .map(|tc| {
                        let tc_frames = (tc * clip_fps.numerator as u64
                            + clip_fps.denominator as u64 * 500_000_000)
                            / (clip_fps.denominator as u64 * 1_000_000_000);
                        let in_frames = (clip.source_in * clip_fps.numerator as u64
                            + clip_fps.denominator as u64 * 500_000_000)
                            / (clip_fps.denominator as u64 * 1_000_000_000);
                        (tc_frames + in_frames) * clip_fps.denominator as u64 * 1_000_000_000
                            / clip_fps.numerator as u64
                    })
                    .unwrap_or(clip.source_in)
            };
            let start = ns_to_fcpxml_time(source_start_ns, clip_fps);

            let mut elem = BytesStart::new("asset-clip");
            elem.push_attribute(("ref", asset_ref.as_str()));
            if let Some(l) = lane {
                elem.push_attribute(("lane", l.to_string().as_str()));
            }
            elem.push_attribute(("offset", offset.as_str()));
            elem.push_attribute(("name", clip.label.as_str()));
            elem.push_attribute(("start", start.as_str()));
            elem.push_attribute(("duration", duration.as_str()));
            if !clip_is_audio_only {
                elem.push_attribute(("format", clip_format));
                elem.push_attribute(("tcFormat", "NDF"));
            } else {
                elem.push_attribute(("format", clip_format));
            }
            elem.push_attribute(("audioRole", "dialogue"));
            writer.write_event(Event::Start(elem))?;
            Ok(source_start_ns)
        };

        if let Some(primary_idx) = primary_track_idx {
            let primary_track = &project.tracks[primary_idx];
            for (clip_idx, clip) in primary_track.clips.iter().enumerate() {
                let source_start = write_asset_clip_start(&mut writer, clip, None, None)?;
                write_strict_clip_body(&mut writer, clip, project, source_start)?;

                // Nest connected clips whose offset falls within this primary clip's range.
                // A connected clip belongs to the last primary clip whose timeline_start
                // is <= the connected clip's timeline_start.
                for ct in &connected_tracks {
                    let conn_track = &project.tracks[ct.track_idx];
                    for conn_clip in &conn_track.clips {
                        let belongs_here = if clip_idx == 0 {
                            clip_idx + 1 >= primary_track.clips.len()
                                || conn_clip.timeline_start
                                    < primary_track.clips[clip_idx + 1].timeline_start
                        } else {
                            conn_clip.timeline_start >= clip.timeline_start
                                && (clip_idx + 1 >= primary_track.clips.len()
                                    || conn_clip.timeline_start
                                        < primary_track.clips[clip_idx + 1].timeline_start)
                        };
                        if belongs_here {
                            let conn_source_start = write_asset_clip_start(
                                &mut writer,
                                conn_clip,
                                Some(ct.lane),
                                Some(clip),
                            )?;
                            write_strict_clip_body(
                                &mut writer,
                                conn_clip,
                                project,
                                conn_source_start,
                            )?;
                            // audio-channel-source after connected clip's own anchors (none here)
                            write_strict_audio_channel_sources(
                                &mut writer,
                                conn_clip,
                                &project.frame_rate,
                                conn_source_start,
                            )?;
                            write_strict_filter_video_color(
                                &mut writer,
                                conn_clip,
                                COLOR_ADJUSTMENTS_EFFECT_ID,
                            )?;
                            writer.write_event(Event::End(BytesEnd::new("asset-clip")))?;
                        }
                    }
                }

                // audio-channel-source after connected clips (per DTD order)
                write_strict_audio_channel_sources(
                    &mut writer,
                    clip,
                    &project.frame_rate,
                    source_start,
                )?;
                // filter-video after audio-channel-source (per DTD order)
                write_strict_filter_video_color(&mut writer, clip, COLOR_ADJUSTMENTS_EFFECT_ID)?;

                writer.write_event(Event::End(BytesEnd::new("asset-clip")))?;

                // Transition between adjacent primary clips
                if clip_idx + 1 < primary_track.clips.len()
                    && !clip.transition_after.trim().is_empty()
                    && clip.transition_after_ns > 0
                {
                    let next_clip = &primary_track.clips[clip_idx + 1];
                    let clamped_duration_ns = clip
                        .transition_after_ns
                        .min(clip.duration())
                        .min(next_clip.duration());
                    if clamped_duration_ns > 0 {
                        let mut transition = BytesStart::new("transition");
                        if let Some(name) =
                            fcpxml_transition_name_for_kind(clip.transition_after.trim())
                        {
                            transition.push_attribute(("name", name));
                        }
                        transition.push_attribute((
                            "offset",
                            ns_to_fcpxml_time(
                                clip.timeline_start
                                    .saturating_add(clip.duration())
                                    .saturating_sub(clamped_duration_ns),
                                &project.frame_rate,
                            )
                            .as_str(),
                        ));
                        transition.push_attribute((
                            "duration",
                            ns_to_fcpxml_time(clamped_duration_ns, &project.frame_rate).as_str(),
                        ));
                        writer.write_event(Event::Empty(transition))?;
                    }
                }
            }
        } else {
            // No primary video track — emit connected clips flat as spine children.
            for ct in &connected_tracks {
                let track = &project.tracks[ct.track_idx];
                for clip in &track.clips {
                    let source_start =
                        write_asset_clip_start(&mut writer, clip, Some(ct.lane), None)?;
                    write_strict_clip_body(&mut writer, clip, project, source_start)?;
                    write_strict_audio_channel_sources(
                        &mut writer,
                        clip,
                        &project.frame_rate,
                        source_start,
                    )?;
                    write_strict_filter_video_color(
                        &mut writer,
                        clip,
                        COLOR_ADJUSTMENTS_EFFECT_ID,
                    )?;
                    writer.write_event(Event::End(BytesEnd::new("asset-clip")))?;
                }
            }
        }
    } else {
        // Non-strict (rich) mode: flat spine structure with us:* vendor attributes.
        let mut video_track_idx = 0usize;
        let mut audio_track_idx = 0usize;
        for (track_idx, track) in project.tracks.iter().enumerate() {
            let track_kind_idx = match track.kind {
                crate::model::track::TrackKind::Video => {
                    let idx = video_track_idx;
                    video_track_idx += 1;
                    idx
                }
                crate::model::track::TrackKind::Audio => {
                    let idx = audio_track_idx;
                    audio_track_idx += 1;
                    idx
                }
            };
            let track_kind = match track.kind {
                crate::model::track::TrackKind::Video => "video",
                crate::model::track::TrackKind::Audio => "audio",
            };
            for (clip_idx, clip) in track.clips.iter().enumerate() {
                let clip_source_key = clip
                    .fcpxml_original_source_path
                    .as_deref()
                    .unwrap_or(&clip.source_path);
                let asset_ref = asset_id_by_source
                    .get(clip_source_key)
                    .cloned()
                    .unwrap_or_else(|| format!("a_{}", sanitize_id(&clip.id)));
                let offset = ns_to_fcpxml_time(clip.timeline_start, &project.frame_rate);
                let duration = ns_to_fcpxml_time(clip.duration(), &project.frame_rate);
                let source_start_ns = clip.source_timecode_start_ns().unwrap_or(clip.source_in);
                let start = ns_to_fcpxml_time(source_start_ns, &project.frame_rate);

                let mut asset_clip = BytesStart::new("asset-clip");
                asset_clip.push_attribute(("ref", asset_ref.as_str()));
                asset_clip.push_attribute(("offset", offset.as_str()));
                asset_clip.push_attribute(("duration", duration.as_str()));
                asset_clip.push_attribute(("start", start.as_str()));
                asset_clip.push_attribute(("name", clip.label.as_str()));
                if emit_vendor_extensions {
                    // Multi-track routing
                    asset_clip.push_attribute(("us:track-idx", track_idx.to_string().as_str()));
                    asset_clip.push_attribute(("us:track-kind", track_kind));
                    asset_clip.push_attribute(("us:track-name", track.label.as_str()));
                    asset_clip.push_attribute(("us:track-muted", track.muted.to_string().as_str()));
                    asset_clip
                        .push_attribute(("us:track-locked", track.locked.to_string().as_str()));
                    asset_clip
                        .push_attribute(("us:track-soloed", track.soloed.to_string().as_str()));
                    if track.audio_role != crate::model::track::AudioRole::None {
                        asset_clip
                            .push_attribute(("us:track-audio-role", track.audio_role.as_str()));
                    }
                    if track.duck {
                        asset_clip.push_attribute(("us:track-duck", "true"));
                        asset_clip.push_attribute((
                            "us:track-duck-amount-db",
                            track.duck_amount_db.to_string().as_str(),
                        ));
                    }
                    asset_clip.push_attribute((
                        "us:track-height",
                        match track.height_preset {
                            crate::model::track::TrackHeightPreset::Small => "small",
                            crate::model::track::TrackHeightPreset::Medium => "medium",
                            crate::model::track::TrackHeightPreset::Large => "large",
                        },
                    ));
                    asset_clip.push_attribute((
                        "us:color-label",
                        match clip.color_label {
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
                    ));
                    if clip.blend_mode != crate::model::clip::BlendMode::Normal {
                        asset_clip.push_attribute((
                            "us:blend-mode",
                            match clip.blend_mode {
                                crate::model::clip::BlendMode::Normal => "normal",
                                crate::model::clip::BlendMode::Multiply => "multiply",
                                crate::model::clip::BlendMode::Screen => "screen",
                                crate::model::clip::BlendMode::Overlay => "overlay",
                                crate::model::clip::BlendMode::Add => "add",
                                crate::model::clip::BlendMode::Difference => "difference",
                                crate::model::clip::BlendMode::SoftLight => "soft_light",
                            },
                        ));
                    }
                    // Store color/effects as custom vendor attributes (us: prefix).
                    // Final Cut Pro ignores unknown attributes, so round-trip is lossless.
                    asset_clip
                        .push_attribute(("us:brightness", clip.brightness.to_string().as_str()));
                    asset_clip.push_attribute(("us:contrast", clip.contrast.to_string().as_str()));
                    asset_clip
                        .push_attribute(("us:saturation", clip.saturation.to_string().as_str()));
                    asset_clip
                        .push_attribute(("us:temperature", clip.temperature.to_string().as_str()));
                    asset_clip.push_attribute(("us:tint", clip.tint.to_string().as_str()));
                    let brightness_keyframes_json = if clip.brightness_keyframes.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&clip.brightness_keyframes).ok()
                    };
                    if let Some(value) = brightness_keyframes_json.as_deref() {
                        asset_clip.push_attribute(("us:brightness-keyframes", value));
                    }
                    let contrast_keyframes_json = if clip.contrast_keyframes.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&clip.contrast_keyframes).ok()
                    };
                    if let Some(value) = contrast_keyframes_json.as_deref() {
                        asset_clip.push_attribute(("us:contrast-keyframes", value));
                    }
                    let saturation_keyframes_json = if clip.saturation_keyframes.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&clip.saturation_keyframes).ok()
                    };
                    if let Some(value) = saturation_keyframes_json.as_deref() {
                        asset_clip.push_attribute(("us:saturation-keyframes", value));
                    }
                    let temperature_keyframes_json = if clip.temperature_keyframes.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&clip.temperature_keyframes).ok()
                    };
                    if let Some(value) = temperature_keyframes_json.as_deref() {
                        asset_clip.push_attribute(("us:temperature-keyframes", value));
                    }
                    let tint_keyframes_json = if clip.tint_keyframes.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&clip.tint_keyframes).ok()
                    };
                    if let Some(value) = tint_keyframes_json.as_deref() {
                        asset_clip.push_attribute(("us:tint-keyframes", value));
                    }
                    asset_clip.push_attribute(("us:denoise", clip.denoise.to_string().as_str()));
                    asset_clip
                        .push_attribute(("us:sharpness", clip.sharpness.to_string().as_str()));
                    asset_clip.push_attribute(("us:blur", clip.blur.to_string().as_str()));
                    if !clip.blur_keyframes.is_empty() {
                        if let Ok(json) = serde_json::to_string(&clip.blur_keyframes) {
                            asset_clip.push_attribute(("us:blur-keyframes", json.as_str()));
                        }
                    }
                    if clip.vidstab_enabled {
                        asset_clip.push_attribute(("us:vidstab-enabled", "true"));
                        asset_clip.push_attribute((
                            "us:vidstab-smoothing",
                            clip.vidstab_smoothing.to_string().as_str(),
                        ));
                    }
                    if !clip.frei0r_effects.is_empty() {
                        if let Ok(json) = serde_json::to_string(&clip.frei0r_effects) {
                            asset_clip.push_attribute(("us:frei0r-effects", json.as_str()));
                        }
                    }
                    if !clip.masks.is_empty() {
                        if let Ok(json) = serde_json::to_string(&clip.masks) {
                            asset_clip.push_attribute(("us:masks", json.as_str()));
                        }
                    }
                    // Subtitle segments + style
                    if !clip.subtitle_segments.is_empty() {
                        if let Ok(json) = serde_json::to_string(&clip.subtitle_segments) {
                            asset_clip.push_attribute(("us:subtitle-segments", json.as_str()));
                        }
                    }
                    if !clip.subtitles_language.is_empty() {
                        asset_clip.push_attribute((
                            "us:subtitles-language",
                            clip.subtitles_language.as_str(),
                        ));
                    }
                    if clip.subtitle_font != "Sans Bold 24" {
                        asset_clip
                            .push_attribute(("us:subtitle-font", clip.subtitle_font.as_str()));
                    }
                    if clip.subtitle_color != 0xFFFFFFFF {
                        asset_clip.push_attribute((
                            "us:subtitle-color",
                            clip.subtitle_color.to_string().as_str(),
                        ));
                    }
                    if clip.subtitle_outline_color != 0x000000FF {
                        asset_clip.push_attribute((
                            "us:subtitle-outline-color",
                            clip.subtitle_outline_color.to_string().as_str(),
                        ));
                    }
                    if (clip.subtitle_outline_width - 2.0).abs() > 0.001 {
                        asset_clip.push_attribute((
                            "us:subtitle-outline-width",
                            clip.subtitle_outline_width.to_string().as_str(),
                        ));
                    }
                    if !clip.subtitle_bg_box {
                        asset_clip.push_attribute(("us:subtitle-bg-box", "false"));
                    }
                    if clip.subtitle_bg_box_color != 0x00000099 {
                        asset_clip.push_attribute((
                            "us:subtitle-bg-box-color",
                            clip.subtitle_bg_box_color.to_string().as_str(),
                        ));
                    }
                    if clip.subtitle_highlight_mode
                        != crate::model::clip::SubtitleHighlightMode::None
                    {
                        let mode_str = match clip.subtitle_highlight_mode {
                            crate::model::clip::SubtitleHighlightMode::Bold => "bold",
                            crate::model::clip::SubtitleHighlightMode::Color => "color",
                            crate::model::clip::SubtitleHighlightMode::Underline => "underline",
                            crate::model::clip::SubtitleHighlightMode::Stroke => "stroke",
                            _ => "none",
                        };
                        asset_clip.push_attribute(("us:subtitle-highlight-mode", mode_str));
                    }
                    if clip.subtitle_highlight_color != 0xFFFF00FF {
                        asset_clip.push_attribute((
                            "us:subtitle-highlight-color",
                            clip.subtitle_highlight_color.to_string().as_str(),
                        ));
                    }
                    if (clip.subtitle_word_window_secs - 2.0).abs() > 0.001 {
                        asset_clip.push_attribute((
                            "us:subtitle-word-window-secs",
                            clip.subtitle_word_window_secs.to_string().as_str(),
                        ));
                    }
                    if (clip.subtitle_position_y - 0.85).abs() > 0.001 {
                        asset_clip.push_attribute((
                            "us:subtitle-position-y",
                            clip.subtitle_position_y.to_string().as_str(),
                        ));
                    }
                    if !clip.ladspa_effects.is_empty() {
                        if let Ok(json) = serde_json::to_string(&clip.ladspa_effects) {
                            asset_clip.push_attribute(("us:ladspa-effects", json.as_str()));
                        }
                    }
                    asset_clip.push_attribute(("us:volume", clip.volume.to_string().as_str()));
                    let volume_keyframes_json = if clip.volume_keyframes.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&clip.volume_keyframes).ok()
                    };
                    if let Some(value) = volume_keyframes_json.as_deref() {
                        asset_clip.push_attribute(("us:volume-keyframes", value));
                    }
                    asset_clip.push_attribute(("us:pan", clip.pan.to_string().as_str()));
                    let pan_keyframes_json = if clip.pan_keyframes.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&clip.pan_keyframes).ok()
                    };
                    if let Some(value) = pan_keyframes_json.as_deref() {
                        asset_clip.push_attribute(("us:pan-keyframes", value));
                    }
                    // EQ bands — only emit when non-default.
                    if clip.has_eq()
                        || clip.eq_bands.iter().any(|b| {
                            (b.freq - 200.0).abs() > 0.01
                                || (b.freq - 1000.0).abs() > 0.01
                                || (b.freq - 5000.0).abs() > 0.01
                                || b.q != 1.0
                        })
                    {
                        if let Ok(json) = serde_json::to_string(&clip.eq_bands) {
                            asset_clip.push_attribute(("us:eq-bands", json.as_str()));
                        }
                    }
                    if !clip.eq_low_gain_keyframes.is_empty() {
                        if let Ok(json) = serde_json::to_string(&clip.eq_low_gain_keyframes) {
                            asset_clip.push_attribute(("us:eq-low-gain-keyframes", json.as_str()));
                        }
                    }
                    if !clip.eq_mid_gain_keyframes.is_empty() {
                        if let Ok(json) = serde_json::to_string(&clip.eq_mid_gain_keyframes) {
                            asset_clip.push_attribute(("us:eq-mid-gain-keyframes", json.as_str()));
                        }
                    }
                    if !clip.eq_high_gain_keyframes.is_empty() {
                        if let Ok(json) = serde_json::to_string(&clip.eq_high_gain_keyframes) {
                            asset_clip.push_attribute(("us:eq-high-gain-keyframes", json.as_str()));
                        }
                    }
                    if clip.pitch_shift_semitones.abs() > 0.001 {
                        asset_clip.push_attribute((
                            "us:pitch-shift-semitones",
                            clip.pitch_shift_semitones.to_string().as_str(),
                        ));
                    }
                    if clip.pitch_preserve {
                        asset_clip.push_attribute(("us:pitch-preserve", "true"));
                    }
                    if clip.audio_channel_mode != crate::model::clip::AudioChannelMode::Stereo {
                        asset_clip.push_attribute((
                            "us:audio-channel-mode",
                            clip.audio_channel_mode.as_str(),
                        ));
                    }
                    if let Some(lufs) = clip.measured_loudness_lufs {
                        asset_clip.push_attribute((
                            "us:measured-loudness-lufs",
                            format!("{lufs:.2}").as_str(),
                        ));
                    }
                    let rotate_keyframes_json = if clip.rotate_keyframes.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&clip.rotate_keyframes).ok()
                    };
                    if let Some(value) = rotate_keyframes_json.as_deref() {
                        asset_clip.push_attribute(("us:rotate-keyframes", value));
                    }
                    asset_clip
                        .push_attribute(("us:crop-left", clip.crop_left.to_string().as_str()));
                    asset_clip
                        .push_attribute(("us:crop-right", clip.crop_right.to_string().as_str()));
                    asset_clip.push_attribute(("us:crop-top", clip.crop_top.to_string().as_str()));
                    asset_clip
                        .push_attribute(("us:crop-bottom", clip.crop_bottom.to_string().as_str()));
                    let crop_left_keyframes_json = if clip.crop_left_keyframes.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&clip.crop_left_keyframes).ok()
                    };
                    if let Some(value) = crop_left_keyframes_json.as_deref() {
                        asset_clip.push_attribute(("us:crop-left-keyframes", value));
                    }
                    let crop_right_keyframes_json = if clip.crop_right_keyframes.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&clip.crop_right_keyframes).ok()
                    };
                    if let Some(value) = crop_right_keyframes_json.as_deref() {
                        asset_clip.push_attribute(("us:crop-right-keyframes", value));
                    }
                    let crop_top_keyframes_json = if clip.crop_top_keyframes.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&clip.crop_top_keyframes).ok()
                    };
                    if let Some(value) = crop_top_keyframes_json.as_deref() {
                        asset_clip.push_attribute(("us:crop-top-keyframes", value));
                    }
                    let crop_bottom_keyframes_json = if clip.crop_bottom_keyframes.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&clip.crop_bottom_keyframes).ok()
                    };
                    if let Some(value) = crop_bottom_keyframes_json.as_deref() {
                        asset_clip.push_attribute(("us:crop-bottom-keyframes", value));
                    }
                    asset_clip.push_attribute(("us:rotate", clip.rotate.to_string().as_str()));
                    asset_clip.push_attribute(("us:flip-h", clip.flip_h.to_string().as_str()));
                    asset_clip.push_attribute(("us:flip-v", clip.flip_v.to_string().as_str()));
                    if (clip.anamorphic_desqueeze - 1.0).abs() > 0.001 {
                        asset_clip.push_attribute((
                            "us:anamorphic-desqueeze",
                            clip.anamorphic_desqueeze.to_string().as_str(),
                        ));
                    }
                    asset_clip.push_attribute(("us:scale", clip.scale.to_string().as_str()));
                    let scale_keyframes_json = if clip.scale_keyframes.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&clip.scale_keyframes).ok()
                    };
                    if let Some(value) = scale_keyframes_json.as_deref() {
                        asset_clip.push_attribute(("us:scale-keyframes", value));
                    }
                    asset_clip.push_attribute(("us:opacity", clip.opacity.to_string().as_str()));
                    let opacity_keyframes_json = if clip.opacity_keyframes.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&clip.opacity_keyframes).ok()
                    };
                    if let Some(value) = opacity_keyframes_json.as_deref() {
                        asset_clip.push_attribute(("us:opacity-keyframes", value));
                    }
                    asset_clip
                        .push_attribute(("us:position-x", clip.position_x.to_string().as_str()));
                    let position_x_keyframes_json = if clip.position_x_keyframes.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&clip.position_x_keyframes).ok()
                    };
                    if let Some(value) = position_x_keyframes_json.as_deref() {
                        asset_clip.push_attribute(("us:position-x-keyframes", value));
                    }
                    asset_clip
                        .push_attribute(("us:position-y", clip.position_y.to_string().as_str()));
                    let position_y_keyframes_json = if clip.position_y_keyframes.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&clip.position_y_keyframes).ok()
                    };
                    if let Some(value) = position_y_keyframes_json.as_deref() {
                        asset_clip.push_attribute(("us:position-y-keyframes", value));
                    }
                    asset_clip.push_attribute(("us:title-text", clip.title_text.as_str()));
                    asset_clip.push_attribute(("us:title-font", clip.title_font.as_str()));
                    asset_clip.push_attribute((
                        "us:title-color",
                        format!("{:08X}", clip.title_color).as_str(),
                    ));
                    asset_clip.push_attribute(("us:title-x", clip.title_x.to_string().as_str()));
                    asset_clip.push_attribute(("us:title-y", clip.title_y.to_string().as_str()));
                    if !clip.title_template.is_empty() {
                        asset_clip
                            .push_attribute(("us:title-template", clip.title_template.as_str()));
                    }
                    if clip.title_outline_width > 0.0 {
                        asset_clip.push_attribute((
                            "us:title-outline-color",
                            format!("{:08X}", clip.title_outline_color).as_str(),
                        ));
                        asset_clip.push_attribute((
                            "us:title-outline-width",
                            clip.title_outline_width.to_string().as_str(),
                        ));
                    }
                    if clip.title_shadow {
                        asset_clip.push_attribute(("us:title-shadow", "true"));
                        asset_clip.push_attribute((
                            "us:title-shadow-color",
                            format!("{:08X}", clip.title_shadow_color).as_str(),
                        ));
                        asset_clip.push_attribute((
                            "us:title-shadow-offset-x",
                            clip.title_shadow_offset_x.to_string().as_str(),
                        ));
                        asset_clip.push_attribute((
                            "us:title-shadow-offset-y",
                            clip.title_shadow_offset_y.to_string().as_str(),
                        ));
                    }
                    if clip.title_bg_box {
                        asset_clip.push_attribute(("us:title-bg-box", "true"));
                        asset_clip.push_attribute((
                            "us:title-bg-box-color",
                            format!("{:08X}", clip.title_bg_box_color).as_str(),
                        ));
                        asset_clip.push_attribute((
                            "us:title-bg-box-padding",
                            clip.title_bg_box_padding.to_string().as_str(),
                        ));
                    }
                    if clip.title_clip_bg_color != 0 {
                        asset_clip.push_attribute((
                            "us:title-clip-bg-color",
                            format!("{:08X}", clip.title_clip_bg_color).as_str(),
                        ));
                    }
                    if !clip.title_secondary_text.is_empty() {
                        asset_clip.push_attribute((
                            "us:title-secondary-text",
                            clip.title_secondary_text.as_str(),
                        ));
                    }
                    if clip.kind == crate::model::clip::ClipKind::Title {
                        asset_clip.push_attribute(("us:clip-kind", "title"));
                    } else if clip.kind == crate::model::clip::ClipKind::Adjustment {
                        asset_clip.push_attribute(("us:clip-kind", "adjustment"));
                    } else if clip.kind == crate::model::clip::ClipKind::Compound {
                        asset_clip.push_attribute(("us:clip-kind", "compound"));
                        if let Some(ref tracks) = clip.compound_tracks {
                            if let Ok(json) = serde_json::to_string(tracks) {
                                let escaped = json.replace('"', "&quot;");
                                asset_clip.push_attribute(("us:compound-tracks", escaped.as_str()));
                            }
                        }
                    } else if clip.kind == crate::model::clip::ClipKind::Multicam {
                        asset_clip.push_attribute(("us:clip-kind", "multicam"));
                        if let Some(ref angles) = clip.multicam_angles {
                            if let Ok(json) = serde_json::to_string(angles) {
                                let escaped = json.replace('"', "&quot;");
                                asset_clip.push_attribute(("us:multicam-angles", escaped.as_str()));
                            }
                        }
                        if let Some(ref switches) = clip.multicam_switches {
                            if let Ok(json) = serde_json::to_string(switches) {
                                let escaped = json.replace('"', "&quot;");
                                asset_clip
                                    .push_attribute(("us:multicam-switches", escaped.as_str()));
                            }
                        }
                    }
                    asset_clip.push_attribute(("us:speed", clip.speed.to_string().as_str()));
                    let speed_keyframes_json = if clip.speed_keyframes.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&clip.speed_keyframes).ok()
                    };
                    if let Some(value) = speed_keyframes_json.as_deref() {
                        asset_clip.push_attribute(("us:speed-keyframes", value));
                    }
                    asset_clip.push_attribute(("us:reverse", clip.reverse.to_string().as_str()));
                    if clip.slow_motion_interp != crate::model::clip::SlowMotionInterp::Off {
                        let val = match clip.slow_motion_interp {
                            crate::model::clip::SlowMotionInterp::Blend => "blend",
                            crate::model::clip::SlowMotionInterp::OpticalFlow => "optical-flow",
                            crate::model::clip::SlowMotionInterp::Off => unreachable!(),
                        };
                        asset_clip.push_attribute(("us:slow-motion-interp", val));
                    }
                    if clip.freeze_frame {
                        asset_clip.push_attribute(("us:freeze-frame", "true"));
                    }
                    if let Some(freeze_source_ns) = clip.freeze_frame_source_ns {
                        asset_clip.push_attribute((
                            "us:freeze-source-ns",
                            freeze_source_ns.to_string().as_str(),
                        ));
                    }
                    if let Some(freeze_hold_duration_ns) = clip.freeze_frame_hold_duration_ns {
                        asset_clip.push_attribute((
                            "us:freeze-hold-duration-ns",
                            freeze_hold_duration_ns.to_string().as_str(),
                        ));
                    }
                    if let Some(ref gid) = clip.group_id {
                        if !gid.is_empty() {
                            asset_clip.push_attribute(("us:group-id", gid.as_str()));
                        }
                    }
                    if let Some(ref link_gid) = clip.link_group_id {
                        if !link_gid.is_empty() {
                            asset_clip.push_attribute(("us:link-group-id", link_gid.as_str()));
                        }
                    }
                    if let Some(source_timecode_base_ns) = clip.source_timecode_base_ns {
                        asset_clip.push_attribute((
                            "us:source-timecode-base-ns",
                            source_timecode_base_ns.to_string().as_str(),
                        ));
                    }
                    asset_clip.push_attribute(("us:shadows", clip.shadows.to_string().as_str()));
                    asset_clip.push_attribute(("us:midtones", clip.midtones.to_string().as_str()));
                    asset_clip
                        .push_attribute(("us:highlights", clip.highlights.to_string().as_str()));
                    asset_clip.push_attribute(("us:exposure", clip.exposure.to_string().as_str()));
                    asset_clip
                        .push_attribute(("us:black-point", clip.black_point.to_string().as_str()));
                    asset_clip.push_attribute((
                        "us:highlights-warmth",
                        clip.highlights_warmth.to_string().as_str(),
                    ));
                    asset_clip.push_attribute((
                        "us:highlights-tint",
                        clip.highlights_tint.to_string().as_str(),
                    ));
                    asset_clip.push_attribute((
                        "us:midtones-warmth",
                        clip.midtones_warmth.to_string().as_str(),
                    ));
                    asset_clip.push_attribute((
                        "us:midtones-tint",
                        clip.midtones_tint.to_string().as_str(),
                    ));
                    asset_clip.push_attribute((
                        "us:shadows-warmth",
                        clip.shadows_warmth.to_string().as_str(),
                    ));
                    asset_clip.push_attribute((
                        "us:shadows-tint",
                        clip.shadows_tint.to_string().as_str(),
                    ));
                    if clip.chroma_key_enabled {
                        asset_clip.push_attribute(("us:chroma-key-enabled", "true"));
                        asset_clip.push_attribute((
                            "us:chroma-key-color",
                            format!("{:#08X}", clip.chroma_key_color).as_str(),
                        ));
                        asset_clip.push_attribute((
                            "us:chroma-key-tolerance",
                            clip.chroma_key_tolerance.to_string().as_str(),
                        ));
                        asset_clip.push_attribute((
                            "us:chroma-key-softness",
                            clip.chroma_key_softness.to_string().as_str(),
                        ));
                    }
                    if clip.bg_removal_enabled {
                        asset_clip.push_attribute(("us:bg-removal-enabled", "true"));
                        asset_clip.push_attribute((
                            "us:bg-removal-threshold",
                            clip.bg_removal_threshold.to_string().as_str(),
                        ));
                    }
                    if !clip.lut_paths.is_empty() {
                        let lut_json = serde_json::to_string(&clip.lut_paths).unwrap_or_default();
                        asset_clip.push_attribute(("us:lut-paths", lut_json.as_str()));
                        // Also write first LUT as us:lut-path for backward compat
                        asset_clip.push_attribute(("us:lut-path", clip.lut_paths[0].as_str()));
                    }
                    if !clip.transition_after.is_empty() {
                        asset_clip.push_attribute((
                            "us:transition-after",
                            clip.transition_after.as_str(),
                        ));
                        asset_clip.push_attribute((
                            "us:transition-after-ns",
                            clip.transition_after_ns.to_string().as_str(),
                        ));
                    }
                }
                if !strip_unknown_fields {
                    for (k, v) in &clip.fcpxml_unknown_attrs {
                        if !is_writer_managed_asset_clip_attr(k) {
                            asset_clip.push_attribute((k.as_str(), v.as_str()));
                        }
                    }
                }
                writer.write_event(Event::Start(asset_clip))?;
                if let Some(fragment) = preserved_unknown_time_map_fragment(clip) {
                    writer.get_mut().write_all(fragment.as_bytes())?;
                } else {
                    write_native_time_map(&mut writer, clip, &project.frame_rate)?;
                }

                let (position_x, position_y) = internal_position_to_fcpxml(
                    clip.position_x,
                    clip.position_y,
                    project.width,
                    project.height,
                    clip.scale,
                );
                {
                    let mut adjust_transform = BytesStart::new("adjust-transform");
                    let has_position_kfs = !clip.position_x_keyframes.is_empty()
                        || !clip.position_y_keyframes.is_empty();
                    let has_scale_kfs = !clip.scale_keyframes.is_empty();
                    let has_rotation_kfs = !clip.rotate_keyframes.is_empty();
                    let has_transform_kfs = has_position_kfs || has_scale_kfs || has_rotation_kfs;
                    // FCP omits inline attrs for properties that have keyframes
                    if !has_position_kfs {
                        adjust_transform.push_attribute((
                            "position",
                            format!("{} {}", position_x, position_y).as_str(),
                        ));
                    }
                    if !has_scale_kfs {
                        adjust_transform.push_attribute((
                            "scale",
                            format!("{} {}", clip.scale, clip.scale).as_str(),
                        ));
                    }
                    if !has_rotation_kfs {
                        adjust_transform
                            .push_attribute(("rotation", clip.rotate.to_string().as_str()));
                    }
                    if has_transform_kfs {
                        writer.write_event(Event::Start(adjust_transform))?;
                        write_transform_keyframe_params(
                            &mut writer,
                            clip,
                            project,
                            source_start_ns,
                        )?;
                        writer.write_event(Event::End(BytesEnd::new("adjust-transform")))?;
                    } else {
                        writer.write_event(Event::Empty(adjust_transform))?;
                    }

                    let mut adjust_compositing = BytesStart::new("adjust-compositing");
                    adjust_compositing
                        .push_attribute(("opacity", clip.opacity.to_string().as_str()));
                    if !clip.opacity_keyframes.is_empty() {
                        writer.write_event(Event::Start(adjust_compositing))?;
                        write_opacity_keyframe_params(
                            &mut writer,
                            clip,
                            &project.frame_rate,
                            source_start_ns,
                        )?;
                        writer.write_event(Event::End(BytesEnd::new("adjust-compositing")))?;
                    } else {
                        writer.write_event(Event::Empty(adjust_compositing))?;
                    }

                    let mut adjust_volume = BytesStart::new("adjust-volume");
                    adjust_volume.push_attribute((
                        "amount",
                        linear_volume_to_fcpxml_db(clip.volume as f64).as_str(),
                    ));
                    if !clip.volume_keyframes.is_empty() {
                        writer.write_event(Event::Start(adjust_volume))?;
                        write_volume_keyframe_params(
                            &mut writer,
                            clip,
                            &project.frame_rate,
                            source_start_ns,
                            false,
                        )?;
                        writer.write_event(Event::End(BytesEnd::new("adjust-volume")))?;
                    } else {
                        writer.write_event(Event::Empty(adjust_volume))?;
                    }

                    let mut adjust_panner = BytesStart::new("adjust-panner");
                    adjust_panner.push_attribute((
                        "amount",
                        format!("{:.6}", clip.pan.clamp(-1.0, 1.0)).as_str(),
                    ));
                    if !clip.pan_keyframes.is_empty() {
                        writer.write_event(Event::Start(adjust_panner))?;
                        write_pan_keyframe_params(
                            &mut writer,
                            clip,
                            &project.frame_rate,
                            source_start_ns,
                            false,
                        )?;
                        writer.write_event(Event::End(BytesEnd::new("adjust-panner")))?;
                    } else {
                        writer.write_event(Event::Empty(adjust_panner))?;
                    }

                    let mut adjust_crop = BytesStart::new("adjust-crop");
                    adjust_crop.push_attribute(("left", clip.crop_left.to_string().as_str()));
                    adjust_crop.push_attribute(("right", clip.crop_right.to_string().as_str()));
                    adjust_crop.push_attribute(("top", clip.crop_top.to_string().as_str()));
                    adjust_crop.push_attribute(("bottom", clip.crop_bottom.to_string().as_str()));
                    writer.write_event(Event::Empty(adjust_crop))?;
                }
                if !strip_unknown_fields {
                    for fragment in &clip.fcpxml_unknown_children {
                        if is_time_map_fragment(fragment) {
                            continue;
                        }
                        writer.get_mut().write_all(fragment.as_bytes())?;
                    }
                }

                writer.write_event(Event::End(BytesEnd::new("asset-clip")))?;

                if clip_idx + 1 < track.clips.len()
                    && !clip.transition_after.trim().is_empty()
                    && clip.transition_after_ns > 0
                {
                    let next_clip = &track.clips[clip_idx + 1];
                    let clamped_duration_ns = clip
                        .transition_after_ns
                        .min(clip.duration())
                        .min(next_clip.duration());
                    if clamped_duration_ns > 0 {
                        let mut transition = BytesStart::new("transition");
                        if let Some(name) =
                            fcpxml_transition_name_for_kind(clip.transition_after.trim())
                        {
                            transition.push_attribute(("name", name));
                        }
                        transition.push_attribute((
                            "offset",
                            ns_to_fcpxml_time(
                                clip.timeline_start
                                    .saturating_add(clip.duration())
                                    .saturating_sub(clamped_duration_ns),
                                &project.frame_rate,
                            )
                            .as_str(),
                        ));
                        transition.push_attribute((
                            "duration",
                            ns_to_fcpxml_time(clamped_duration_ns, &project.frame_rate).as_str(),
                        ));
                        writer.write_event(Event::Empty(transition))?;
                    }
                }
            }
        }
    } // end non-strict else

    if !strip_unknown_fields {
        for fragment in &project.fcpxml_unknown_spine.children {
            writer.get_mut().write_all(fragment.as_bytes())?;
        }
    }
    writer.write_event(Event::End(BytesEnd::new("spine")))?;

    if !options.strict_dtd {
        // Rich mode compatibility: write timeline markers as <marker> inside <sequence>.
        // Strict mode omits these because sequence-level markers are outside DTD sequence content.
        for marker in &project.markers {
            let mut m = BytesStart::new("marker");
            m.push_attribute((
                "start",
                ns_to_fcpxml_time(marker.position_ns, &project.frame_rate).as_str(),
            ));
            m.push_attribute(("duration", "1/24s"));
            m.push_attribute(("value", marker.label.as_str()));
            if emit_vendor_extensions {
                m.push_attribute(("us:color", format!("{:08X}", marker.color).as_str()));
            }
            writer.write_event(Event::Empty(m))?;
        }
    }
    if !strip_unknown_fields {
        for fragment in &project.fcpxml_unknown_sequence.children {
            writer.get_mut().write_all(fragment.as_bytes())?;
        }
    }

    writer.write_event(Event::End(BytesEnd::new("sequence")))?;
    if !strip_unknown_fields {
        for fragment in &project.fcpxml_unknown_project.children {
            writer.get_mut().write_all(fragment.as_bytes())?;
        }
    }
    writer.write_event(Event::End(BytesEnd::new("project")))?;
    if !strip_unknown_fields {
        for fragment in &project.fcpxml_unknown_event.children {
            writer.get_mut().write_all(fragment.as_bytes())?;
        }
    }
    writer.write_event(Event::End(BytesEnd::new("event")))?;
    if !strip_unknown_fields {
        for fragment in &project.fcpxml_unknown_library.children {
            writer.get_mut().write_all(fragment.as_bytes())?;
        }
    }
    writer.write_event(Event::End(BytesEnd::new("library")))?;
    if !strip_unknown_fields {
        for fragment in &project.fcpxml_unknown_root.children {
            writer.get_mut().write_all(fragment.as_bytes())?;
        }
    }
    writer.write_event(Event::End(BytesEnd::new("fcpxml")))?;

    let result = writer.into_inner().into_inner();
    Ok(String::from_utf8(result)?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectFilesMode {
    TimelineUsedOnly,
    EntireLibrary,
}

impl CollectFilesMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TimelineUsedOnly => "timeline_used",
            Self::EntireLibrary => "entire_library",
        }
    }

    pub fn ui_label(self) -> &'static str {
        match self {
            Self::TimelineUsedOnly => "Timeline-used only",
            Self::EntireLibrary => "Entire library",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "timeline_used" | "timeline_used_only" => Some(Self::TimelineUsedOnly),
            "entire_library" => Some(Self::EntireLibrary),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollectFilesProgress {
    Copying {
        copied_files: usize,
        total_files: usize,
        current_file: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectFilesResult {
    pub destination_dir: PathBuf,
    pub media_files_copied: usize,
    pub lut_files_copied: usize,
}

impl CollectFilesResult {
    pub fn total_files_copied(&self) -> usize {
        self.media_files_copied + self.lut_files_copied
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportProjectWithMediaProgress {
    Copying {
        copied_files: usize,
        total_files: usize,
        current_file: String,
    },
    WritingProjectXml,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectFilesManifest {
    pub result: CollectFilesResult,
    pub source_to_destination_path: HashMap<String, PathBuf>,
    pub lut_source_to_destination_path: HashMap<String, PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyCollectedFilesResult {
    pub project_media_references_updated: usize,
    pub project_lut_references_updated: usize,
    pub library_items_updated: usize,
}

impl ApplyCollectedFilesResult {
    pub fn updated_any(&self) -> bool {
        self.project_media_references_updated > 0
            || self.project_lut_references_updated > 0
            || self.library_items_updated > 0
    }
}

/// Copy referenced project media into a destination directory without writing project XML.
pub fn collect_files(
    project: &Project,
    library: &[MediaItem],
    destination_dir: &Path,
    mode: CollectFilesMode,
) -> Result<CollectFilesResult> {
    collect_files_with_progress(project, library, destination_dir, mode, |_| {})
}

pub fn collect_files_with_manifest<F>(
    project: &Project,
    library: &[MediaItem],
    destination_dir: &Path,
    mode: CollectFilesMode,
    on_progress: F,
) -> Result<CollectFilesManifest>
where
    F: FnMut(CollectFilesProgress),
{
    collect_files_internal(project, library, destination_dir, mode, true, on_progress)
}

pub fn collect_files_with_progress<F>(
    project: &Project,
    library: &[MediaItem],
    destination_dir: &Path,
    mode: CollectFilesMode,
    on_progress: F,
) -> Result<CollectFilesResult>
where
    F: FnMut(CollectFilesProgress),
{
    Ok(collect_files_with_manifest(project, library, destination_dir, mode, on_progress)?.result)
}

pub fn apply_collected_files_manifest(
    project: &mut Project,
    library: &mut [MediaItem],
    manifest: &CollectFilesManifest,
) -> ApplyCollectedFilesResult {
    let (project_media_references_updated, project_lut_references_updated) =
        apply_collected_files_to_tracks(
            project.tracks.as_mut_slice(),
            &manifest.source_to_destination_path,
            &manifest.lut_source_to_destination_path,
        );
    let mut library_items_updated = 0usize;
    for item in library.iter_mut() {
        let Some(new_path) =
            remapped_collect_path(&item.source_path, &manifest.source_to_destination_path)
        else {
            continue;
        };
        if item.source_path != new_path {
            item.source_path = new_path;
            library_items_updated += 1;
        }
    }
    if project_media_references_updated > 0
        || project_lut_references_updated > 0
        || library_items_updated > 0
    {
        project.dirty = true;
    }
    ApplyCollectedFilesResult {
        project_media_references_updated,
        project_lut_references_updated,
        library_items_updated,
    }
}

fn apply_collected_files_to_tracks(
    tracks: &mut [crate::model::track::Track],
    source_to_destination_path: &HashMap<String, PathBuf>,
    lut_source_to_destination_path: &HashMap<String, PathBuf>,
) -> (usize, usize) {
    let mut project_media_references_updated = 0usize;
    let mut project_lut_references_updated = 0usize;
    for track in tracks {
        for clip in track.clips.iter_mut() {
            if let Some(new_path) =
                remapped_collect_path(&clip.source_path, source_to_destination_path)
            {
                if clip.source_path != new_path {
                    clip.source_path = new_path;
                    clip.fcpxml_original_source_path = None;
                    project_media_references_updated += 1;
                }
            }
            for lut_path in &mut clip.lut_paths {
                let Some(new_path) =
                    remapped_collect_path(lut_path, lut_source_to_destination_path)
                else {
                    continue;
                };
                if *lut_path != new_path {
                    *lut_path = new_path;
                    project_lut_references_updated += 1;
                }
            }
            if let Some(angles) = clip.multicam_angles.as_mut() {
                for angle in angles {
                    if let Some(new_path) =
                        remapped_collect_path(&angle.source_path, source_to_destination_path)
                    {
                        if angle.source_path != new_path {
                            angle.source_path = new_path;
                            project_media_references_updated += 1;
                        }
                    }
                }
            }
            if let Some(compound_tracks) = clip.compound_tracks.as_mut() {
                let (nested_media, nested_luts) = apply_collected_files_to_tracks(
                    compound_tracks.as_mut_slice(),
                    source_to_destination_path,
                    lut_source_to_destination_path,
                );
                project_media_references_updated += nested_media;
                project_lut_references_updated += nested_luts;
            }
        }
    }
    (
        project_media_references_updated,
        project_lut_references_updated,
    )
}

fn remapped_collect_path(path: &str, path_map: &HashMap<String, PathBuf>) -> Option<String> {
    path_map
        .get(path)
        .map(|mapped| mapped.to_string_lossy().to_string())
}

/// Export a packaged project: write `.uspxml` and copy referenced timeline media
/// into a sibling `ProjectName.Library` directory, then rewrite XML paths to use
/// that copied media.
pub fn export_project_with_media(project: &Project, output_fcpxml_path: &Path) -> Result<PathBuf> {
    export_project_with_media_with_progress(project, output_fcpxml_path, |_| {})
}

pub fn export_project_with_media_with_progress<F>(
    project: &Project,
    output_fcpxml_path: &Path,
    mut on_progress: F,
) -> Result<PathBuf>
where
    F: FnMut(ExportProjectWithMediaProgress),
{
    let output_fcpxml_path = if output_fcpxml_path.is_absolute() {
        output_fcpxml_path.to_path_buf()
    } else {
        std::env::current_dir()?.join(output_fcpxml_path)
    };
    let parent = output_fcpxml_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Export path has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let stem = output_fcpxml_path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Project");
    let library_dir = parent.join(format!("{stem}.Library"));
    let collected = collect_files_internal(
        project,
        &[],
        &library_dir,
        CollectFilesMode::TimelineUsedOnly,
        false,
        |progress| match progress {
            CollectFilesProgress::Copying {
                copied_files,
                total_files,
                current_file,
            } => on_progress(ExportProjectWithMediaProgress::Copying {
                copied_files,
                total_files,
                current_file,
            }),
        },
    )?;

    let mut export_project = project.clone();
    export_project.source_fcpxml = None;
    export_project.file_path = None;
    export_project.dirty = true;
    for clip in export_project
        .tracks
        .iter_mut()
        .flat_map(|track| track.clips.iter_mut())
    {
        if clip.source_path.is_empty() {
            clip.fcpxml_original_source_path = None;
            continue;
        }
        let mapped = collected
            .source_to_destination_path
            .get(&clip.source_path)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Missing packaged media mapping for source: {}",
                    clip.source_path
                )
            })?;
        let portable_path = normalize_packaged_path_for_portability(mapped);
        clip.source_path = portable_path.to_string_lossy().to_string();
        clip.fcpxml_original_source_path = None;
    }

    for clip in export_project
        .tracks
        .iter_mut()
        .flat_map(|track| track.clips.iter_mut())
    {
        let mut rewritten: Vec<String> = Vec::new();
        for lut_path in &clip.lut_paths {
            let Some(mapped) = collected.lut_source_to_destination_path.get(lut_path) else {
                continue;
            };
            let portable_path = normalize_packaged_path_for_portability(mapped);
            rewritten.push(portable_path.to_string_lossy().to_string());
        }
        clip.lut_paths = rewritten;
    }

    on_progress(ExportProjectWithMediaProgress::WritingProjectXml);
    let xml = write_fcpxml_for_path(&export_project, &output_fcpxml_path)?;
    std::fs::write(&output_fcpxml_path, xml)?;
    Ok(collected.result.destination_dir)
}

fn collect_files_internal<F>(
    project: &Project,
    library: &[MediaItem],
    destination_dir: &Path,
    mode: CollectFilesMode,
    reserve_existing_names: bool,
    mut on_progress: F,
) -> Result<CollectFilesManifest>
where
    F: FnMut(CollectFilesProgress),
{
    let destination_dir = if destination_dir.is_absolute() {
        destination_dir.to_path_buf()
    } else {
        std::env::current_dir()?.join(destination_dir)
    };
    std::fs::create_dir_all(&destination_dir)?;

    let mut source_to_canonical_path: HashMap<String, PathBuf> = HashMap::new();
    let mut unique_canonical_sources: Vec<PathBuf> = Vec::new();
    let mut seen_canonical_sources: HashSet<PathBuf> = HashSet::new();
    for source_path in source_paths_for_collect_mode(project, library, mode) {
        let source_local_path = source_path_to_local_path(&source_path)?;
        if !source_local_path.exists() {
            anyhow::bail!("Source media not found: {}", source_local_path.display());
        }
        let canonical_source = std::fs::canonicalize(&source_local_path).map_err(|e| {
            anyhow::anyhow!(
                "Failed to resolve source media path {}: {e}",
                source_local_path.display()
            )
        })?;
        if seen_canonical_sources.insert(canonical_source.clone()) {
            unique_canonical_sources.push(canonical_source.clone());
        }
        source_to_canonical_path.insert(source_path, canonical_source);
    }

    let mut used_file_names: HashSet<String> = HashSet::new();
    if reserve_existing_names {
        if let Ok(entries) = std::fs::read_dir(&destination_dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if !name.is_empty() {
                        used_file_names.insert(name.to_string());
                    }
                }
            }
        }
    }
    let mut canonical_to_destination_path: HashMap<PathBuf, PathBuf> = HashMap::new();
    for canonical_source in &unique_canonical_sources {
        let file_name = canonical_source
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Unable to determine file name for source media: {}",
                    canonical_source.display()
                )
            })?;
        let unique_name = unique_packaged_media_name(file_name, canonical_source, &used_file_names);
        used_file_names.insert(unique_name.clone());
        canonical_to_destination_path
            .insert(canonical_source.clone(), destination_dir.join(unique_name));
    }

    let mut lut_source_to_canonical_path: HashMap<String, PathBuf> = HashMap::new();
    let mut unique_canonical_luts: Vec<PathBuf> = Vec::new();
    let mut seen_canonical_luts: HashSet<PathBuf> = HashSet::new();
    for lut_path in collect_clip_lut_paths(project) {
        let lut_local = match source_path_to_local_path(&lut_path) {
            Ok(path) => path,
            Err(_) => continue,
        };
        if !lut_local.exists() {
            continue;
        }
        let lut_canonical = std::fs::canonicalize(&lut_local).unwrap_or(lut_local);
        if seen_canonical_luts.insert(lut_canonical.clone()) {
            unique_canonical_luts.push(lut_canonical.clone());
        }
        lut_source_to_canonical_path.insert(lut_path, lut_canonical);
    }

    let mut lut_canonical_to_destination_path: HashMap<PathBuf, PathBuf> = HashMap::new();
    for lut_canonical in &unique_canonical_luts {
        let lut_file_name = lut_canonical
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("lut.cube");
        let unique_name =
            unique_packaged_media_name(lut_file_name, lut_canonical, &used_file_names);
        used_file_names.insert(unique_name.clone());
        lut_canonical_to_destination_path
            .insert(lut_canonical.clone(), destination_dir.join(unique_name));
    }

    let total_files = unique_canonical_sources.len() + unique_canonical_luts.len();
    let mut copied_files = 0usize;

    for canonical_source in &unique_canonical_sources {
        let destination = canonical_to_destination_path
            .get(canonical_source)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Missing destination for source media: {}",
                    canonical_source.display()
                )
            })?;
        std::fs::copy(canonical_source, &destination).map_err(|e| {
            anyhow::anyhow!(
                "Failed to copy media {} to {}: {e}",
                canonical_source.display(),
                destination.display()
            )
        })?;
        let resolved_destination = std::fs::canonicalize(&destination).unwrap_or(destination);
        canonical_to_destination_path.insert(canonical_source.clone(), resolved_destination);
        copied_files += 1;
        on_progress(CollectFilesProgress::Copying {
            copied_files,
            total_files,
            current_file: canonical_source
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("media")
                .to_string(),
        });
    }

    for lut_canonical in &unique_canonical_luts {
        let destination = lut_canonical_to_destination_path
            .get(lut_canonical)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!("Missing destination for LUT: {}", lut_canonical.display())
            })?;
        std::fs::copy(lut_canonical, &destination).map_err(|e| {
            anyhow::anyhow!(
                "Failed to copy LUT {} to {}: {e}",
                lut_canonical.display(),
                destination.display()
            )
        })?;
        let resolved_destination = std::fs::canonicalize(&destination).unwrap_or(destination);
        lut_canonical_to_destination_path.insert(lut_canonical.clone(), resolved_destination);
        copied_files += 1;
        on_progress(CollectFilesProgress::Copying {
            copied_files,
            total_files,
            current_file: lut_canonical
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("lut")
                .to_string(),
        });
    }

    let mut source_to_destination_path: HashMap<String, PathBuf> = HashMap::new();
    for (source_path, canonical_source) in source_to_canonical_path {
        let destination = canonical_to_destination_path
            .get(&canonical_source)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Missing collected media mapping for source: {}",
                    canonical_source.display()
                )
            })?
            .clone();
        source_to_destination_path.insert(source_path, destination);
    }

    let mut lut_source_to_destination_path: HashMap<String, PathBuf> = HashMap::new();
    for (lut_path, lut_canonical) in lut_source_to_canonical_path {
        let destination = lut_canonical_to_destination_path
            .get(&lut_canonical)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Missing collected LUT mapping for source: {}",
                    lut_canonical.display()
                )
            })?
            .clone();
        lut_source_to_destination_path.insert(lut_path, destination);
    }

    Ok(CollectFilesManifest {
        result: CollectFilesResult {
            destination_dir,
            media_files_copied: unique_canonical_sources.len(),
            lut_files_copied: unique_canonical_luts.len(),
        },
        source_to_destination_path,
        lut_source_to_destination_path,
    })
}

fn source_paths_for_collect_mode(
    project: &Project,
    library: &[MediaItem],
    mode: CollectFilesMode,
) -> Vec<String> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for clip in project.tracks.iter().flat_map(|track| track.clips.iter()) {
        push_unique_source_path(&mut paths, &mut seen, &clip.source_path);
    }
    if mode == CollectFilesMode::EntireLibrary {
        for item in library.iter().filter(|item| item.has_backing_file()) {
            push_unique_source_path(&mut paths, &mut seen, &item.source_path);
        }
    }
    paths
}

fn collect_clip_lut_paths(project: &Project) -> Vec<String> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for lut_path in project
        .tracks
        .iter()
        .flat_map(|track| track.clips.iter())
        .flat_map(|clip| clip.lut_paths.iter())
    {
        push_unique_source_path(&mut paths, &mut seen, lut_path);
    }
    paths
}

fn push_unique_source_path(paths: &mut Vec<String>, seen: &mut HashSet<String>, source_path: &str) {
    if source_path.trim().is_empty() {
        return;
    }
    if seen.insert(source_path.to_string()) {
        paths.push(source_path.to_string());
    }
}

fn source_path_to_local_path(source_path: &str) -> Result<PathBuf> {
    if source_path.starts_with("http://") || source_path.starts_with("https://") {
        anyhow::bail!(
            "Remote media URI is not supported for project packaging: {}",
            source_path
        );
    }
    let raw_path = source_path.strip_prefix("file://").unwrap_or(source_path);
    Ok(PathBuf::from(decode_percent_encoded_path(raw_path)))
}

fn fcpxml_media_src_uri(source_path: &str) -> String {
    if source_path.starts_with("http://") || source_path.starts_with("https://") {
        return source_path.to_string();
    }
    let raw_path = source_path.strip_prefix("file://").unwrap_or(source_path);
    let decoded_path = decode_percent_encoded_path(raw_path);
    let canonical = Path::new(&decoded_path)
        .canonicalize()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or(decoded_path);
    format!("file://{}", percent_encode_path_for_fcpxml_uri(&canonical))
}

fn percent_encode_path_for_fcpxml_uri(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for &b in path.as_bytes() {
        if matches!(
            b,
            b'A'..=b'Z'
                | b'a'..=b'z'
                | b'0'..=b'9'
                | b'-'
                | b'_'
                | b'.'
                | b'~'
                | b'/'
                | b':'
        ) {
            encoded.push(b as char);
        } else {
            encoded.push('%');
            encoded.push(char::from_digit(((b >> 4) & 0x0f) as u32, 16).unwrap_or('0'));
            encoded.push(char::from_digit((b & 0x0f) as u32, 16).unwrap_or('0'));
        }
    }
    encoded
}

fn normalize_packaged_path_for_portability(path: &Path) -> PathBuf {
    if !path.is_absolute() {
        return path.to_path_buf();
    }
    let components: Vec<String> = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().to_string()),
            _ => None,
        })
        .collect();
    if components.is_empty() {
        return path.to_path_buf();
    }

    let suffix_start = if components[0] == "media" && components.len() >= 3 {
        Some(2usize)
    } else if components[0] == "run"
        && components.get(1).is_some_and(|c| c == "media")
        && components.len() >= 4
    {
        Some(3usize)
    } else if components[0] == "mnt" && components.len() >= 2 {
        Some(1usize)
    } else {
        None
    };

    let Some(start) = suffix_start else {
        return path.to_path_buf();
    };
    let mut normalized = PathBuf::from("/Volumes");
    for part in components.iter().skip(start) {
        normalized.push(part);
    }
    normalized
}

fn decode_percent_encoded_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push(((hi << 4) as u8) | (lo as u8));
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn unique_packaged_media_name(
    original_name: &str,
    canonical_source: &Path,
    used_file_names: &HashSet<String>,
) -> String {
    if !used_file_names.contains(original_name) {
        return original_name.to_string();
    }

    let mut candidate = append_hash_suffix(original_name, &short_source_hash(canonical_source));
    if !used_file_names.contains(&candidate) {
        return candidate;
    }

    let mut attempt = 2u32;
    loop {
        candidate = append_hash_suffix(
            original_name,
            &format!("{}-{attempt}", short_source_hash(canonical_source)),
        );
        if !used_file_names.contains(&candidate) {
            return candidate;
        }
        attempt += 1;
    }
}

fn append_hash_suffix(file_name: &str, suffix: &str) -> String {
    let path = Path::new(file_name);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(file_name);
    match path.extension().and_then(|s| s.to_str()) {
        Some(ext) if !ext.is_empty() => format!("{stem}_{suffix}.{ext}"),
        _ => format!("{stem}_{suffix}"),
    }
}

fn short_source_hash(path: &Path) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    format!("{:08x}", hasher.finish() as u32)
}

fn patch_imported_fcpxml_transform(project: &Project, original: &str) -> Option<String> {
    let clips: Vec<_> = project.tracks.iter().flat_map(|t| t.clips.iter()).collect();
    if clips.is_empty() {
        return None;
    }

    // If clips were added or deleted, the patch path can't handle the
    // structural change — fall through to the full rewrite path.
    let original_clip_count = original.matches("<asset-clip").count();
    if clips.len() != original_clip_count {
        return None;
    }

    let mut xml = original.to_string();
    let mut by_ref_occurrence: HashMap<String, usize> = HashMap::new();
    let mut any_change = false;

    for clip in clips {
        let asset_ref = clip.fcpxml_asset_ref.as_deref()?;
        let occurrence = by_ref_occurrence.entry(asset_ref.to_string()).or_insert(0);
        let (block_start, block_end) = find_asset_clip_block_by_ref(&xml, asset_ref, *occurrence)?;
        let block = &xml[block_start..block_end];
        let (patched_block, changed) = patch_asset_clip_block_transform(block, clip, project)?;
        xml.replace_range(block_start..block_end, &patched_block);
        any_change |= changed;
        *occurrence += 1;
    }

    if any_change {
        Some(xml)
    } else {
        None
    }
}

/// Write the body of an asset-clip in strict DTD mode: timeMap followed by
/// intrinsic params in DTD order (adjust-crop, adjust-transform, adjust-blend,
/// adjust-volume, adjust-panner).
fn write_strict_clip_body(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    clip: &crate::model::clip::Clip,
    project: &Project,
    source_start_ns: u64,
) -> Result<()> {
    // timeMap
    if let Some(fragment) = preserved_unknown_time_map_fragment(clip) {
        writer.get_mut().write_all(fragment.as_bytes())?;
    } else {
        write_native_time_map(writer, clip, &project.frame_rate)?;
    }

    // Video intrinsic params only for clips with video content
    let has_video = clip.kind != crate::model::clip::ClipKind::Audio;
    if has_video {
        let (position_x, position_y) = internal_position_to_fcpxml(
            clip.position_x,
            clip.position_y,
            project.width,
            project.height,
            clip.scale,
        );
        // Normalize negative zero to positive zero for FCP compatibility
        let position_x = if position_x == 0.0 { 0.0 } else { position_x };
        let position_y = if position_y == 0.0 { 0.0 } else { position_y };

        // adjust-crop
        let mut adjust_crop = BytesStart::new("adjust-crop");
        adjust_crop.push_attribute(("mode", "trim"));
        writer.write_event(Event::Start(adjust_crop))?;
        let mut crop_rect = BytesStart::new("crop-rect");
        crop_rect.push_attribute(("left", clip.crop_left.to_string().as_str()));
        crop_rect.push_attribute(("right", clip.crop_right.to_string().as_str()));
        crop_rect.push_attribute(("top", clip.crop_top.to_string().as_str()));
        crop_rect.push_attribute(("bottom", clip.crop_bottom.to_string().as_str()));
        writer.write_event(Event::Empty(crop_rect))?;
        writer.write_event(Event::End(BytesEnd::new("adjust-crop")))?;

        // adjust-transform
        let mut adjust_transform = BytesStart::new("adjust-transform");
        let has_position_kfs =
            !clip.position_x_keyframes.is_empty() || !clip.position_y_keyframes.is_empty();
        let has_scale_kfs = !clip.scale_keyframes.is_empty();
        let has_rotation_kfs = !clip.rotate_keyframes.is_empty();
        let has_transform_kfs = has_position_kfs || has_scale_kfs || has_rotation_kfs;
        // FCP omits inline attrs for properties that have keyframes
        if !has_position_kfs {
            adjust_transform.push_attribute((
                "position",
                format!("{} {}", position_x, position_y).as_str(),
            ));
        }
        if !has_scale_kfs {
            adjust_transform
                .push_attribute(("scale", format!("{} {}", clip.scale, clip.scale).as_str()));
        }
        if !has_rotation_kfs {
            adjust_transform.push_attribute(("rotation", clip.rotate.to_string().as_str()));
        }
        if has_transform_kfs {
            writer.write_event(Event::Start(adjust_transform))?;
            write_transform_keyframe_params(writer, clip, project, source_start_ns)?;
            writer.write_event(Event::End(BytesEnd::new("adjust-transform")))?;
        } else {
            writer.write_event(Event::Empty(adjust_transform))?;
        }

        // adjust-blend
        let mut adjust_blend = BytesStart::new("adjust-blend");
        adjust_blend.push_attribute(("amount", clip.opacity.to_string().as_str()));
        if !clip.opacity_keyframes.is_empty() {
            writer.write_event(Event::Start(adjust_blend))?;
            write_opacity_keyframe_params(writer, clip, &project.frame_rate, source_start_ns)?;
            writer.write_event(Event::End(BytesEnd::new("adjust-blend")))?;
        } else {
            writer.write_event(Event::Empty(adjust_blend))?;
        }
    }

    // audio intrinsic params (adjust-volume, adjust-panner)
    // DTD: these come after video intrinsic params, before anchor_items.
    // When keyframed, the keyframes go in audio-channel-source (emitted later,
    // after connected clips per DTD), so we skip the flat element here.
    if clip.volume_keyframes.is_empty() {
        let mut adjust_volume = BytesStart::new("adjust-volume");
        adjust_volume.push_attribute((
            "amount",
            linear_volume_to_fcpxml_db(clip.volume as f64).as_str(),
        ));
        writer.write_event(Event::Empty(adjust_volume))?;
    }

    if clip.pan_keyframes.is_empty() {
        let mut adjust_panner = BytesStart::new("adjust-panner");
        adjust_panner.push_attribute((
            "amount",
            format!("{:.6}", clip.pan.clamp(-1.0, 1.0)).as_str(),
        ));
        writer.write_event(Event::Empty(adjust_panner))?;
    }

    Ok(())
}

/// Emit `<audio-channel-source>` with keyframed volume/pan.
/// Per DTD, audio-channel-source comes AFTER anchor_items (connected clips).
fn write_strict_audio_channel_sources(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    clip: &crate::model::clip::Clip,
    fps: &crate::model::project::FrameRate,
    source_start_ns: u64,
) -> Result<()> {
    let has_vol_kf = !clip.volume_keyframes.is_empty();
    let has_pan_kf = !clip.pan_keyframes.is_empty();

    if !has_vol_kf && !has_pan_kf {
        return Ok(());
    }

    let mut acs = BytesStart::new("audio-channel-source");
    acs.push_attribute(("srcCh", "1, 2"));
    acs.push_attribute(("role", "dialogue"));
    writer.write_event(Event::Start(acs))?;

    if has_vol_kf {
        let mut adjust_volume = BytesStart::new("adjust-volume");
        adjust_volume.push_attribute((
            "amount",
            linear_volume_to_fcpxml_db(clip.volume as f64).as_str(),
        ));
        writer.write_event(Event::Start(adjust_volume))?;
        write_volume_keyframe_params(writer, clip, fps, source_start_ns, true)?;
        writer.write_event(Event::End(BytesEnd::new("adjust-volume")))?;
    }

    if has_pan_kf {
        let mut adjust_panner = BytesStart::new("adjust-panner");
        adjust_panner.push_attribute((
            "amount",
            format!("{:.6}", clip.pan.clamp(-1.0, 1.0)).as_str(),
        ));
        writer.write_event(Event::Start(adjust_panner))?;
        write_pan_keyframe_params(writer, clip, fps, source_start_ns, true)?;
        writer.write_event(Event::End(BytesEnd::new("adjust-panner")))?;
    }

    writer.write_event(Event::End(BytesEnd::new("audio-channel-source")))?;
    Ok(())
}

/// FCP "Color Adjustments" effect UID (built-in FxPlug).
const FCP_COLOR_ADJUSTMENTS_UID: &str = "FxPlug:7E2022A5-202B-4EEB-A311-AC2B585D01B0";
/// Resource ID used for the Color Adjustments effect in strict FCPXML.
const COLOR_ADJUSTMENTS_EFFECT_ID: &str = "r_fcp_color_adj";

/// Check whether a clip has any non-default color adjustment values that need
/// a `<filter-video name="Color Adjustments">` element.
fn clip_has_color_adjustments(clip: &crate::model::clip::Clip) -> bool {
    clip.exposure != 0.0
        || clip.brightness != 0.0
        || clip.contrast != 1.0
        || clip.saturation != 1.0
        || clip.highlights != 0.0
        || clip.black_point != 0.0
        || clip.shadows != 0.0
        || clip.highlights_warmth != 0.0
        || clip.highlights_tint != 0.0
        || clip.midtones_warmth != 0.0
        || clip.midtones_tint != 0.0
        || clip.shadows_warmth != 0.0
        || clip.shadows_tint != 0.0
}

/// Emit `<filter-video ref="..." name="Color Adjustments">` with `<param>` children.
/// Per DTD, filter-video comes AFTER audio-channel-source*.
fn write_strict_filter_video_color(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    clip: &crate::model::clip::Clip,
    color_effect_id: &str,
) -> Result<()> {
    if !clip_has_color_adjustments(clip) {
        return Ok(());
    }
    let mut fv = BytesStart::new("filter-video");
    fv.push_attribute(("ref", color_effect_id));
    fv.push_attribute(("name", "Color Adjustments"));
    writer.write_event(Event::Start(fv))?;

    // Helper: emit <param name="..." key="..." value="..." />
    // Round to 4 decimal places to avoid f32 precision artifacts.
    fn write_param(
        writer: &mut Writer<Cursor<Vec<u8>>>,
        name: &str,
        key: &str,
        value: f32,
    ) -> Result<()> {
        let rounded = (value * 10000.0).round() / 10000.0;
        let mut p = BytesStart::new("param");
        p.push_attribute(("name", name));
        p.push_attribute(("key", key));
        p.push_attribute(("value", format!("{}", rounded).as_str()));
        writer.write_event(Event::Empty(p))?;
        Ok(())
    }

    // Convert UltimateSlice internal values back to FCP −100..100 range.
    write_param(writer, "Exposure", "3", clip.exposure * 100.0)?;
    write_param(writer, "Contrast", "17", (clip.contrast - 1.0) * 100.0)?;
    write_param(writer, "Brightness", "2", clip.brightness * 100.0)?;
    write_param(writer, "Highlights", "7", clip.highlights * 100.0)?;
    write_param(writer, "Black Point", "1", clip.black_point * 100.0)?;
    write_param(writer, "Shadows", "4", clip.shadows * 100.0)?;
    write_param(writer, "Saturation", "16", (clip.saturation - 1.0) * 100.0)?;
    write_param(
        writer,
        "Highlights Warmth",
        "10",
        clip.highlights_warmth * 100.0,
    )?;
    write_param(
        writer,
        "Highlights Tint",
        "11",
        clip.highlights_tint * 100.0,
    )?;
    write_param(
        writer,
        "Midtones Warmth",
        "12",
        clip.midtones_warmth * 100.0,
    )?;
    write_param(writer, "Midtones Tint", "13", clip.midtones_tint * 100.0)?;
    write_param(writer, "Shadows Warmth", "14", clip.shadows_warmth * 100.0)?;
    write_param(writer, "Shadows Tint", "15", clip.shadows_tint * 100.0)?;

    writer.write_event(Event::End(BytesEnd::new("filter-video")))?;
    Ok(())
}

fn internal_position_to_fcpxml(
    x: f64,
    y: f64,
    project_width: u32,
    project_height: u32,
    scale: f64,
) -> (f64, f64) {
    let range_x = (project_width as f64) * (1.0 - scale) / 2.0;
    let range_y = (project_height as f64) * (1.0 - scale) / 2.0;
    let (shift_x_px, shift_y_px) = if range_x.abs() < f64::EPSILON || range_y.abs() < f64::EPSILON {
        (
            x * (project_width as f64 / 2.0),
            y * (project_height as f64 / 2.0),
        )
    } else {
        (x * range_x, y * range_y)
    };
    let frame_height = project_height as f64;
    let x_percent = shift_x_px * 100.0 / frame_height;
    let y_percent = -shift_y_px * 100.0 / frame_height;
    (x_percent, y_percent)
}

fn find_asset_clip_block_by_ref(
    xml: &str,
    asset_ref: &str,
    occurrence: usize,
) -> Option<(usize, usize)> {
    let mut search_from = 0usize;
    let mut seen = 0usize;
    let ref_attr = format!(r#"ref="{asset_ref}""#);
    while let Some(rel_start) = xml[search_from..].find("<asset-clip") {
        let start = search_from + rel_start;
        let tag_end_rel = xml[start..].find('>')?;
        let tag_end = start + tag_end_rel;
        let tag_text = &xml[start..=tag_end];
        let block_end = find_asset_clip_block_end(xml, start, tag_end)?;
        if tag_text.contains(&ref_attr) {
            if seen == occurrence {
                return Some((start, block_end));
            }
            seen += 1;
        }
        search_from = start + 1;
    }
    None
}

fn find_asset_clip_block_end(xml: &str, _start: usize, tag_end: usize) -> Option<usize> {
    let open_tag = &xml[..=tag_end];
    let open_tag_start = open_tag.rfind("<asset-clip")?;
    let open_tag = &open_tag[open_tag_start..];
    if open_tag.trim_end().ends_with("/>") {
        return Some(tag_end + 1);
    }
    let mut depth = 1usize;
    let mut cursor = tag_end + 1;
    while depth > 0 {
        let next_open = xml[cursor..].find("<asset-clip").map(|i| cursor + i);
        let next_close = xml[cursor..].find("</asset-clip>").map(|i| cursor + i);
        match (next_open, next_close) {
            (Some(open), Some(close)) if open < close => {
                let open_end = open + xml[open..].find('>')?;
                let open_text = &xml[open..=open_end];
                if !open_text.trim_end().ends_with("/>") {
                    depth += 1;
                }
                cursor = open_end + 1;
            }
            (_, Some(close)) => {
                depth = depth.saturating_sub(1);
                cursor = close + "</asset-clip>".len();
                if depth == 0 {
                    return Some(cursor);
                }
            }
            _ => return None,
        }
    }
    None
}

fn patch_asset_clip_block_transform(
    block: &str,
    clip: &crate::model::clip::Clip,
    project: &Project,
) -> Option<(String, bool)> {
    match find_direct_child_tag_range(block, "adjust-transform") {
        Some(_) => {}
        None => return Some((block.to_string(), false)),
    }

    let start_tag_end = block.find('>')?;
    let start_tag = &block[..=start_tag_end];
    let mut updated_start = start_tag.to_string();
    let mut changed = false;

    for (attr, value) in [
        (
            "offset",
            ns_to_fcpxml_time(clip.timeline_start, &project.frame_rate),
        ),
        (
            "start",
            ns_to_fcpxml_time(
                clip.source_timecode_start_ns().unwrap_or(clip.source_in),
                &project.frame_rate,
            ),
        ),
        (
            "duration",
            ns_to_fcpxml_time(clip.duration(), &project.frame_rate),
        ),
    ] {
        let next = replace_or_insert_attr(&updated_start, attr, &value)?;
        if next != updated_start {
            changed = true;
        }
        updated_start = next;
    }

    let next = match clip.source_timecode_base_ns {
        Some(source_timecode_base_ns) => replace_or_insert_attr(
            &updated_start,
            "us:source-timecode-base-ns",
            &source_timecode_base_ns.to_string(),
        )?,
        None => remove_attr(&updated_start, "us:source-timecode-base-ns"),
    };
    if next != updated_start {
        changed = true;
    }
    updated_start = next;

    for (attr, value) in [
        ("us:brightness", clip.brightness.to_string()),
        ("us:contrast", clip.contrast.to_string()),
        ("us:saturation", clip.saturation.to_string()),
        ("us:temperature", clip.temperature.to_string()),
        ("us:tint", clip.tint.to_string()),
        (
            "us:anamorphic-desqueeze",
            clip.anamorphic_desqueeze.to_string(),
        ),
        ("us:scale", clip.scale.to_string()),
        ("us:position-x", clip.position_x.to_string()),
        ("us:position-y", clip.position_y.to_string()),
        ("us:opacity", clip.opacity.to_string()),
        ("us:rotate", clip.rotate.to_string()),
    ] {
        let next = replace_or_insert_attr(&updated_start, attr, &value)?;
        if next != updated_start {
            changed = true;
        }
        updated_start = next;
    }

    let keyframe_attrs: [(&str, Option<String>); 16] = [
        (
            "us:brightness-keyframes",
            if clip.brightness_keyframes.is_empty() {
                None
            } else {
                serde_json::to_string(&clip.brightness_keyframes).ok()
            },
        ),
        (
            "us:contrast-keyframes",
            if clip.contrast_keyframes.is_empty() {
                None
            } else {
                serde_json::to_string(&clip.contrast_keyframes).ok()
            },
        ),
        (
            "us:saturation-keyframes",
            if clip.saturation_keyframes.is_empty() {
                None
            } else {
                serde_json::to_string(&clip.saturation_keyframes).ok()
            },
        ),
        (
            "us:temperature-keyframes",
            if clip.temperature_keyframes.is_empty() {
                None
            } else {
                serde_json::to_string(&clip.temperature_keyframes).ok()
            },
        ),
        (
            "us:tint-keyframes",
            if clip.tint_keyframes.is_empty() {
                None
            } else {
                serde_json::to_string(&clip.tint_keyframes).ok()
            },
        ),
        (
            "us:scale-keyframes",
            if clip.scale_keyframes.is_empty() {
                None
            } else {
                serde_json::to_string(&clip.scale_keyframes).ok()
            },
        ),
        (
            "us:opacity-keyframes",
            if clip.opacity_keyframes.is_empty() {
                None
            } else {
                serde_json::to_string(&clip.opacity_keyframes).ok()
            },
        ),
        (
            "us:position-x-keyframes",
            if clip.position_x_keyframes.is_empty() {
                None
            } else {
                serde_json::to_string(&clip.position_x_keyframes).ok()
            },
        ),
        (
            "us:position-y-keyframes",
            if clip.position_y_keyframes.is_empty() {
                None
            } else {
                serde_json::to_string(&clip.position_y_keyframes).ok()
            },
        ),
        (
            "us:volume-keyframes",
            if clip.volume_keyframes.is_empty() {
                None
            } else {
                serde_json::to_string(&clip.volume_keyframes).ok()
            },
        ),
        (
            "us:pan-keyframes",
            if clip.pan_keyframes.is_empty() {
                None
            } else {
                serde_json::to_string(&clip.pan_keyframes).ok()
            },
        ),
        (
            "us:rotate-keyframes",
            if clip.rotate_keyframes.is_empty() {
                None
            } else {
                serde_json::to_string(&clip.rotate_keyframes).ok()
            },
        ),
        (
            "us:crop-left-keyframes",
            if clip.crop_left_keyframes.is_empty() {
                None
            } else {
                serde_json::to_string(&clip.crop_left_keyframes).ok()
            },
        ),
        (
            "us:crop-right-keyframes",
            if clip.crop_right_keyframes.is_empty() {
                None
            } else {
                serde_json::to_string(&clip.crop_right_keyframes).ok()
            },
        ),
        (
            "us:crop-top-keyframes",
            if clip.crop_top_keyframes.is_empty() {
                None
            } else {
                serde_json::to_string(&clip.crop_top_keyframes).ok()
            },
        ),
        (
            "us:crop-bottom-keyframes",
            if clip.crop_bottom_keyframes.is_empty() {
                None
            } else {
                serde_json::to_string(&clip.crop_bottom_keyframes).ok()
            },
        ),
    ];
    for (attr, value) in keyframe_attrs {
        let next = if let Some(v) = value {
            replace_or_insert_attr(&updated_start, attr, &v)?
        } else {
            remove_attr(&updated_start, attr)
        };
        if next != updated_start {
            changed = true;
        }
        updated_start = next;
    }

    // Patch title styling attributes.
    for (attr, value) in [
        ("us:title-text", clip.title_text.clone()),
        ("us:title-font", clip.title_font.clone()),
        ("us:title-color", format!("{:08X}", clip.title_color)),
        ("us:title-x", clip.title_x.to_string()),
        ("us:title-y", clip.title_y.to_string()),
    ] {
        let next = replace_or_insert_attr(&updated_start, attr, &value)?;
        if next != updated_start {
            changed = true;
        }
        updated_start = next;
    }
    // Conditional title attrs
    for (attr, value) in [
        (
            "us:title-template",
            if clip.title_template.is_empty() {
                None
            } else {
                Some(clip.title_template.clone())
            },
        ),
        (
            "us:title-outline-color",
            if clip.title_outline_width > 0.0 {
                Some(format!("{:08X}", clip.title_outline_color))
            } else {
                None
            },
        ),
        (
            "us:title-outline-width",
            if clip.title_outline_width > 0.0 {
                Some(clip.title_outline_width.to_string())
            } else {
                None
            },
        ),
        (
            "us:title-shadow",
            if clip.title_shadow {
                Some("true".to_string())
            } else {
                None
            },
        ),
        (
            "us:title-shadow-color",
            if clip.title_shadow {
                Some(format!("{:08X}", clip.title_shadow_color))
            } else {
                None
            },
        ),
        (
            "us:title-shadow-offset-x",
            if clip.title_shadow {
                Some(clip.title_shadow_offset_x.to_string())
            } else {
                None
            },
        ),
        (
            "us:title-shadow-offset-y",
            if clip.title_shadow {
                Some(clip.title_shadow_offset_y.to_string())
            } else {
                None
            },
        ),
        (
            "us:title-bg-box",
            if clip.title_bg_box {
                Some("true".to_string())
            } else {
                None
            },
        ),
        (
            "us:title-bg-box-color",
            if clip.title_bg_box {
                Some(format!("{:08X}", clip.title_bg_box_color))
            } else {
                None
            },
        ),
        (
            "us:title-bg-box-padding",
            if clip.title_bg_box {
                Some(clip.title_bg_box_padding.to_string())
            } else {
                None
            },
        ),
        (
            "us:title-clip-bg-color",
            if clip.title_clip_bg_color != 0 {
                Some(format!("{:08X}", clip.title_clip_bg_color))
            } else {
                None
            },
        ),
        (
            "us:title-secondary-text",
            if clip.title_secondary_text.is_empty() {
                None
            } else {
                Some(clip.title_secondary_text.clone())
            },
        ),
        (
            "us:clip-kind",
            match clip.kind {
                crate::model::clip::ClipKind::Title => Some("title".to_string()),
                crate::model::clip::ClipKind::Adjustment => Some("adjustment".to_string()),
                crate::model::clip::ClipKind::Compound => Some("compound".to_string()),
                crate::model::clip::ClipKind::Multicam => Some("multicam".to_string()),
                _ => None,
            },
        ),
        (
            "us:compound-tracks",
            if clip.kind == crate::model::clip::ClipKind::Compound {
                clip.compound_tracks
                    .as_ref()
                    .and_then(|t| serde_json::to_string(t).ok())
                    .map(|j| j.replace('"', "&quot;"))
            } else {
                None
            },
        ),
        (
            "us:multicam-angles",
            if clip.kind == crate::model::clip::ClipKind::Multicam {
                clip.multicam_angles
                    .as_ref()
                    .and_then(|a| serde_json::to_string(a).ok())
                    .map(|j| j.replace('"', "&quot;"))
            } else {
                None
            },
        ),
        (
            "us:multicam-switches",
            if clip.kind == crate::model::clip::ClipKind::Multicam {
                clip.multicam_switches
                    .as_ref()
                    .and_then(|s| serde_json::to_string(s).ok())
                    .map(|j| j.replace('"', "&quot;"))
            } else {
                None
            },
        ),
    ] {
        let next = if let Some(v) = value {
            replace_or_insert_attr(&updated_start, attr, &v)?
        } else {
            remove_attr(&updated_start, attr)
        };
        if next != updated_start {
            changed = true;
        }
        updated_start = next;
    }

    // Patch frei0r effects JSON attribute.
    {
        let frei0r_value = if clip.frei0r_effects.is_empty() {
            None
        } else {
            serde_json::to_string(&clip.frei0r_effects)
                .ok()
                .map(|s| s.replace('"', "&quot;"))
        };
        let next = if let Some(v) = frei0r_value {
            replace_or_insert_attr(&updated_start, "us:frei0r-effects", &v)?
        } else {
            remove_attr(&updated_start, "us:frei0r-effects")
        };
        if next != updated_start {
            changed = true;
        }
        updated_start = next;
    }

    // Patch masks JSON attribute.
    {
        let masks_value = if clip.masks.is_empty() {
            None
        } else {
            serde_json::to_string(&clip.masks)
                .ok()
                .map(|s| s.replace('"', "&quot;"))
        };
        let next = if let Some(v) = masks_value {
            replace_or_insert_attr(&updated_start, "us:masks", &v)?
        } else {
            remove_attr(&updated_start, "us:masks")
        };
        if next != updated_start {
            changed = true;
        }
        updated_start = next;
    }

    let mut updated_block = String::with_capacity(block.len());
    updated_block.push_str(&updated_start);
    updated_block.push_str(&block[start_tag_end + 1..]);

    let (transform_start, transform_end) =
        find_direct_child_tag_range(&updated_block, "adjust-transform")?;
    let transform_text = &updated_block[transform_start..transform_end];
    let transform_scale = format!("{} {}", clip.scale, clip.scale);
    let (position_x, position_y) = internal_position_to_fcpxml(
        clip.position_x,
        clip.position_y,
        project.width,
        project.height,
        clip.scale,
    );
    let transform_position = format!("{} {}", position_x, position_y);
    let patched = replace_or_insert_attr(
        &replace_or_insert_attr(
            &replace_or_insert_attr(transform_text, "scale", &transform_scale)?,
            "position",
            &transform_position,
        )?,
        "rotation",
        &clip.rotate.to_string(),
    )?;
    if patched != transform_text {
        changed = true;
        updated_block.replace_range(transform_start..transform_end, &patched);
    }

    Some((updated_block, changed))
}

fn find_direct_child_tag_range(asset_clip_block: &str, tag_name: &str) -> Option<(usize, usize)> {
    let root_end = asset_clip_block.find('>')?;
    let root_tag = &asset_clip_block[..=root_end];
    if root_tag.trim_end().ends_with("/>") {
        return None;
    }

    let open_tag = format!("<{tag_name}");
    let mut nested_asset_depth = 0usize;
    let mut cursor = root_end + 1;
    while cursor < asset_clip_block.len() {
        let next_lt_rel = match asset_clip_block[cursor..].find('<') {
            Some(v) => v,
            None => break,
        };
        let lt = cursor + next_lt_rel;
        if asset_clip_block[lt..].starts_with("</asset-clip>") {
            if nested_asset_depth == 0 {
                break;
            }
            nested_asset_depth -= 1;
            cursor = lt + "</asset-clip>".len();
            continue;
        }
        if asset_clip_block[lt..].starts_with("<asset-clip") {
            let end = lt + asset_clip_block[lt..].find('>')?;
            let tag = &asset_clip_block[lt..=end];
            if !tag.trim_end().ends_with("/>") {
                nested_asset_depth += 1;
            }
            cursor = end + 1;
            continue;
        }
        let end = lt + asset_clip_block[lt..].find('>')?;
        if nested_asset_depth == 0 && asset_clip_block[lt..].starts_with(&open_tag) {
            return Some((lt, end + 1));
        }
        cursor = end + 1;
    }
    None
}

fn replace_or_insert_attr(tag_text: &str, attr_name: &str, new_value: &str) -> Option<String> {
    let attr_prefix = format!(r#"{attr_name}=""#);
    if let Some(attr_start) = tag_text.find(&attr_prefix) {
        let value_start = attr_start + attr_prefix.len();
        let value_end_rel = tag_text[value_start..].find('"')?;
        let value_end = value_start + value_end_rel;
        let mut updated = String::with_capacity(
            tag_text.len()
                + new_value
                    .len()
                    .saturating_sub(value_end.saturating_sub(value_start)),
        );
        updated.push_str(&tag_text[..value_start]);
        updated.push_str(new_value);
        updated.push_str(&tag_text[value_end..]);
        return Some(updated);
    }

    let insert_pos = if let Some(pos) = tag_text.rfind("/>") {
        pos
    } else {
        tag_text.rfind('>')?
    };
    let mut updated = String::with_capacity(tag_text.len() + attr_name.len() + new_value.len() + 4);
    updated.push_str(&tag_text[..insert_pos]);
    updated.push(' ');
    updated.push_str(attr_name);
    updated.push_str("=\"");
    updated.push_str(new_value);
    updated.push('"');
    updated.push_str(&tag_text[insert_pos..]);
    Some(updated)
}

fn remove_attr(tag_text: &str, attr_name: &str) -> String {
    let attr_prefix = format!(r#" {attr_name}=""#);
    if let Some(attr_start) = tag_text.find(&attr_prefix) {
        let value_start = attr_start + attr_prefix.len();
        if let Some(value_end_rel) = tag_text[value_start..].find('"') {
            let value_end = value_start + value_end_rel + 1;
            let mut updated =
                String::with_capacity(tag_text.len().saturating_sub(value_end - attr_start));
            updated.push_str(&tag_text[..attr_start]);
            updated.push_str(&tag_text[value_end..]);
            return updated;
        }
    }
    tag_text.to_string()
}

/// Build export context by probing media files for native frame rate and
/// embedded timecodes. Only used for strict FCPXML export to FCP.
fn build_export_context(project: &Project) -> ExportContext {
    let project_fps_key = (project.frame_rate.numerator, project.frame_rate.denominator);
    let mut media_map: HashMap<String, MediaExportInfo> = HashMap::new();
    let mut fps_to_format: HashMap<(u32, u32), String> = HashMap::new();
    fps_to_format.insert(project_fps_key, "r1".to_string());
    let mut next_format_id = 2u32;
    let mut extra_formats = Vec::new();
    let mut audio_format_id: Option<String> = None;

    for track in project.video_tracks().chain(project.audio_tracks()) {
        for clip in &track.clips {
            let source = clip
                .fcpxml_original_source_path
                .as_deref()
                .unwrap_or(&clip.source_path);
            if media_map.contains_key(source) {
                continue;
            }

            let probed = probe_media_format(source);
            let is_audio_only = match &probed {
                Some((_, _, w, h, _)) => *w == 0 && *h == 0,
                None => clip.kind == crate::model::clip::ClipKind::Audio,
            };

            if is_audio_only {
                // Audio-only: use FFVideoFormatRateUndefined with 48kHz time base.
                let fmt_id = audio_format_id.get_or_insert_with(|| {
                    let id = format!("r{next_format_id}");
                    next_format_id += 1;
                    id
                });
                let duration_ns = probe_audio_duration(source);
                // BWF time_reference (in samples) → nanoseconds for asset start.
                let timecode_ns = probe_audio_time_reference(source);
                media_map.insert(
                    source.to_string(),
                    MediaExportInfo {
                        format_id: fmt_id.clone(),
                        fps: FrameRate {
                            numerator: 48000,
                            denominator: 1,
                        },
                        timecode_ns,
                        width: 0,
                        height: 0,
                        is_audio_only: true,
                        duration_ns,
                    },
                );
                continue;
            }

            let (fps_num, fps_den, media_w, media_h, probed_tc) = match probed {
                Some(result) => result,
                None => (
                    project.frame_rate.numerator,
                    project.frame_rate.denominator,
                    project.width,
                    project.height,
                    None,
                ),
            };

            let fps_key = (fps_num, fps_den);
            let format_id = fps_to_format
                .entry(fps_key)
                .or_insert_with(|| {
                    let id = format!("r{next_format_id}");
                    next_format_id += 1;
                    extra_formats.push((
                        id.clone(),
                        FrameRate {
                            numerator: fps_num,
                            denominator: fps_den,
                        },
                        media_w,
                        media_h,
                    ));
                    id
                })
                .clone();

            // Prefer probed timecode, fall back to clip's stored value.
            let timecode_ns = probed_tc.or(clip.source_timecode_base_ns);

            media_map.insert(
                source.to_string(),
                MediaExportInfo {
                    format_id,
                    fps: FrameRate {
                        numerator: fps_num,
                        denominator: fps_den,
                    },
                    timecode_ns,
                    width: media_w,
                    height: media_h,
                    is_audio_only: false,
                    duration_ns: None,
                },
            );
        }
    }

    ExportContext {
        media: media_map,
        extra_formats,
        audio_format_id,
    }
}

/// Probe an audio file with ffprobe to get its duration in nanoseconds.
fn probe_audio_duration(path: &str) -> Option<u64> {
    use std::process::Command;
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
            path,
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let secs: f64 = text.trim().parse().ok()?;
    Some((secs * 1_000_000_000.0) as u64)
}

/// Probe an audio file for BWF time_reference (sample offset timecode).
/// Returns the timecode in nanoseconds (at 48kHz: samples * 1e9 / 48000).
fn probe_audio_time_reference(path: &str) -> Option<u64> {
    use std::process::Command;
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format_tags=time_reference",
            "-of",
            "csv=p=0",
            path,
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let samples: u64 = text.trim().parse().ok()?;
    if samples == 0 {
        return None;
    }
    // Convert samples at 48kHz to nanoseconds.
    // Use integer division that round-trips through ns_to_fcpxml_time.
    Some(samples * 1_000_000_000 / 48000)
}

/// Probe a media file with ffprobe to get native frame rate, resolution, and embedded timecode.
fn probe_media_format(path: &str) -> Option<(u32, u32, u32, u32, Option<u64>)> {
    use std::process::Command;

    let output = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=r_frame_rate,width,height",
            "-show_entries",
            "stream_tags=timecode",
            "-of",
            "csv=p=0",
            path,
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.trim();
    if line.is_empty() {
        return None;
    }

    // CSV format: "width,height,fps_ratio[,timecode]"
    // e.g. "5312,2988,24000/1001,20:13:33:07" or "1920,1080,24/1"
    let fields: Vec<&str> = line.splitn(4, ',').collect();
    if fields.len() < 3 {
        return None;
    }

    let width: u32 = fields[0].trim().parse().ok()?;
    let height: u32 = fields[1].trim().parse().ok()?;
    let fps_str = fields[2].trim();
    let tc_str = fields.get(3).copied();

    let mut fps_parts = fps_str.split('/');
    let fps_num: u32 = fps_parts.next()?.parse().ok()?;
    let fps_den: u32 = fps_parts.next()?.parse().ok()?;

    let tc_ns = tc_str.and_then(|tc| parse_timecode_to_ns_writer(tc.trim(), fps_num, fps_den));

    Some((fps_num, fps_den, width, height, tc_ns))
}

/// Parse a timecode string (HH:MM:SS:FF or HH:MM:SS;FF) to nanoseconds.
fn parse_timecode_to_ns_writer(tc: &str, fps_num: u32, fps_den: u32) -> Option<u64> {
    let tc = tc.replace(';', ":");
    let parts: Vec<&str> = tc.split(':').collect();
    if parts.len() != 4 {
        return None;
    }

    let hours: u64 = parts[0].parse().ok()?;
    let minutes: u64 = parts[1].parse().ok()?;
    let seconds: u64 = parts[2].parse().ok()?;
    let frames: u64 = parts[3].parse().ok()?;

    let nominal_fps = (fps_num as u64 + fps_den as u64 - 1) / fps_den as u64;

    let total_frames =
        hours * 3600 * nominal_fps + minutes * 60 * nominal_fps + seconds * nominal_fps + frames;

    Some(total_frames * fps_den as u64 * 1_000_000_000 / fps_num as u64)
}

/// Generate a known FCP format name for a given resolution and frame rate.
fn known_fcpxml_format_name_for(
    width: u32,
    height: u32,
    fps_num: u32,
    fps_den: u32,
) -> Option<&'static str> {
    match (width, height, fps_num, fps_den) {
        (1920, 1080, 24000, 1001) => Some("FFVideoFormat1080p2398"),
        (1920, 1080, 24, 1) => Some("FFVideoFormat1080p24"),
        (1920, 1080, 25, 1) => Some("FFVideoFormat1080p25"),
        (1920, 1080, 30000, 1001) => Some("FFVideoFormat1080p2997"),
        (1920, 1080, 30, 1) => Some("FFVideoFormat1080p30"),
        (1920, 1080, 50, 1) => Some("FFVideoFormat1080p50"),
        (1920, 1080, 60000, 1001) => Some("FFVideoFormat1080p5994"),
        (1920, 1080, 60, 1) => Some("FFVideoFormat1080p60"),
        (3840, 2160, 24000, 1001) => Some("FFVideoFormat2160p2398"),
        (3840, 2160, 24, 1) => Some("FFVideoFormat2160p24"),
        (3840, 2160, 25, 1) => Some("FFVideoFormat2160p25"),
        (3840, 2160, 30000, 1001) => Some("FFVideoFormat2160p2997"),
        (3840, 2160, 30, 1) => Some("FFVideoFormat2160p30"),
        (3840, 2160, 60000, 1001) => Some("FFVideoFormat2160p5994"),
        (3840, 2160, 60, 1) => Some("FFVideoFormat2160p60"),
        (1280, 720, 24000, 1001) => Some("FFVideoFormat720p2398"),
        (1280, 720, 25, 1) => Some("FFVideoFormat720p25"),
        (1280, 720, 30000, 1001) => Some("FFVideoFormat720p2997"),
        (1280, 720, 50, 1) => Some("FFVideoFormat720p50"),
        (1280, 720, 60000, 1001) => Some("FFVideoFormat720p5994"),
        (1280, 720, 60, 1) => Some("FFVideoFormat720p60"),
        _ => None,
    }
}

fn write_resources(
    project: &Project,
    writer: &mut Writer<Cursor<Vec<u8>>>,
    options: WriterOptions,
    export_ctx: Option<&ExportContext>,
    asset_id_by_source: &HashMap<String, String>,
) -> Result<()> {
    let strip_unknown_fields = options.strict_dtd;
    let mut resources = BytesStart::new("resources");
    if !strip_unknown_fields {
        for (k, v) in &project.fcpxml_unknown_resources.attrs {
            if !is_writer_managed_resources_attr(k) {
                resources.push_attribute((k.as_str(), v.as_str()));
            }
        }
    }
    writer.write_event(Event::Start(resources))?;

    // Format resource (r1 = project/sequence format)
    let _fps = format!(
        "{}/{}",
        project.frame_rate.numerator, project.frame_rate.denominator
    );
    let mut fmt = BytesStart::new("format");
    fmt.push_attribute(("id", "r1"));
    if let Some(name) = known_fcpxml_format_name(project) {
        fmt.push_attribute(("name", name));
    }
    fmt.push_attribute((
        "frameDuration",
        format!(
            "{}/{}s",
            project.frame_rate.denominator, project.frame_rate.numerator
        )
        .as_str(),
    ));
    fmt.push_attribute(("width", project.width.to_string().as_str()));
    fmt.push_attribute(("height", project.height.to_string().as_str()));
    // FCP expects colorSpace on format resources; emit Rec. 709 default in strict mode.
    if strip_unknown_fields {
        fmt.push_attribute(("colorSpace", "1-1-1 (Rec. 709)"));
    }
    if !strip_unknown_fields {
        for (k, v) in &project.fcpxml_unknown_format.attrs {
            if !is_writer_managed_format_attr(k) {
                fmt.push_attribute((k.as_str(), v.as_str()));
            }
        }
    }
    if strip_unknown_fields || project.fcpxml_unknown_format.children.is_empty() {
        writer.write_event(Event::Empty(fmt))?;
    } else {
        writer.write_event(Event::Start(fmt))?;
        for fragment in &project.fcpxml_unknown_format.children {
            writer.get_mut().write_all(fragment.as_bytes())?;
        }
        writer.write_event(Event::End(BytesEnd::new("format")))?;
    }

    // Extra format elements for media with different native frame rates.
    if let Some(ctx) = export_ctx {
        for (fmt_id, fps, w, h) in &ctx.extra_formats {
            let mut extra_fmt = BytesStart::new("format");
            extra_fmt.push_attribute(("id", fmt_id.as_str()));
            if let Some(name) = known_fcpxml_format_name_for(*w, *h, fps.numerator, fps.denominator)
            {
                extra_fmt.push_attribute(("name", name));
            }
            extra_fmt.push_attribute((
                "frameDuration",
                format!("{}/{}s", fps.denominator, fps.numerator).as_str(),
            ));
            extra_fmt.push_attribute(("width", w.to_string().as_str()));
            extra_fmt.push_attribute(("height", h.to_string().as_str()));
            if strip_unknown_fields {
                extra_fmt.push_attribute(("colorSpace", "1-1-1 (Rec. 709)"));
            }
            writer.write_event(Event::Empty(extra_fmt))?;
        }

        // Audio-only format: FFVideoFormatRateUndefined (no frameDuration/dimensions).
        if let Some(ref audio_fmt_id) = ctx.audio_format_id {
            let mut audio_fmt = BytesStart::new("format");
            audio_fmt.push_attribute(("id", audio_fmt_id.as_str()));
            audio_fmt.push_attribute(("name", "FFVideoFormatRateUndefined"));
            writer.write_event(Event::Empty(audio_fmt))?;
        }
    }

    // Asset resources — deduplicated by source path so clips from the same
    // media file share a single <asset> element.
    let mut written_asset_sources: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    // Write placeholder assets for sourceless clips (Title, Adjustment, etc.)
    // so the parser can find them via their asset-clip ref attribute.
    {
        let mut written_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        for track in project.video_tracks().chain(project.audio_tracks()) {
            for clip in &track.clips {
                if !matches!(
                    clip.kind,
                    crate::model::clip::ClipKind::Title | crate::model::clip::ClipKind::Adjustment
                ) {
                    continue;
                }
                let asset_id = asset_id_by_source
                    .get(
                        clip.fcpxml_original_source_path
                            .as_deref()
                            .unwrap_or(&clip.source_path),
                    )
                    .cloned()
                    .unwrap_or_else(|| format!("a_{}", sanitize_id(&clip.id)));
                if !written_ids.insert(asset_id.clone()) {
                    continue;
                }
                let mut asset_elem = BytesStart::new("asset");
                asset_elem.push_attribute(("id", asset_id.as_str()));
                asset_elem.push_attribute(("src", ""));
                asset_elem.push_attribute(("name", clip.label.as_str()));
                asset_elem.push_attribute(("hasVideo", "1"));
                asset_elem.push_attribute((
                    "duration",
                    format!(
                        "{}/{}s",
                        clip.source_out.saturating_sub(clip.source_in),
                        1_000_000_000u64
                    )
                    .as_str(),
                ));
                asset_elem.push_attribute(("format", "r1"));
                writer.write_event(Event::Empty(asset_elem))?;
            }
        }
    }
    for track in project.video_tracks().chain(project.audio_tracks()) {
        for clip in &track.clips {
            // Title/Adjustment clips handled above with placeholder assets.
            // Compound/Multicam clips have no source media — skip.
            if clip.source_path.is_empty()
                || clip.kind == crate::model::clip::ClipKind::Title
                || clip.kind == crate::model::clip::ClipKind::Adjustment
                || clip.kind == crate::model::clip::ClipKind::Compound
                || clip.kind == crate::model::clip::ClipKind::Multicam
            {
                continue;
            }
            let export_source_path = clip
                .fcpxml_original_source_path
                .as_deref()
                .unwrap_or(&clip.source_path);
            // Skip if we already wrote an asset for this source path.
            if !written_asset_sources.insert(export_source_path.to_string()) {
                continue;
            }
            let asset_id = asset_id_by_source
                .get(export_source_path)
                .cloned()
                .unwrap_or_else(|| format!("a_{}", sanitize_id(&clip.id)));
            let uri = fcpxml_media_src_uri(export_source_path);

            // Use export context for accurate timecode and format, with fallbacks.
            let media_info = export_ctx.and_then(|ctx| ctx.media.get(export_source_path));
            let is_audio_only = media_info.map(|m| m.is_audio_only).unwrap_or(false);
            let asset_fps = media_info.map(|m| &m.fps).unwrap_or(&project.frame_rate);
            let asset_format_id = media_info.map(|m| m.format_id.as_str()).unwrap_or("r1");
            let asset_start = if is_audio_only {
                // Audio-only: use BWF time_reference if available.
                media_info
                    .and_then(|m| m.timecode_ns)
                    .map(|ns| ns_to_fcpxml_time(ns, asset_fps))
                    .unwrap_or_else(|| "0s".to_string())
            } else {
                media_info
                    .and_then(|m| m.timecode_ns)
                    .map(|ns| ns_to_fcpxml_time(ns, asset_fps))
                    .unwrap_or_else(|| "0s".to_string())
            };

            // Determine hasVideo from probe: if media has non-zero resolution it has video.
            let media_has_video = media_info
                .map(|m| m.width > 0 && m.height > 0)
                .unwrap_or(clip.kind != crate::model::clip::ClipKind::Audio);
            let has_audio = "1";

            let mut asset = BytesStart::new("asset");
            asset.push_attribute(("id", asset_id.as_str()));
            asset.push_attribute(("name", clip.label.as_str()));
            asset.push_attribute(("start", asset_start.as_str()));
            // For audio-only assets, include probed duration (FCP requires it
            // since FFVideoFormatRateUndefined has no frame grid).
            // For video assets, omit duration — FCP will probe the media file.
            // Declaring a duration that exceeds the real video (even by a
            // fraction of a frame) triggers a setClippedRange: assertion.
            let audio_dur_str;
            if is_audio_only {
                if let Some(dur_ns) = media_info.and_then(|m| m.duration_ns) {
                    audio_dur_str = ns_to_fcpxml_time(dur_ns, asset_fps);
                    asset.push_attribute(("duration", audio_dur_str.as_str()));
                }
            }
            if media_has_video {
                asset.push_attribute(("format", asset_format_id));
            }
            if media_has_video {
                asset.push_attribute(("hasVideo", "1"));
            }
            asset.push_attribute(("hasAudio", has_audio));
            if is_audio_only {
                asset.push_attribute(("audioSources", "1"));
                asset.push_attribute(("audioChannels", "2"));
                asset.push_attribute(("audioRate", "48000"));
            }
            if !strip_unknown_fields {
                for (k, v) in &clip.fcpxml_unknown_asset_attrs {
                    if !is_writer_managed_asset_attr(k) {
                        asset.push_attribute((k.as_str(), v.as_str()));
                    }
                }
            }
            writer.write_event(Event::Start(asset))?;

            let mut media_rep = BytesStart::new("media-rep");
            media_rep.push_attribute(("kind", media_rep_kind_for_path(export_source_path)));
            media_rep.push_attribute(("src", uri.as_str()));
            writer.write_event(Event::Empty(media_rep))?;
            if !strip_unknown_fields {
                for fragment in &clip.fcpxml_unknown_asset_children {
                    writer.get_mut().write_all(fragment.as_bytes())?;
                }
            }

            writer.write_event(Event::End(BytesEnd::new("asset")))?;
        }
    }
    if !strip_unknown_fields {
        for fragment in &project.fcpxml_unknown_resources.children {
            writer.get_mut().write_all(fragment.as_bytes())?;
        }
    }

    // Emit <effect> for FCP Color Adjustments if any clip uses color grading.
    if options.strict_dtd {
        let needs_color_effect = project
            .video_tracks()
            .chain(project.audio_tracks())
            .flat_map(|t| t.clips.iter())
            .any(clip_has_color_adjustments);
        if needs_color_effect {
            let mut effect = BytesStart::new("effect");
            effect.push_attribute(("id", COLOR_ADJUSTMENTS_EFFECT_ID));
            effect.push_attribute(("name", "Color Adjustments"));
            effect.push_attribute(("uid", FCP_COLOR_ADJUSTMENTS_UID));
            writer.write_event(Event::Empty(effect))?;
        }
    }

    writer.write_event(Event::End(BytesEnd::new("resources")))?;
    Ok(())
}

fn known_fcpxml_format_name(project: &Project) -> Option<&'static str> {
    known_fcpxml_format_name_for(
        project.width,
        project.height,
        project.frame_rate.numerator,
        project.frame_rate.denominator,
    )
}

/// Convert linear volume (0.0–~4.0) to dB string for FCPXML.
fn linear_volume_to_fcpxml_db(linear: f64) -> String {
    if linear <= 0.0 {
        return "-96dB".to_string();
    }
    let db = 20.0 * linear.log10();
    if db <= -96.0 {
        "-96dB".to_string()
    } else {
        format!("{:.1}dB", db)
    }
}

fn is_time_map_fragment(fragment: &str) -> bool {
    fragment.contains("<timeMap")
}

fn preserved_unknown_time_map_fragment(clip: &crate::model::clip::Clip) -> Option<&str> {
    clip.fcpxml_unknown_children
        .iter()
        .find(|fragment| is_time_map_fragment(fragment))
        .map(|fragment| fragment.as_str())
}

#[derive(Clone, Copy)]
struct TimeMapPoint {
    time_ns: u64,
    value_ns: u64,
    interp: crate::model::clip::KeyframeInterpolation,
}

fn time_map_interp_to_fcpxml(interp: crate::model::clip::KeyframeInterpolation) -> &'static str {
    match interp {
        crate::model::clip::KeyframeInterpolation::Linear => "linear",
        crate::model::clip::KeyframeInterpolation::EaseIn
        | crate::model::clip::KeyframeInterpolation::EaseOut
        | crate::model::clip::KeyframeInterpolation::EaseInOut => "smooth2",
    }
}

fn write_native_time_map(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    clip: &crate::model::clip::Clip,
    fps: &crate::model::project::FrameRate,
) -> Result<()> {
    let clip_duration_ns = clip.duration();
    if clip_duration_ns == 0 {
        return Ok(());
    }

    let Some(points) = native_time_map_points_for_clip(clip, clip_duration_ns) else {
        return Ok(());
    };
    let mut time_map = BytesStart::new("timeMap");
    time_map.push_attribute(("preservesPitch", "1"));
    writer.write_event(Event::Start(time_map))?;

    for point in points {
        let mut pt = BytesStart::new("timept");
        pt.push_attribute(("time", ns_to_fcpxml_time(point.time_ns, fps).as_str()));
        pt.push_attribute(("value", ns_to_fcpxml_time(point.value_ns, fps).as_str()));
        pt.push_attribute(("interp", time_map_interp_to_fcpxml(point.interp)));
        writer.write_event(Event::Empty(pt))?;
    }

    writer.write_event(Event::End(BytesEnd::new("timeMap")))?;
    Ok(())
}

fn native_time_map_points_for_clip(
    clip: &crate::model::clip::Clip,
    clip_duration_ns: u64,
) -> Option<Vec<TimeMapPoint>> {
    let source_span_ns = clip.source_out.saturating_sub(clip.source_in);
    if source_span_ns == 0 {
        return None;
    }
    if clip.is_freeze_frame() {
        let source_abs_ns = clip.freeze_frame_source_time_ns().unwrap_or(clip.source_in);
        let source_value_ns = source_abs_ns.saturating_sub(clip.source_in);
        return Some(vec![
            TimeMapPoint {
                time_ns: 0,
                value_ns: source_value_ns,
                interp: crate::model::clip::KeyframeInterpolation::Linear,
            },
            TimeMapPoint {
                time_ns: clip_duration_ns,
                value_ns: source_value_ns,
                interp: crate::model::clip::KeyframeInterpolation::Linear,
            },
        ]);
    }

    if !clip.speed_keyframes.is_empty() {
        return native_time_map_points_for_speed_keyframes(clip, clip_duration_ns, source_span_ns);
    }

    let has_constant_retime = clip.reverse || (clip.speed - 1.0).abs() > f64::EPSILON;
    if !has_constant_retime {
        return None;
    }
    let (start_value_ns, end_value_ns) = if clip.reverse {
        (source_span_ns, 0u64)
    } else {
        (0u64, source_span_ns)
    };
    Some(vec![
        TimeMapPoint {
            time_ns: 0,
            value_ns: start_value_ns,
            interp: crate::model::clip::KeyframeInterpolation::Linear,
        },
        TimeMapPoint {
            time_ns: clip_duration_ns,
            value_ns: end_value_ns,
            interp: crate::model::clip::KeyframeInterpolation::Linear,
        },
    ])
}

fn native_time_map_points_for_speed_keyframes(
    clip: &crate::model::clip::Clip,
    clip_duration_ns: u64,
    source_span_ns: u64,
) -> Option<Vec<TimeMapPoint>> {
    let mut times = vec![0u64, clip_duration_ns];
    for keyframe in &clip.speed_keyframes {
        let t = keyframe.time_ns.min(clip_duration_ns);
        if !times.contains(&t) {
            times.push(t);
        }
    }
    times.sort_unstable();
    if times.len() < 2 {
        return None;
    }

    let mut points = Vec::with_capacity(times.len());
    for &time_ns in &times {
        let forward_distance_ns =
            integrate_speed_distance_to_time_ns(clip, time_ns).clamp(0.0, source_span_ns as f64);
        let value_ns = if clip.reverse {
            source_span_ns.saturating_sub(forward_distance_ns as u64)
        } else {
            (forward_distance_ns as u64).min(source_span_ns)
        };
        let interp = clip
            .speed_keyframes
            .iter()
            .find(|kf| kf.time_ns == time_ns)
            .map(|kf| kf.interpolation)
            .unwrap_or(crate::model::clip::KeyframeInterpolation::Linear);
        points.push(TimeMapPoint {
            time_ns,
            value_ns,
            interp,
        });
    }
    Some(points)
}

fn integrate_speed_distance_to_time_ns(
    clip: &crate::model::clip::Clip,
    local_timeline_ns: u64,
) -> f64 {
    if local_timeline_ns == 0 {
        return 0.0;
    }
    if clip.speed_keyframes.is_empty() {
        return clip.speed.clamp(0.05, 16.0) * local_timeline_ns as f64;
    }
    const MAX_SAMPLES: u64 = 4096;
    const STEP_NS: u64 = 8_333_333; // ~120Hz
    let sample_count = (local_timeline_ns / STEP_NS).max(1).min(MAX_SAMPLES);
    let mut integrated = 0.0f64;
    for i in 0..sample_count {
        let t0 = (u128::from(local_timeline_ns) * u128::from(i) / u128::from(sample_count)) as u64;
        let t1 =
            (u128::from(local_timeline_ns) * u128::from(i + 1) / u128::from(sample_count)) as u64;
        let dt = t1.saturating_sub(t0);
        if dt == 0 {
            continue;
        }
        let mid = t0.saturating_add(dt / 2);
        integrated += clip.speed_at_local_timeline_ns(mid) * dt as f64;
    }
    integrated
}

fn keyframe_curve_attr(kf: &crate::model::clip::NumericKeyframe) -> Option<&'static str> {
    if kf.bezier_controls.is_some() {
        Some("smooth")
    } else {
        None
    }
}

/// Write native `<param>/<keyframeAnimation>/<keyframe>` children for transform properties.
/// `source_start_ns` is the FCPXML `start` attribute value in nanoseconds — keyframe times
/// are offset by this amount so they appear in absolute source time as FCP expects.
fn write_transform_keyframe_params(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    clip: &crate::model::clip::Clip,
    project: &Project,
    source_start_ns: u64,
) -> Result<()> {
    let fps = &project.frame_rate;

    // Position keyframes — FCP omits value attr on <param> when keyframes present
    if !clip.position_x_keyframes.is_empty() || !clip.position_y_keyframes.is_empty() {
        let mut param = BytesStart::new("param");
        param.push_attribute(("name", "position"));
        writer.write_event(Event::Start(param))?;

        let kfa = BytesStart::new("keyframeAnimation");
        writer.write_event(Event::Start(kfa))?;

        // Merge position_x and position_y keyframes by time
        let mut time_set: Vec<u64> = Vec::new();
        for kf in &clip.position_x_keyframes {
            if !time_set.contains(&kf.time_ns) {
                time_set.push(kf.time_ns);
            }
        }
        for kf in &clip.position_y_keyframes {
            if !time_set.contains(&kf.time_ns) {
                time_set.push(kf.time_ns);
            }
        }
        time_set.sort();

        for &t in &time_set {
            // Evaluate scale at this time for position conversion
            let scale_at_t = crate::model::clip::Clip::evaluate_keyframed_value(
                &clip.scale_keyframes,
                t,
                clip.scale,
            );
            let ix = crate::model::clip::Clip::evaluate_keyframed_value(
                &clip.position_x_keyframes,
                t,
                clip.position_x,
            );
            let iy = crate::model::clip::Clip::evaluate_keyframed_value(
                &clip.position_y_keyframes,
                t,
                clip.position_y,
            );
            let (fx, fy) =
                internal_position_to_fcpxml(ix, iy, project.width, project.height, scale_at_t);
            let mut kf_elem = BytesStart::new("keyframe");
            // Offset clip-local time back to absolute source time for FCP
            kf_elem.push_attribute(("time", ns_to_fcpxml_time(t + source_start_ns, fps).as_str()));
            kf_elem.push_attribute(("value", format!("{} {}", fx, fy).as_str()));
            let x_kf = clip.position_x_keyframes.iter().find(|kf| kf.time_ns == t);
            let y_kf = clip.position_y_keyframes.iter().find(|kf| kf.time_ns == t);
            let interp = x_kf
                .map(|kf| kf.interpolation)
                .or_else(|| y_kf.map(|kf| kf.interpolation))
                .unwrap_or(crate::model::clip::KeyframeInterpolation::Linear);
            kf_elem.push_attribute(("interp", interp.to_fcpxml()));
            let has_curve = x_kf.and_then(keyframe_curve_attr).is_some()
                || y_kf.and_then(keyframe_curve_attr).is_some();
            if has_curve {
                kf_elem.push_attribute(("curve", "smooth"));
            }
            writer.write_event(Event::Empty(kf_elem))?;
        }

        writer.write_event(Event::End(BytesEnd::new("keyframeAnimation")))?;
        writer.write_event(Event::End(BytesEnd::new("param")))?;
    }

    // Scale keyframes — FCP uses lowercase "scale"
    if !clip.scale_keyframes.is_empty() {
        let mut param = BytesStart::new("param");
        param.push_attribute(("name", "scale"));
        writer.write_event(Event::Start(param))?;

        let kfa = BytesStart::new("keyframeAnimation");
        writer.write_event(Event::Start(kfa))?;

        let mut sorted: Vec<&crate::model::clip::NumericKeyframe> =
            clip.scale_keyframes.iter().collect();
        sorted.sort_by_key(|kf| kf.time_ns);

        for kf in &sorted {
            let mut kf_elem = BytesStart::new("keyframe");
            // Offset clip-local time back to absolute source time for FCP
            kf_elem.push_attribute((
                "time",
                ns_to_fcpxml_time(kf.time_ns + source_start_ns, fps).as_str(),
            ));
            kf_elem.push_attribute(("value", format!("{} {}", kf.value, kf.value).as_str()));
            kf_elem.push_attribute(("interp", kf.interpolation.to_fcpxml()));
            if let Some(curve) = keyframe_curve_attr(kf) {
                kf_elem.push_attribute(("curve", curve));
            }
            writer.write_event(Event::Empty(kf_elem))?;
        }

        writer.write_event(Event::End(BytesEnd::new("keyframeAnimation")))?;
        writer.write_event(Event::End(BytesEnd::new("param")))?;
    }

    // Rotation keyframes
    if !clip.rotate_keyframes.is_empty() {
        let mut param = BytesStart::new("param");
        param.push_attribute(("name", "rotation"));
        writer.write_event(Event::Start(param))?;

        let kfa = BytesStart::new("keyframeAnimation");
        writer.write_event(Event::Start(kfa))?;

        let mut sorted: Vec<&crate::model::clip::NumericKeyframe> =
            clip.rotate_keyframes.iter().collect();
        sorted.sort_by_key(|kf| kf.time_ns);

        for kf in &sorted {
            let mut kf_elem = BytesStart::new("keyframe");
            // Offset clip-local time back to absolute source time for FCP
            kf_elem.push_attribute((
                "time",
                ns_to_fcpxml_time(kf.time_ns + source_start_ns, fps).as_str(),
            ));
            kf_elem.push_attribute(("value", kf.value.to_string().as_str()));
            kf_elem.push_attribute(("interp", kf.interpolation.to_fcpxml()));
            if let Some(curve) = keyframe_curve_attr(kf) {
                kf_elem.push_attribute(("curve", curve));
            }
            writer.write_event(Event::Empty(kf_elem))?;
        }

        writer.write_event(Event::End(BytesEnd::new("keyframeAnimation")))?;
        writer.write_event(Event::End(BytesEnd::new("param")))?;
    }

    Ok(())
}

/// Write native `<param>/<keyframeAnimation>/<keyframe>` children for opacity.
fn write_opacity_keyframe_params(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    clip: &crate::model::clip::Clip,
    fps: &crate::model::project::FrameRate,
    source_start_ns: u64,
) -> Result<()> {
    if clip.opacity_keyframes.is_empty() {
        return Ok(());
    }

    let mut param = BytesStart::new("param");
    param.push_attribute(("name", "amount"));
    param.push_attribute(("value", clip.opacity.to_string().as_str()));
    writer.write_event(Event::Start(param))?;

    let kfa = BytesStart::new("keyframeAnimation");
    writer.write_event(Event::Start(kfa))?;

    let mut sorted: Vec<&crate::model::clip::NumericKeyframe> =
        clip.opacity_keyframes.iter().collect();
    sorted.sort_by_key(|kf| kf.time_ns);

    for kf in &sorted {
        let mut kf_elem = BytesStart::new("keyframe");
        // Offset clip-local time back to absolute source time for FCP
        kf_elem.push_attribute((
            "time",
            ns_to_fcpxml_time(kf.time_ns + source_start_ns, fps).as_str(),
        ));
        kf_elem.push_attribute(("value", kf.value.to_string().as_str()));
        kf_elem.push_attribute(("interp", kf.interpolation.to_fcpxml()));
        if let Some(curve) = keyframe_curve_attr(kf) {
            kf_elem.push_attribute(("curve", curve));
        }
        writer.write_event(Event::Empty(kf_elem))?;
    }

    writer.write_event(Event::End(BytesEnd::new("keyframeAnimation")))?;
    writer.write_event(Event::End(BytesEnd::new("param")))?;

    Ok(())
}

/// Write native `<param>/<keyframeAnimation>/<keyframe>` children for volume (dB).
fn write_volume_keyframe_params(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    clip: &crate::model::clip::Clip,
    fps: &crate::model::project::FrameRate,
    source_start_ns: u64,
    strict: bool,
) -> Result<()> {
    if clip.volume_keyframes.is_empty() {
        return Ok(());
    }

    let mut param = BytesStart::new("param");
    param.push_attribute(("name", "amount"));
    param.push_attribute((
        "value",
        linear_volume_to_fcpxml_db(clip.volume as f64).as_str(),
    ));
    writer.write_event(Event::Start(param))?;

    let kfa = BytesStart::new("keyframeAnimation");
    writer.write_event(Event::Start(kfa))?;

    let mut sorted: Vec<&crate::model::clip::NumericKeyframe> =
        clip.volume_keyframes.iter().collect();
    sorted.sort_by_key(|kf| kf.time_ns);

    for kf in &sorted {
        let mut kf_elem = BytesStart::new("keyframe");
        // Offset clip-local time back to source time for FCP
        kf_elem.push_attribute((
            "time",
            ns_to_fcpxml_time(kf.time_ns + source_start_ns, fps).as_str(),
        ));
        kf_elem.push_attribute(("value", linear_volume_to_fcpxml_db(kf.value).as_str()));
        // FCP ignores interp on volume param keyframes — omit in strict mode.
        if !strict {
            kf_elem.push_attribute(("interp", kf.interpolation.to_fcpxml()));
            if let Some(curve) = keyframe_curve_attr(kf) {
                kf_elem.push_attribute(("curve", curve));
            }
        }
        writer.write_event(Event::Empty(kf_elem))?;
    }

    writer.write_event(Event::End(BytesEnd::new("keyframeAnimation")))?;
    writer.write_event(Event::End(BytesEnd::new("param")))?;

    Ok(())
}

/// Write native `<param>/<keyframeAnimation>/<keyframe>` children for pan.
fn write_pan_keyframe_params(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    clip: &crate::model::clip::Clip,
    fps: &crate::model::project::FrameRate,
    source_start_ns: u64,
    strict: bool,
) -> Result<()> {
    if clip.pan_keyframes.is_empty() {
        return Ok(());
    }

    let mut param = BytesStart::new("param");
    param.push_attribute(("name", "amount"));
    param.push_attribute((
        "value",
        format!("{:.6}", clip.pan.clamp(-1.0, 1.0)).as_str(),
    ));
    writer.write_event(Event::Start(param))?;

    let kfa = BytesStart::new("keyframeAnimation");
    writer.write_event(Event::Start(kfa))?;

    let mut sorted: Vec<&crate::model::clip::NumericKeyframe> = clip.pan_keyframes.iter().collect();
    sorted.sort_by_key(|kf| kf.time_ns);

    for kf in &sorted {
        let mut kf_elem = BytesStart::new("keyframe");
        // Offset clip-local time back to source time for FCP
        kf_elem.push_attribute((
            "time",
            ns_to_fcpxml_time(kf.time_ns + source_start_ns, fps).as_str(),
        ));
        kf_elem.push_attribute((
            "value",
            format!("{:.6}", kf.value.clamp(-1.0, 1.0)).as_str(),
        ));
        // FCP ignores interp on pan param keyframes — omit in strict mode.
        if !strict {
            kf_elem.push_attribute(("interp", kf.interpolation.to_fcpxml()));
            if let Some(curve) = keyframe_curve_attr(kf) {
                kf_elem.push_attribute(("curve", curve));
            }
        }
        writer.write_event(Event::Empty(kf_elem))?;
    }

    writer.write_event(Event::End(BytesEnd::new("keyframeAnimation")))?;
    writer.write_event(Event::End(BytesEnd::new("param")))?;
    Ok(())
}

/// Convert nanoseconds to FCPXML rational time string (e.g. "48048/24000s").
///
/// FCPXML encodes time as `numerator/denominator s` where the result is seconds.
/// For NTSC rates (fps_den=1001): 119 frames at 23.976fps → 119×1001/24000 s = 119119/24000s.
/// For integer rates (fps_den=1):  48 frames at 24fps      → 48×1/24 s       = 48/24s.
fn ns_to_fcpxml_time(ns: u64, fps: &crate::model::project::FrameRate) -> String {
    let timebase = fps.numerator as u64;
    let denom = fps.denominator as u64;
    // frames = ns * fps_num / (fps_den * 1_000_000_000), rounded to nearest frame
    // to avoid off-by-one truncation at NTSC rates (1001/24000 = 41708333.33… ns).
    let frames = (ns * timebase + denom * 500_000_000) / (denom * 1_000_000_000);
    // FCPXML numerator = frames × fps_den so that numerator/timebase gives seconds
    let numerator = frames * denom;
    format!("{numerator}/{timebase}s")
}

fn sanitize_id(id: &str) -> String {
    id.replace('-', "_")
}

fn media_rep_kind_for_path(source_path: &str) -> &'static str {
    if source_path.contains("UltimateSlice.cache") && source_path.contains(".proxy_") {
        "proxy-media"
    } else {
        "original-media"
    }
}

fn is_writer_managed_fcpxml_attr(key: &str) -> bool {
    matches!(key, "version" | "xmlns:us")
}

fn is_writer_managed_resources_attr(_key: &str) -> bool {
    false
}

fn is_writer_managed_format_attr(key: &str) -> bool {
    matches!(key, "id" | "name" | "frameDuration" | "width" | "height")
}

fn is_writer_managed_library_attr(_key: &str) -> bool {
    false
}

fn is_writer_managed_event_attr(_key: &str) -> bool {
    matches!(_key, "us:bins" | "us:media-bins" | "us:smart-collections")
}

fn is_writer_managed_project_attr(key: &str) -> bool {
    matches!(key, "name")
}

fn is_writer_managed_sequence_attr(key: &str) -> bool {
    matches!(
        key,
        "duration" | "format" | "tcFormat" | "audioLayout" | "audioRate"
    )
}

fn is_writer_managed_spine_attr(_key: &str) -> bool {
    false
}

fn is_writer_managed_asset_attr(key: &str) -> bool {
    matches!(
        key,
        "id" | "src" | "name" | "start" | "duration" | "format" | "hasVideo" | "hasAudio"
    )
}

fn is_writer_managed_asset_clip_attr(key: &str) -> bool {
    matches!(
        key,
        "ref"
            | "offset"
            | "duration"
            | "start"
            | "name"
            | "us:track-idx"
            | "us:track-kind"
            | "us:track-name"
            | "us:track-muted"
            | "us:track-locked"
            | "us:track-soloed"
            | "us:track-audio-role"
            | "us:track-duck"
            | "us:track-duck-amount-db"
            | "us:track-height"
            | "us:color-label"
            | "us:brightness"
            | "us:contrast"
            | "us:saturation"
            | "us:temperature"
            | "us:tint"
            | "us:brightness-keyframes"
            | "us:contrast-keyframes"
            | "us:saturation-keyframes"
            | "us:temperature-keyframes"
            | "us:tint-keyframes"
            | "us:denoise"
            | "us:sharpness"
            | "us:blur"
            | "us:blur-keyframes"
            | "us:frei0r-effects"
            | "us:volume"
            | "us:volume-keyframes"
            | "us:pan"
            | "us:pan-keyframes"
            | "us:rotate-keyframes"
            | "us:crop-left"
            | "us:crop-right"
            | "us:crop-top"
            | "us:crop-bottom"
            | "us:crop-left-keyframes"
            | "us:crop-right-keyframes"
            | "us:crop-top-keyframes"
            | "us:crop-bottom-keyframes"
            | "us:rotate"
            | "us:flip-h"
            | "us:flip-v"
            | "us:anamorphic-desqueeze"
            | "us:scale"
            | "us:scale-keyframes"
            | "us:opacity"
            | "us:opacity-keyframes"
            | "us:position-x"
            | "us:position-x-keyframes"
            | "us:position-y"
            | "us:position-y-keyframes"
            | "us:title-text"
            | "us:title-font"
            | "us:title-color"
            | "us:title-x"
            | "us:title-y"
            | "us:speed"
            | "us:speed-keyframes"
            | "us:slow-motion-interp"
            | "us:reverse"
            | "us:freeze-frame"
            | "us:freeze-source-ns"
            | "us:freeze-hold-duration-ns"
            | "us:group-id"
            | "us:link-group-id"
            | "us:source-timecode-base-ns"
            | "us:shadows"
            | "us:midtones"
            | "us:highlights"
            | "us:chroma-key-enabled"
            | "us:chroma-key-color"
            | "us:chroma-key-tolerance"
            | "us:chroma-key-softness"
            | "us:lut-paths"
            | "us:lut-path"
            | "us:transition-after"
            | "us:transition-after-ns"
            | "us:blend-mode"
            | "us:exposure"
            | "us:black-point"
            | "us:highlights-warmth"
            | "us:highlights-tint"
            | "us:midtones-warmth"
            | "us:midtones-tint"
            | "us:shadows-warmth"
            | "us:shadows-tint"
            | "us:bg-removal-enabled"
            | "us:bg-removal-threshold"
            | "us:title-template"
            | "us:title-outline-color"
            | "us:title-outline-width"
            | "us:title-shadow"
            | "us:title-shadow-color"
            | "us:title-shadow-offset-x"
            | "us:title-shadow-offset-y"
            | "us:title-bg-box"
            | "us:title-bg-box-color"
            | "us:title-bg-box-padding"
            | "us:title-clip-bg-color"
            | "us:title-secondary-text"
            | "us:clip-kind"
            | "us:eq-bands"
            | "us:eq-low-gain-keyframes"
            | "us:eq-mid-gain-keyframes"
            | "us:eq-high-gain-keyframes"
            | "us:pitch-shift-semitones"
            | "us:pitch-preserve"
            | "us:audio-channel-mode"
            | "us:ladspa-effects"
            | "us:masks"
            | "us:measured-loudness-lufs"
            | "us:subtitle-segments"
            | "us:subtitles-language"
            | "us:subtitle-font"
            | "us:subtitle-color"
            | "us:subtitle-outline-color"
            | "us:subtitle-outline-width"
            | "us:subtitle-bg-box"
            | "us:subtitle-bg-box-color"
            | "us:subtitle-highlight-mode"
            | "us:subtitle-highlight-color"
            | "us:subtitle-word-window-secs"
            | "us:subtitle-position-y"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fcpxml::parser::parse_fcpxml;
    use crate::model::clip::{Clip, ClipKind};
    use crate::model::project::Project;
    use crate::model::track::Track;
    use quick_xml::events::Event;
    use quick_xml::Reader;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn has_asset_src_attr(xml: &str) -> bool {
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                    if e.local_name().as_ref() == b"asset" {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"src" {
                                return true;
                            }
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(_) => break,
            }
            buf.clear();
        }
        false
    }

    fn media_rep_src_values(xml: &str) -> Vec<String> {
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        let mut srcs = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                    if e.local_name().as_ref() == b"media-rep" {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"src" {
                                srcs.push(String::from_utf8_lossy(attr.value.as_ref()).to_string());
                            }
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(_) => break,
            }
            buf.clear();
        }
        srcs
    }

    fn asset_clip_lane_values(xml: &str) -> Vec<Option<String>> {
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        let mut lanes = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                    if e.local_name().as_ref() == b"asset-clip" {
                        let mut lane = None;
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"lane" {
                                lane =
                                    Some(String::from_utf8_lossy(attr.value.as_ref()).to_string());
                                break;
                            }
                        }
                        lanes.push(lane);
                    }
                }
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(_) => break,
            }
            buf.clear();
        }
        lanes
    }

    fn unique_test_dir(prefix: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("ultimateslice-{prefix}-{nanos}"))
    }

    #[test]
    fn test_write_fcpxml_emits_media_rep_with_original_media_kind() {
        let mut project = Project::new("Test");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/source.mov", 2_000_000_000, 0, ClipKind::Video);
        clip.position_x = 0.25;
        clip.position_y = -0.5;
        clip.scale = 1.5;
        clip.rotate = 90;
        clip.opacity = 0.75;
        clip.crop_left = 10;
        clip.crop_right = 20;
        clip.crop_top = 30;
        clip.crop_bottom = 40;
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml(&project).expect("write should succeed");
        assert!(xml.contains("<media-rep kind=\"original-media\""));
        assert!(xml.contains("src=\"file:///tmp/source.mov\""));
        assert!(xml.contains("<adjust-transform"));
        assert!(xml.contains("position=\"-11.111111"));
        assert!(xml.contains(" -12.5\""));
        assert!(xml.contains("scale=\"1.5 1.5\""));
        assert!(xml.contains("rotation=\"90\""));
        assert!(xml.contains("<adjust-compositing opacity=\"0.75\""));
        assert!(xml.contains("<adjust-crop left=\"10\" right=\"20\" top=\"30\" bottom=\"40\""));
    }

    #[test]
    fn test_fcpxml_media_src_uri_percent_encodes_special_chars() {
        let uri = fcpxml_media_src_uri("/tmp/Final Cut/clip #1%.mov");
        assert_eq!(uri, "file:///tmp/Final%20Cut/clip%20%231%25.mov");
    }

    #[test]
    fn test_fcpxml_media_src_uri_reencodes_existing_file_uri_path() {
        let uri = fcpxml_media_src_uri("file:///tmp/Final%20Cut/clip%20A.mov");
        assert_eq!(uri, "file:///tmp/Final%20Cut/clip%20A.mov");
    }

    #[test]
    fn test_write_fcpxml_emits_native_transition_for_adjacent_clips() {
        let mut project = Project::new("TransitionWrite");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut a = Clip::new("/tmp/a.mov", 2_000_000_000, 0, ClipKind::Video);
        a.transition_after = "cross_dissolve".to_string();
        a.transition_after_ns = 1_000_000_000;
        track.add_clip(a);
        track.add_clip(Clip::new(
            "/tmp/b.mov",
            2_000_000_000,
            1_000_000_000,
            ClipKind::Video,
        ));
        project.tracks.push(track);

        let xml = write_fcpxml(&project).expect("write should succeed");
        assert!(xml.contains("<transition name=\"Cross Dissolve\""));
        assert!(xml.contains("duration=\"24/24s\"") || xml.contains("duration=\"1/1s\""));

        let parsed = parse_fcpxml(&xml).expect("parse written xml");
        let first = &parsed.video_tracks().next().expect("video track").clips[0];
        assert_eq!(first.transition_after, "cross_dissolve");
        assert_eq!(first.transition_after_ns, 1_000_000_000);
    }

    #[test]
    fn test_write_fcpxml_marks_proxy_media_rep_kind() {
        let mut project = Project::new("Test");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        track.add_clip(Clip::new(
            "/tmp/UltimateSlice.cache/clip.proxy_half.mp4",
            2_000_000_000,
            0,
            ClipKind::Video,
        ));
        project.tracks.push(track);

        let xml = write_fcpxml(&project).expect("write should succeed");
        assert!(xml.contains("<media-rep kind=\"proxy-media\""));
    }

    #[test]
    fn test_write_fcpxml_emits_link_group_attr() {
        let mut project = Project::new("Test");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/source.mov", 2_000_000_000, 0, ClipKind::Video);
        clip.link_group_id = Some("link-1".to_string());
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml(&project).expect("write should succeed");
        assert!(xml.contains("us:link-group-id=\"link-1\""));
    }

    #[test]
    fn test_write_fcpxml_emits_source_timecode_base_attr() {
        let mut project = Project::new("Test");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/source.mov", 2_000_000_000, 0, ClipKind::Video);
        clip.source_in = 1_000_000_000;
        clip.source_timecode_base_ns = Some(4_000_000_000);
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml(&project).expect("write should succeed");
        assert!(xml.contains("us:source-timecode-base-ns=\"4000000000\""));
        assert!(xml.contains("start=\"120/24s\""));
    }

    #[test]
    fn test_write_fcpxml_emits_freeze_frame_attrs() {
        let mut project = Project::new("Test");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/source.mov", 2_000_000_000, 0, ClipKind::Video);
        clip.freeze_frame = true;
        clip.freeze_frame_source_ns = Some(1_250_000_000);
        clip.freeze_frame_hold_duration_ns = Some(3_000_000_000);
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml(&project).expect("write should succeed");
        assert!(xml.contains("us:freeze-frame=\"true\""));
        assert!(xml.contains("us:freeze-source-ns=\"1250000000\""));
        assert!(xml.contains("us:freeze-hold-duration-ns=\"3000000000\""));
    }

    #[test]
    fn test_write_fcpxml_emits_native_time_map_for_constant_speed() {
        let mut project = Project::new("TimeMapSpeed");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/source.mov", 2_000_000_000, 0, ClipKind::Video);
        clip.speed = 2.0;
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml(&project).expect("write should succeed");
        assert!(xml.contains("<timeMap preservesPitch=\"1\">"));
        assert!(xml.contains("time=\"24/24s\""));
        assert!(xml.contains("value=\"48/24s\""));
    }

    #[test]
    fn test_write_fcpxml_emits_native_time_map_for_reverse() {
        let mut project = Project::new("TimeMapReverse");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/source.mov", 2_000_000_000, 0, ClipKind::Video);
        clip.reverse = true;
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml(&project).expect("write should succeed");
        assert!(xml.contains("<timeMap preservesPitch=\"1\">"));
        assert!(xml.contains("time=\"48/24s\""));
        assert!(xml.contains("value=\"48/24s\""));
        assert!(xml.contains("value=\"0/24s\""));
    }

    #[test]
    fn test_write_fcpxml_avoids_duplicate_timemap_when_preserving_unknown_timemap() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14" xmlns:us="urn:ultimateslice">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///tmp/clip.mov" name="clip" duration="240/24s"/>
  </resources>
  <library>
    <event>
      <project name="DupTimeMapGuard">
        <sequence duration="240/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0s" start="0s" duration="120/24s" us:speed="3.0">
              <timeMap>
                <timept time="0s" value="0s" interp="linear"/>
                <timept time="120/24s" value="240/24s" interp="linear"/>
              </timeMap>
            </asset-clip>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let mut project = parse_fcpxml(xml).expect("parse should succeed");
        project.dirty = true;
        let written = write_fcpxml(&project).expect("write should succeed");
        assert_eq!(written.match_indices("<timeMap").count(), 1);
    }

    #[test]
    fn test_write_fcpxml_does_not_duplicate_lut_paths_attribute() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14" xmlns:us="urn:ultimateslice">
    <resources>
        <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
        <asset id="a1" src="file:///tmp/clip.mov" name="clip" duration="240/24s"/>
    </resources>
    <library>
        <event>
            <project name="LutDupGuard">
                <sequence duration="240/24s" format="r1">
                    <spine>
                        <asset-clip
                            ref="a1"
                            offset="0s"
                            start="0s"
                            duration="120/24s"
                            us:lut-paths="[&quot;/tmp/look.cube&quot;]"
                            us:lut-path="/tmp/look.cube"/>
                    </spine>
                </sequence>
            </project>
        </event>
    </library>
</fcpxml>"#;

        let mut project = parse_fcpxml(xml).expect("parse should succeed");
        project.dirty = true;
        let written = write_fcpxml(&project).expect("write should succeed");
        assert_eq!(written.match_indices("us:lut-paths=").count(), 1);
    }

    #[test]
    fn test_write_fcpxml_strict_preserves_imported_unknown_timemap_fragment() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14" xmlns:us="urn:ultimateslice">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///tmp/clip.mov" name="clip" duration="240/24s"/>
  </resources>
  <library>
    <event>
      <project name="StrictTimeMapPreserve">
        <sequence duration="240/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0s" start="0s" duration="120/24s" us:speed="3.0">
              <timeMap>
                <timept time="0s" value="0s" interp="linear"/>
                <timept time="48/24s" value="96/24s" interp="linear"/>
                <timept time="96/24s" value="24/24s" interp="linear"/>
              </timeMap>
            </asset-clip>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let mut project = parse_fcpxml(xml).expect("parse should succeed");
        project.dirty = true;
        let written = write_fcpxml_strict(&project).expect("strict write should succeed");
        assert!(!written.contains("xmlns:us="));
        assert_eq!(written.match_indices("<timeMap").count(), 1);
        assert!(written.contains("value=\"96/24s\""));
        let t_idx = written
            .find("<timeMap")
            .expect("timeMap should be emitted in strict output");
        let transform_idx = written
            .find("<adjust-transform")
            .expect("transform should be present");
        assert!(
            t_idx < transform_idx,
            "timeMap must appear before intrinsic params per DTD timing order"
        );
    }

    #[test]
    fn test_write_fcpxml_emits_native_time_map_for_speed_keyframes() {
        let mut project = Project::new("TimeMapKeyframed");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/source.mov", 4_000_000_000, 0, ClipKind::Video);
        clip.speed = 1.0;
        clip.speed_keyframes = vec![
            crate::model::clip::NumericKeyframe {
                time_ns: 0,
                value: 1.0,
                interpolation: crate::model::clip::KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            crate::model::clip::NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 2.0,
                interpolation: crate::model::clip::KeyframeInterpolation::EaseInOut,
                bezier_controls: None,
            },
        ];
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml(&project).expect("write should succeed");
        assert!(xml.contains("<timeMap preservesPitch=\"1\">"));
        assert!(
            xml.matches("<timept ").count() >= 3,
            "expected multi-point timeMap output"
        );
    }

    #[test]
    fn test_write_fcpxml_emits_smooth2_interp_for_eased_speed_keyframes() {
        let mut project = Project::new("TimeMapSmooth2Write");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/source.mov", 4_000_000_000, 0, ClipKind::Video);
        clip.speed = 1.0;
        clip.speed_keyframes = vec![crate::model::clip::NumericKeyframe {
            time_ns: 1_000_000_000,
            value: 2.0,
            interpolation: crate::model::clip::KeyframeInterpolation::EaseInOut,
            bezier_controls: None,
        }];
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml(&project).expect("write should succeed");
        assert!(xml.contains("interp=\"smooth2\""));
    }

    #[test]
    fn test_write_fcpxml_keyframed_timemap_roundtrip_preserves_keyframes() {
        let mut project = Project::new("TimeMapRoundtrip");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/source.mov", 4_000_000_000, 0, ClipKind::Video);
        clip.speed = 1.0;
        clip.speed_keyframes = vec![
            crate::model::clip::NumericKeyframe {
                time_ns: 0,
                value: 1.0,
                interpolation: crate::model::clip::KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            crate::model::clip::NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 2.0,
                interpolation: crate::model::clip::KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml(&project).expect("write should succeed");
        let parsed = parse_fcpxml(&xml).expect("parse should succeed");
        let parsed_clip = &parsed.video_tracks().next().expect("video").clips[0];
        assert!(
            !parsed_clip.speed_keyframes.is_empty(),
            "expected native timeMap to parse back into speed keyframes"
        );
    }

    #[test]
    fn test_write_fcpxml_preserves_original_document_for_clean_import() {
        let original = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14" customRoot="keep-me">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" name="clip" hasVideo="1" hasAudio="1" duration="24/24s" customAsset="yes">
      <media-rep kind="original-media" src="file:///tmp/clip.mov" customRep="keep"/>
      <md key="com.example.keep" value="1"/>
    </asset>
  </resources>
  <library>
    <event name="MainEvent">
      <project name="MainProject" customProject="keep">
        <sequence format="r1" duration="24/24s" customSequence="keep">
          <spine>
            <asset-clip ref="a1" offset="0s" start="0s" duration="24/24s" customClip="keep"/>
          </spine>
        </sequence>
      </project>
      <project name="UnselectedProject">
        <sequence format="r1" duration="24/24s">
          <spine>
            <asset-clip ref="a1" offset="0s" start="0s" duration="24/24s"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(original).expect("parse should succeed");
        assert!(!project.dirty);
        let written = write_fcpxml(&project).expect("write should succeed");
        assert_eq!(written, original);
    }

    #[test]
    fn test_write_fcpxml_dirty_import_regenerates_document() {
        let original = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///tmp/clip.mov" name="clip" duration="24/24s"/>
  </resources>
  <library>
    <event>
      <project name="MainProject">
        <sequence format="r1" duration="24/24s">
          <spine>
            <asset-clip ref="a1" offset="0s" start="0s" duration="24/24s" customClip="keep"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let mut project = parse_fcpxml(original).expect("parse should succeed");
        project.dirty = true;
        let written = write_fcpxml(&project).expect("write should succeed");
        assert_ne!(written, original);
        assert!(written.contains("<fcpxml version=\"1.14\""));
    }

    #[test]
    fn test_write_fcpxml_clean_project_without_source_fcpxml_generates_xml() {
        let mut project = Project::new("NoSource");
        project.source_fcpxml = None;
        project.dirty = false;

        let xml = write_fcpxml(&project).expect("write should succeed");
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(xml.contains("<!DOCTYPE fcpxml>"));
        assert!(xml.contains("<fcpxml version=\"1.14\""));
        assert!(xml.contains("xmlns:us=\"urn:ultimateslice\""));
    }

    #[test]
    fn test_use_strict_fcpxml_for_path_extension_detection() {
        assert!(use_strict_fcpxml_for_path(std::path::Path::new(
            "/tmp/test.fcpxml"
        )));
        assert!(use_strict_fcpxml_for_path(std::path::Path::new(
            "/tmp/test.FCPXML"
        )));
        assert!(!use_strict_fcpxml_for_path(std::path::Path::new(
            "/tmp/test.uspxml"
        )));
        assert!(!use_strict_fcpxml_for_path(std::path::Path::new(
            "/tmp/test.xml"
        )));
    }

    #[test]
    fn test_write_fcpxml_for_path_routes_by_extension() {
        let project = Project::new("RouteByExtension");
        let strict = write_fcpxml_for_path(&project, std::path::Path::new("/tmp/test.fcpxml"))
            .expect("strict extension write should succeed");
        let rich = write_fcpxml_for_path(&project, std::path::Path::new("/tmp/test.uspxml"))
            .expect("uspxml extension write should succeed");
        assert!(!strict.contains("xmlns:us=\"urn:ultimateslice\""));
        assert!(rich.contains("xmlns:us=\"urn:ultimateslice\""));
    }

    #[test]
    fn test_write_fcpxml_strict_emits_lane_mapping_for_multitrack() {
        let mut project = Project::new("StrictLanes");
        project.tracks.clear();

        let mut video_1 = Track::new_video("Video 1");
        let mut v1_clip = Clip::new("/tmp/v1.mov", 1_000_000_000, 0, ClipKind::Video);
        // Give primary clip a source timecode base so we can verify connected
        // clip offset is expressed in the parent's source time space.
        v1_clip.source_timecode_base_ns = Some(10_000_000_000); // 10s timecode base
        video_1.add_clip(v1_clip);
        let mut video_2 = Track::new_video("Video 2");
        let mut v2_clip = Clip::new("/tmp/v2.mov", 1_000_000_000, 0, ClipKind::Video);
        v2_clip.timeline_start = 500_000_000; // 0.5s into timeline
        video_2.add_clip(v2_clip);
        let mut audio_1 = Track::new_audio("Audio 1");
        audio_1.add_clip(Clip::new("/tmp/a1.wav", 1_000_000_000, 0, ClipKind::Audio));

        project.tracks.push(video_1);
        project.tracks.push(video_2);
        project.tracks.push(audio_1);

        let xml = write_fcpxml_strict(&project).expect("strict write should succeed");
        let lanes = asset_clip_lane_values(&xml);
        assert_eq!(
            lanes,
            vec![None, Some("1".to_string()), Some("-1".to_string())]
        );

        // Verify connected clips are nested inside the primary clip, not flat
        // siblings in the <spine>.
        let spine_start = xml.find("<spine>").expect("spine start");
        let spine_end = xml.find("</spine>").expect("spine end");
        let spine_xml = &xml[spine_start..spine_end];

        // The spine should contain exactly one direct asset-clip child (the primary).
        // Connected clips (lane="1", lane="-1") must be nested inside it.
        let primary_start = spine_xml.find("<asset-clip ").expect("primary clip");
        let primary_clip_xml = &spine_xml[primary_start..];
        // Check just the opening tag of the primary clip (up to first '>').
        let primary_tag_end = primary_clip_xml.find('>').expect("primary tag end");
        let primary_tag = &primary_clip_xml[..primary_tag_end];
        assert!(
            !primary_tag.contains("lane="),
            "primary clip opening tag should not have a lane attribute"
        );
        // Find the LAST </asset-clip> in the spine — this is the primary's
        // closing tag. Everything between the primary's opening and this closing
        // tag includes the nested connected clips.
        let last_close = primary_clip_xml.rfind("</asset-clip>").expect("last close");
        let inner = &primary_clip_xml[..last_close];
        assert!(
            inner.contains("lane=\"1\""),
            "video overlay (lane=1) should be nested inside primary clip"
        );
        assert!(
            inner.contains("lane=\"-1\""),
            "audio (lane=-1) should be nested inside primary clip"
        );
        // Audio-only clip should not have video intrinsic params
        let audio_start = inner.find("lane=\"-1\"").expect("audio lane");
        let audio_clip_xml = &inner[audio_start..];
        assert!(
            !audio_clip_xml.contains("<adjust-crop"),
            "audio clip should not have adjust-crop"
        );
        assert!(
            !audio_clip_xml.contains("<adjust-transform"),
            "audio clip should not have adjust-transform"
        );
        assert!(
            !audio_clip_xml.contains("<adjust-blend"),
            "audio clip should not have adjust-blend"
        );
        assert!(
            audio_clip_xml.contains("<adjust-volume"),
            "audio clip should still have adjust-volume"
        );

        // Verify connected clip offset is in parent's source time space.
        // Primary clip: timeline_start=0, source_timecode_base=10s, source_in=0 →
        //   start = 10s. Connected video: timeline_start=0.5s →
        //   offset should be 10 + (0.5 - 0) = 10.5s = 10500000000ns.
        // At 24fps (denom=1): 10.5s = 252 frames → "252/24s".
        let connected_video_lane = inner.find("lane=\"1\"").expect("connected video lane");
        let tag_start = inner[..connected_video_lane].rfind("<asset-clip").unwrap();
        let tag_end = inner[connected_video_lane..].find('>').unwrap() + connected_video_lane;
        let connected_video_tag = &inner[tag_start..=tag_end];
        assert!(
            connected_video_tag.contains("offset=\"252/24s\""),
            "connected video offset should be in parent source time space: {connected_video_tag}"
        );
    }

    #[test]
    fn test_write_fcpxml_strict_intrinsic_param_order_matches_dtd() {
        let mut project = Project::new("StrictOrder");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/source.mov", 2_000_000_000, 0, ClipKind::Video);
        clip.crop_left = 2;
        clip.crop_top = 3;
        clip.crop_right = 4;
        clip.crop_bottom = 5;
        clip.opacity = 0.7;
        clip.opacity_keyframes = vec![crate::model::clip::NumericKeyframe {
            time_ns: 250_000_000,
            value: 0.7,
            interpolation: crate::model::clip::KeyframeInterpolation::EaseOut,
            bezier_controls: None,
        }];
        clip.volume_keyframes = vec![crate::model::clip::NumericKeyframe {
            time_ns: 250_000_000,
            value: 0.9,
            interpolation: crate::model::clip::KeyframeInterpolation::Linear,
            bezier_controls: None,
        }];
        clip.pan_keyframes = vec![crate::model::clip::NumericKeyframe {
            time_ns: 250_000_000,
            value: 0.2,
            interpolation: crate::model::clip::KeyframeInterpolation::Linear,
            bezier_controls: None,
        }];
        track.add_clip(clip);
        project.tracks.push(track);
        project.markers.push(crate::model::project::Marker {
            id: "m1".to_string(),
            position_ns: 0,
            label: "Marker".to_string(),
            color: 0xFF00FF00,
        });

        let xml = write_fcpxml_strict(&project).expect("strict write should succeed");
        let clip_start = xml.find("<asset-clip ").expect("asset-clip start");
        let clip_end = xml[clip_start..]
            .find("</asset-clip>")
            .map(|idx| clip_start + idx)
            .expect("asset-clip end");
        let clip_xml = &xml[clip_start..clip_end];
        let crop_idx = clip_xml.find("<adjust-crop").expect("adjust-crop");
        let transform_idx = clip_xml
            .find("<adjust-transform")
            .expect("adjust-transform");
        let blend_idx = clip_xml.find("<adjust-blend").expect("adjust-blend");
        let volume_idx = clip_xml.find("<adjust-volume").expect("adjust-volume");
        let panner_idx = clip_xml.find("<adjust-panner").expect("adjust-panner");
        assert!(crop_idx < transform_idx);
        assert!(transform_idx < blend_idx);
        assert!(blend_idx < volume_idx);
        assert!(volume_idx < panner_idx);
        assert!(
            !xml.contains("<marker "),
            "strict output should omit sequence markers"
        );
    }

    #[test]
    fn test_write_fcpxml_strict_wraps_volume_keyframes_in_audio_channel_source() {
        let mut project = Project::new("VolKF");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/source.mov", 2_000_000_000, 0, ClipKind::Video);
        clip.volume_keyframes = vec![
            crate::model::clip::NumericKeyframe {
                time_ns: 0,
                value: 1.0,
                interpolation: crate::model::clip::KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            crate::model::clip::NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 0.5,
                interpolation: crate::model::clip::KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml_strict(&project).expect("strict write should succeed");
        assert!(
            xml.contains("<audio-channel-source"),
            "keyframed volume should be wrapped in audio-channel-source"
        );
        // The keyframed adjust-volume should be inside audio-channel-source
        let acs_start = xml.find("<audio-channel-source").unwrap();
        let acs_end = xml.find("</audio-channel-source>").unwrap();
        let acs_block = &xml[acs_start..acs_end];
        assert!(
            acs_block.contains("<adjust-volume"),
            "adjust-volume should be inside audio-channel-source"
        );
        assert!(
            acs_block.contains("<keyframeAnimation"),
            "keyframe animation should be inside audio-channel-source"
        );
        // No flat adjust-volume outside audio-channel-source
        let after_acs = &xml[acs_end..];
        assert!(
            !after_acs.contains("<adjust-volume"),
            "no duplicate adjust-volume after audio-channel-source"
        );
        // DTD order: adjust-blend before audio-channel-source (no flat adjust-volume/panner between)
        let blend_pos = xml.find("<adjust-blend").expect("adjust-blend present");
        assert!(
            blend_pos < acs_start,
            "adjust-blend must come before audio-channel-source per DTD"
        );
        // No flat adjust-volume or adjust-panner should appear before audio-channel-source
        // (they are omitted when keyframed)
        let before_acs = &xml[..acs_start];
        assert!(
            !before_acs.contains("<adjust-volume"),
            "no flat adjust-volume when volume is keyframed"
        );
        // FCP doesn't support interp on volume param keyframes — strict mode omits it.
        assert!(
            !acs_block.contains("interp="),
            "strict mode should omit interp on volume keyframes"
        );
    }

    #[test]
    fn test_write_fcpxml_strict_flat_volume_no_audio_channel_source() {
        let mut project = Project::new("FlatVol");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let clip = Clip::new("/tmp/source.mov", 2_000_000_000, 0, ClipKind::Video);
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml_strict(&project).expect("strict write should succeed");
        assert!(
            !xml.contains("<audio-channel-source"),
            "flat volume should NOT use audio-channel-source"
        );
        assert!(
            xml.contains("<adjust-volume"),
            "flat volume should still emit adjust-volume"
        );
    }

    /// Verify that volume keyframe times are offset by source_in when written
    /// to strict FCPXML, so FCP sees them in source-absolute time.
    #[test]
    fn test_write_fcpxml_strict_volume_keyframes_offset_by_source_in() {
        let mut project = Project::new("VolKFOffset");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/source.mov", 2_000_000_000, 0, ClipKind::Video);
        clip.source_in = 10_000_000_000; // 10 seconds into source
        clip.volume_keyframes = vec![
            crate::model::clip::NumericKeyframe {
                time_ns: 0,
                value: 1.0,
                interpolation: crate::model::clip::KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            crate::model::clip::NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 0.5,
                interpolation: crate::model::clip::KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml_strict(&project).expect("strict write should succeed");
        // start="10s" → source_start_ns = 10_000_000_000
        // Keyframe 0 time: 0 + 10s = "240/24s" (10s at 24fps = 240 frames)
        // Keyframe 1 time: 1s + 10s = "264/24s" (11s at 24fps = 264 frames)
        assert!(
            xml.contains("time=\"240/24s\""),
            "first keyframe should be at 10s (240/24s), got:\n{}",
            xml
        );
        assert!(
            xml.contains("time=\"264/24s\""),
            "second keyframe should be at 11s (264/24s), got:\n{}",
            xml
        );
    }

    #[test]
    fn test_write_fcpxml_strict_omits_vendor_extensions_and_unknown_fields() {
        let original = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14" xmlns:us="urn:ultimateslice" customRoot="keep-root">
  <resources customResources="keep-resources">
    <format id="r1" frameDuration="1/24s" width="1920" height="1080" customFormat="keep-format"/>
    <asset id="a1" src="file:///tmp/clip.mov" name="clip" duration="48/24s" customAsset="keep-asset">
      <media-rep kind="original-media" src="file:///tmp/clip.mov"/>
      <metadata key="com.example.unknown" value="keep-meta"/>
    </asset>
  </resources>
  <library customLibrary="keep-library">
    <event customEvent="keep-event">
      <project name="MainProject" customProject="keep-project">
        <sequence format="r1" duration="48/24s" customSequence="keep-seq">
          <spine customSpine="keep-spine">
            <asset-clip ref="a1" offset="0s" start="0s" duration="48/24s" customClip="keep-clip" us:track-idx="0"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;
        let project = parse_fcpxml(original).expect("parse should succeed");
        let xml = write_fcpxml_strict(&project).expect("strict write should succeed");
        assert!(xml.contains("<fcpxml version=\"1.14\""));
        assert!(!xml.contains("xmlns:us="));
        assert!(!xml.contains("us:"));
        assert!(!xml.contains("customRoot="));
        assert!(!xml.contains("customResources="));
        assert!(!xml.contains("customAsset="));
        assert!(!xml.contains("customClip="));
        assert!(!xml.contains("<metadata "));
        assert!(xml.contains("<adjust-blend amount=\"1\""));
        assert!(xml.contains("<adjust-crop mode=\"trim\">"));
        assert!(xml.contains("<crop-rect left=\"0\" right=\"0\" top=\"0\" bottom=\"0\""));
    }

    #[test]
    fn test_write_fcpxml_uses_original_imported_source_path_when_present() {
        let mut project = Project::new("OriginalPath");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/remapped.mp4", 2_000_000_000, 0, ClipKind::Video);
        clip.fcpxml_original_source_path = Some("/Volumes/original.mp4".to_string());
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml(&project).expect("write should succeed");
        assert!(xml.contains("src=\"file:///Volumes/original.mp4\""));
        assert!(!xml.contains("src=\"file:///tmp/remapped.mp4\""));
    }

    #[test]
    fn test_write_fcpxml_dirty_scale_edit_preserves_fields_via_full_rewrite() {
        let original = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14" customRoot="keep-root">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///tmp/clip.mov" name="clip" duration="48/24s" customAsset="keep-asset">
      <metadata key="com.example.unknown" value="keep-meta"/>
    </asset>
  </resources>
  <library>
    <event customEvent="keep-event">
      <project name="MainProject">
        <sequence format="r1" duration="48/24s" customSequence="keep-seq">
          <spine>
            <asset-clip ref="a1" offset="0s" start="0s" duration="48/24s" customClip="keep-clip" us:scale="1">
              <adjust-transform position="0 0" scale="1 1" rotation="0"/>
            </asset-clip>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let mut project = parse_fcpxml(original).expect("parse should succeed");
        let clip = project
            .tracks
            .iter_mut()
            .flat_map(|t| t.clips.iter_mut())
            .next()
            .expect("expected imported clip");
        clip.scale = 1.75;
        clip.position_x = 0.25;
        clip.position_y = -0.5;
        clip.scale_keyframes = vec![
            crate::model::clip::NumericKeyframe {
                time_ns: 0,
                value: 1.0,
                interpolation: crate::model::clip::KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            crate::model::clip::NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 1.75,
                interpolation: crate::model::clip::KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        clip.opacity_keyframes = vec![crate::model::clip::NumericKeyframe {
            time_ns: 250_000_000,
            value: 0.5,
            interpolation: crate::model::clip::KeyframeInterpolation::Linear,
            bezier_controls: None,
        }];
        clip.position_x_keyframes = vec![crate::model::clip::NumericKeyframe {
            time_ns: 500_000_000,
            value: 0.25,
            interpolation: crate::model::clip::KeyframeInterpolation::Linear,
            bezier_controls: None,
        }];
        clip.position_y_keyframes = vec![crate::model::clip::NumericKeyframe {
            time_ns: 500_000_000,
            value: -0.5,
            interpolation: crate::model::clip::KeyframeInterpolation::Linear,
            bezier_controls: None,
        }];
        clip.volume_keyframes = vec![crate::model::clip::NumericKeyframe {
            time_ns: 0,
            value: 0.8,
            interpolation: crate::model::clip::KeyframeInterpolation::Linear,
            bezier_controls: None,
        }];
        clip.pan_keyframes = vec![crate::model::clip::NumericKeyframe {
            time_ns: 500_000_000,
            value: -0.25,
            interpolation: crate::model::clip::KeyframeInterpolation::Linear,
            bezier_controls: None,
        }];
        clip.brightness_keyframes = vec![crate::model::clip::NumericKeyframe {
            time_ns: 0,
            value: -0.2,
            interpolation: crate::model::clip::KeyframeInterpolation::Linear,
            bezier_controls: None,
        }];
        clip.contrast_keyframes = vec![crate::model::clip::NumericKeyframe {
            time_ns: 1_000_000_000,
            value: 1.4,
            interpolation: crate::model::clip::KeyframeInterpolation::Linear,
            bezier_controls: None,
        }];
        clip.saturation_keyframes = vec![crate::model::clip::NumericKeyframe {
            time_ns: 1_000_000_000,
            value: 0.7,
            interpolation: crate::model::clip::KeyframeInterpolation::Linear,
            bezier_controls: None,
        }];
        clip.temperature_keyframes = vec![crate::model::clip::NumericKeyframe {
            time_ns: 1_000_000_000,
            value: 7500.0,
            interpolation: crate::model::clip::KeyframeInterpolation::Linear,
            bezier_controls: None,
        }];
        clip.tint_keyframes = vec![crate::model::clip::NumericKeyframe {
            time_ns: 1_000_000_000,
            value: 0.3,
            interpolation: crate::model::clip::KeyframeInterpolation::Linear,
            bezier_controls: None,
        }];
        project.dirty = true;

        let written = write_fcpxml(&project).expect("write should succeed");
        // Full rewrite preserves unknown attributes via fcpxml_unknown_* fields.
        assert!(written.contains("customRoot=\"keep-root\""));
        assert!(written.contains("customAsset=\"keep-asset\""));
        assert!(written.contains("customSequence=\"keep-seq\""));
        assert!(written.contains("customClip=\"keep-clip\""));
        assert!(written.contains("<metadata key=\"com.example.unknown\" value=\"keep-meta\""));
        // Verify data values are correct.
        assert!(written.contains("us:scale=\"1.75\""));
        assert!(written.contains("us:position-x=\"0.25\""));
        assert!(written.contains("us:position-y=\"-0.5\""));
        assert!(written.contains("us:scale-keyframes="));
        assert!(written.contains("us:opacity-keyframes="));
        assert!(written.contains("us:position-x-keyframes="));
        assert!(written.contains("us:position-y-keyframes="));
        assert!(written.contains("us:volume-keyframes="));
        assert!(written.contains("us:brightness-keyframes="));
        assert!(written.contains("us:contrast-keyframes="));
        assert!(written.contains("us:saturation-keyframes="));
        assert!(written.contains("us:temperature-keyframes="));
        assert!(written.contains("us:tint-keyframes="));
        // Verify round-trip: parse the written XML back and check data values.
        let reparsed = parse_fcpxml(&written).expect("round-trip parse should succeed");
        let clip2 = reparsed
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .next()
            .expect("clip should survive round-trip");
        assert!(
            (clip2.scale - 1.75).abs() < 0.001,
            "scale should round-trip"
        );
        assert!(
            (clip2.position_x - 0.25).abs() < 0.001,
            "position_x should round-trip"
        );
        assert!(
            (clip2.position_y - (-0.5)).abs() < 0.001,
            "position_y should round-trip"
        );
        assert_eq!(
            clip2.scale_keyframes.len(),
            2,
            "scale keyframes should round-trip"
        );
        assert_eq!(
            clip2.opacity_keyframes.len(),
            1,
            "opacity keyframes should round-trip"
        );
    }

    #[test]
    fn test_write_fcpxml_allows_position_beyond_one() {
        let mut project = Project::new("LargeOffsetExport");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/source.mov", 2_000_000_000, 0, ClipKind::Video);
        clip.scale = 0.51;
        clip.position_x = 1.2;
        clip.position_y = 1.2;
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml(&project).expect("write should succeed");
        assert!(xml.contains("adjust-transform position=\"52.266666"));
        assert!(xml.contains("scale=\"0.51 0.51\""));
    }

    #[test]
    fn test_write_fcpxml_regenerated_xml_preserves_unknown_clip_attrs_after_scale_edit() {
        let original = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///tmp/clip1.mov" name="clip1" duration="24/24s"/>
    <asset id="a2" src="file:///tmp/clip2.mov" name="clip2" duration="24/24s"/>
  </resources>
  <library>
    <event>
      <project name="MainProject">
        <sequence format="r1" duration="48/24s">
          <spine>
            <asset-clip ref="a1" offset="0s" start="0s" duration="24/24s" customUnsupported="keep-one" us:scale="1">
              <customTag mode="hold">
                <childTag value="keep-child"/>
              </customTag>
            </asset-clip>
            <asset-clip ref="a2" offset="24/24s" start="0s" duration="24/24s" customUnsupported="keep-two" us:scale="1"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let mut project = parse_fcpxml(original).expect("parse should succeed");
        assert_eq!(
            project.tracks.iter().map(|t| t.clips.len()).sum::<usize>(),
            2,
            "two clips ensure dirty-save full regeneration path"
        );
        let first_clip = project
            .tracks
            .iter_mut()
            .flat_map(|t| t.clips.iter_mut())
            .next()
            .expect("expected imported clip");
        first_clip.scale = 2.25;
        project.dirty = true;

        let written = write_fcpxml(&project).expect("write should succeed");
        assert_ne!(written, original);
        assert!(written.contains("customUnsupported=\"keep-one\""));
        assert!(written.contains("customUnsupported=\"keep-two\""));
        assert!(written.contains("<customTag mode=\"hold\">"));
        assert!(written.contains("<childTag value=\"keep-child\"/>"));
        assert!(written.contains("us:scale=\"2.25\""));
        assert!(written.contains("adjust-transform"));
        assert!(written.contains("scale=\"2.25 2.25\""));
    }

    #[test]
    fn test_write_fcpxml_dirty_regeneration_preserves_asset_metadata_and_uses_media_rep() {
        let original = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///tmp/clip1.mov" name="clip1" duration="24/24s" customAsset="one">
      <metadata>
        <md key="com.example.keep.one" value="1"/>
      </metadata>
    </asset>
    <asset id="a2" src="file:///tmp/clip2.mov" name="clip2" duration="24/24s" customAsset="two">
      <metadata>
        <md key="com.example.keep.two" value="2"/>
      </metadata>
    </asset>
  </resources>
  <library>
    <event>
      <project name="MainProject">
        <sequence format="r1" duration="48/24s">
          <spine>
            <asset-clip ref="a1" offset="0s" start="0s" duration="24/24s" name="clip1"/>
            <asset-clip ref="a2" offset="24/24s" start="0s" duration="24/24s" name="clip2"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let mut project = parse_fcpxml(original).expect("parse should succeed");
        let first_clip = project
            .tracks
            .iter_mut()
            .flat_map(|t| t.clips.iter_mut())
            .next()
            .expect("expected imported clip");
        first_clip.scale = 1.5;
        project.dirty = true;

        let written = write_fcpxml(&project).expect("write should succeed");
        assert!(written.contains("<!DOCTYPE fcpxml>"));
        assert!(written.contains("<media-rep kind=\"original-media\""));
        assert!(written.contains("customAsset=\"one\""));
        assert!(written.contains("customAsset=\"two\""));
        assert!(written.contains("com.example.keep.one"));
        assert!(written.contains("com.example.keep.two"));
        assert!(!has_asset_src_attr(&written));
    }

    #[test]
    fn test_write_fcpxml_dirty_regeneration_preserves_unknown_tags_across_document() {
        let original = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14" customRoot="keep-root">
  <resources customResources="keep-resources">
    <format id="r1" frameDuration="1/24s" width="1920" height="1080" customFormat="keep-format"/>
    <asset id="a1" src="file:///tmp/clip1.mov" name="clip1" duration="24/24s"/>
    <asset id="a2" src="file:///tmp/clip2.mov" name="clip2" duration="24/24s"/>
    <resource-note value="keep-resource-child"/>
  </resources>
  <library location="file:///tmp/test.fcpbundle/" customLibrary="keep-library">
    <event customEvent="keep-event">
      <project name="MainProject" customProject="keep-project">
        <sequence format="r1" duration="48/24s" tcStart="0s" customSequence="keep-sequence">
          <spine customSpine="keep-spine">
            <asset-clip ref="a1" offset="0s" start="0s" duration="24/24s" name="clip1"/>
            <asset-clip ref="a2" offset="24/24s" start="0s" duration="24/24s" name="clip2"/>
            <gap name="keep-gap" duration="1/24s"/>
          </spine>
          <sequence-note value="keep-sequence-child"/>
        </sequence>
        <project-note value="keep-project-child"/>
      </project>
      <event-note value="keep-event-child"/>
    </event>
    <smart-collection name="keep-library-child"/>
  </library>
  <root-note value="keep-root-child"/>
</fcpxml>"#;

        let mut project = parse_fcpxml(original).expect("parse should succeed");
        let first_clip = project
            .tracks
            .iter_mut()
            .flat_map(|t| t.clips.iter_mut())
            .next()
            .expect("expected imported clip");
        first_clip.scale = 1.25;
        project.dirty = true;

        let written = write_fcpxml(&project).expect("write should succeed");
        assert!(written.contains("customRoot=\"keep-root\""));
        assert!(written.contains("customResources=\"keep-resources\""));
        assert!(written.contains("customFormat=\"keep-format\""));
        assert!(written.contains("customLibrary=\"keep-library\""));
        assert!(written.contains("customEvent=\"keep-event\""));
        assert!(written.contains("customProject=\"keep-project\""));
        assert!(written.contains("customSequence=\"keep-sequence\""));
        assert!(written.contains("tcStart=\"0s\""));
        assert!(written.contains("customSpine=\"keep-spine\""));
        assert!(written.contains("<resource-note value=\"keep-resource-child\"/>"));
        assert!(written.contains("<sequence-note value=\"keep-sequence-child\"/>"));
        assert!(written.contains("<project-note value=\"keep-project-child\"/>"));
        assert!(written.contains("<event-note value=\"keep-event-child\"/>"));
        assert!(written.contains("<smart-collection name=\"keep-library-child\"/>"));
        assert!(written.contains("<root-note value=\"keep-root-child\"/>"));
        assert!(written.contains("<gap name=\"keep-gap\" duration=\"1/24s\"/>"));
        assert!(!has_asset_src_attr(&written));
    }

    #[test]
    fn test_write_fcpxml_dirty_transform_edit_uses_full_rewrite_for_multi_clip_import() {
        let original = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE fcpxml>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="r1v" name="base" duration="48/24s" hasVideo="1" hasAudio="1">
      <media-rep kind="original-media" src="file:///tmp/base.mov"/>
    </asset>
    <asset id="r2v" name="overlay" duration="48/24s" hasVideo="1" hasAudio="1">
      <media-rep kind="original-media" src="file:///tmp/overlay.mov"/>
    </asset>
  </resources>
  <library location="file:///tmp/example.fcpbundle/">
    <event name="Event 1">
      <project name="Project 1">
        <sequence format="r1" duration="48/24s">
          <spine>
            <asset-clip ref="r1v" offset="0s" start="0s" duration="48/24s" name="base">
              <asset-clip ref="r2v" lane="1" offset="0s" start="0s" duration="48/24s" name="overlay">
                <adjust-transform position="20 -10" scale="0.5 0.5" rotation="0"/>
              </asset-clip>
            </asset-clip>
          </spine>
        </sequence>
      </project>
    </event>
    <smart-collection name="Projects" match="all">
      <match-clip rule="is" type="project"/>
    </smart-collection>
  </library>
</fcpxml>"#;

        let mut project = parse_fcpxml(original).expect("parse should succeed");
        let overlay = project
            .tracks
            .iter_mut()
            .flat_map(|t| t.clips.iter_mut())
            .find(|c| c.label == "overlay")
            .expect("overlay clip should exist");
        overlay.scale = 0.75;
        overlay.rotate = 37;
        project.dirty = true;

        let written = write_fcpxml(&project).expect("write should succeed");
        // Full rewrite generates its own asset IDs and includes vendor extensions.
        assert!(written.contains("scale=\"0.75 0.75\""));
        assert!(written.contains("rotation=\"37\""));
        // Verify the smart-collection is preserved via unknown library children.
        assert!(written.contains("<smart-collection name=\"Projects\""));
        // Verify the full rewrite emits USPXML vendor extensions.
        assert!(written.contains("us:track-idx="));
        // Round-trip: verify the written XML can be parsed back.
        let reparsed = parse_fcpxml(&written).expect("round-trip parse should succeed");
        let overlay = reparsed
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.label == "overlay")
            .expect("overlay clip should survive round-trip");
        assert!((overlay.scale - 0.75).abs() < 0.001);
        assert_eq!(overlay.rotate, 37);
    }

    #[test]
    fn test_write_fcpxml_uses_known_format_name_for_1080p24() {
        let project = Project::new("KnownFormatName");
        let xml = write_fcpxml(&project).expect("write should succeed");
        assert!(
            xml.contains(
                "<format id=\"r1\" name=\"FFVideoFormat1080p24\" frameDuration=\"1/24s\" width=\"1920\" height=\"1080\"/>"
            ),
            "expected known format name for standard 1080p24 export:\n{xml}"
        );
    }

    #[test]
    fn test_write_fcpxml_emits_track_state_vendor_attrs() {
        let mut project = Project::new("TrackStateWrite");
        project.tracks[0].muted = true;
        project.tracks[0].locked = true;
        project.tracks[0].soloed = true;
        project.tracks[0].height_preset = crate::model::track::TrackHeightPreset::Large;
        let mut clip = Clip::new("file:///tmp/clip.mp4", 1_000_000_000, 0, ClipKind::Video);
        clip.color_label = crate::model::clip::ClipColorLabel::Purple;
        project.tracks[0].add_clip(clip);

        let xml = write_fcpxml(&project).expect("write should succeed");
        assert!(xml.contains("us:track-muted=\"true\""));
        assert!(xml.contains("us:track-locked=\"true\""));
        assert!(xml.contains("us:track-soloed=\"true\""));
        assert!(xml.contains("us:track-height=\"large\""));
        assert!(xml.contains("us:color-label=\"purple\""));
    }

    #[test]
    fn test_write_fcpxml_omits_unknown_format_name_for_nonstandard_preset() {
        let mut project = Project::new("UnknownFormatName");
        project.width = 2560;
        project.height = 1080;
        project.frame_rate.numerator = 30000;
        project.frame_rate.denominator = 1001;

        let xml = write_fcpxml(&project).expect("write should succeed");
        assert!(
            xml.contains(
                "<format id=\"r1\" frameDuration=\"1001/30000s\" width=\"2560\" height=\"1080\"/>"
            ),
            "expected export to keep numeric format data for non-standard preset:\n{xml}"
        );
        assert!(
            !xml.contains("name=\"FFVideoFormat"),
            "expected export to omit unsupported hardcoded format names:\n{xml}"
        );
    }

    #[test]
    fn test_write_fcpxml_emits_native_keyframe_elements() {
        use crate::model::clip::{Clip, ClipKind, KeyframeInterpolation, NumericKeyframe};
        use crate::model::track::Track;

        let mut project = Project::new("NativeKF");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/clip.mp4", 5_000_000_000, 0, ClipKind::Video);
        clip.opacity_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 1.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 2_000_000_000,
                value: 0.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        clip.scale_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 0.5,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 5_000_000_000,
                value: 1.5,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        clip.volume_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 1.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 3_000_000_000,
                value: 0.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        clip.pan_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: -0.5,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 3_000_000_000,
                value: 0.5,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        clip.rotate_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: -30.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 3_000_000_000,
                value: 45.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        clip.crop_left_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 0.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 3_000_000_000,
                value: 120.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml(&project).expect("write should succeed");

        // Should contain native <adjust-transform> with param/keyframeAnimation/keyframe children
        assert!(
            xml.contains("<adjust-transform"),
            "missing adjust-transform"
        );
        assert!(xml.contains("<param name=\"scale\""), "missing scale param");
        assert!(
            xml.contains("<param name=\"rotation\""),
            "missing rotation param"
        );
        assert!(
            xml.contains("<keyframeAnimation"),
            "missing keyframeAnimation"
        );
        assert!(xml.contains("<keyframe "), "missing keyframe element");
        assert!(
            xml.contains("interp=\"linear\""),
            "missing interp attribute"
        );

        // Should contain native <adjust-compositing> with opacity keyframes
        assert!(
            xml.contains("<adjust-compositing"),
            "missing adjust-compositing"
        );
        assert!(
            xml.contains("<param name=\"amount\""),
            "missing amount param for opacity"
        );

        // Should contain <adjust-volume> with volume keyframes
        assert!(xml.contains("<adjust-volume"), "missing adjust-volume");
        assert!(xml.contains("<adjust-panner"), "missing adjust-panner");

        // Should also still have vendor attrs for lossless round-trip
        assert!(
            xml.contains("us:scale-keyframes="),
            "missing vendor scale keyframes"
        );
        assert!(
            xml.contains("us:opacity-keyframes="),
            "missing vendor opacity keyframes"
        );
        assert!(
            xml.contains("us:volume-keyframes="),
            "missing vendor volume keyframes"
        );
        assert!(
            xml.contains("us:pan-keyframes="),
            "missing vendor pan keyframes"
        );
        assert!(
            xml.contains("us:rotate-keyframes="),
            "missing vendor rotate keyframes"
        );
        assert!(
            xml.contains("us:crop-left-keyframes="),
            "missing vendor crop keyframes"
        );
    }

    #[test]
    fn test_write_fcpxml_strict_emits_native_curve_for_custom_bezier_keyframes() {
        use crate::model::clip::{
            BezierControls, Clip, ClipKind, KeyframeInterpolation, NumericKeyframe,
        };
        use crate::model::track::Track;

        let mut project = Project::new("StrictBezierCurve");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/clip.mp4", 5_000_000_000, 0, ClipKind::Video);
        clip.scale_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 1.0,
                interpolation: KeyframeInterpolation::EaseOut,
                bezier_controls: Some(BezierControls {
                    x1: 0.10,
                    y1: 0.05,
                    x2: 0.72,
                    y2: 0.95,
                }),
            },
            NumericKeyframe {
                time_ns: 2_000_000_000,
                value: 1.5,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml_strict(&project).expect("strict write should succeed");
        assert!(
            !xml.contains("xmlns:us="),
            "strict export should omit vendor ns"
        );
        assert!(xml.contains("<param name=\"scale\""), "missing scale param");
        assert!(
            xml.contains("curve=\"smooth\""),
            "strict keyframe should emit native curve attribute for custom tangents: {xml}"
        );
    }

    #[test]
    fn test_write_read_native_keyframe_round_trip() {
        use crate::model::clip::{Clip, ClipKind, KeyframeInterpolation, NumericKeyframe};
        use crate::model::track::Track;

        let mut project = Project::new("RoundTrip");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/clip.mp4", 5_000_000_000, 0, ClipKind::Video);
        clip.opacity_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 1.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 2_000_000_000,
                value: 0.3,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        clip.pan_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: -0.2,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 2_000_000_000,
                value: 0.4,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml(&project).expect("write should succeed");
        let loaded = crate::fcpxml::parser::parse_fcpxml(&xml).expect("parse should succeed");
        let loaded_clip = &loaded.video_tracks().next().unwrap().clips[0];

        // Vendor attrs should survive exactly (they take priority)
        assert_eq!(loaded_clip.opacity_keyframes.len(), 2);
        assert_eq!(loaded_clip.opacity_keyframes[0].time_ns, 0);
        assert!((loaded_clip.opacity_keyframes[0].value - 1.0).abs() < 0.001);
        assert_eq!(loaded_clip.opacity_keyframes[1].time_ns, 2_000_000_000);
        assert!((loaded_clip.opacity_keyframes[1].value - 0.3).abs() < 0.001);
        assert_eq!(loaded_clip.pan_keyframes.len(), 2);
        assert_eq!(loaded_clip.pan_keyframes[0].time_ns, 0);
        assert!((loaded_clip.pan_keyframes[0].value + 0.2).abs() < 0.001);
        assert_eq!(loaded_clip.pan_keyframes[1].time_ns, 2_000_000_000);
        assert!((loaded_clip.pan_keyframes[1].value - 0.4).abs() < 0.001);
    }

    #[test]
    fn test_write_read_transform_keyframe_round_trip_with_source_offset() {
        // When source_in != 0, writer must add source_in to clip-local keyframe
        // times so FCPXML contains absolute source times. Parser must subtract
        // source_in back to get clip-local times.
        use crate::model::clip::{Clip, ClipKind, KeyframeInterpolation, NumericKeyframe};
        use crate::model::track::Track;

        let source_in_ns = 10_000_000_000u64; // 10 seconds into source

        let mut project = Project::new("OffsetRT");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/clip.mp4", 5_000_000_000, 0, ClipKind::Video);
        clip.source_in = source_in_ns;
        clip.source_out = source_in_ns + 5_000_000_000;
        clip.scale_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 0.5,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 2_000_000_000,
                value: 1.5,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        clip.rotate_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 0.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 2_000_000_000,
                value: 45.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        clip.opacity_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
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
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml(&project).expect("write should succeed");

        // FCPXML keyframe times should be absolute (source_in + local)
        // 10s + 0s = 10s, 10s + 2s = 12s → in rational time at 24fps
        // The written FCPXML should NOT have keyframes at time="0s"
        // since source_in is 10s.
        assert!(
            !xml.contains("time=\"0s\"") && !xml.contains("time=\"0/24s\""),
            "keyframe times should be absolute, not clip-local: {xml}"
        );

        // Now parse it back and verify clip-local times are restored
        let loaded = crate::fcpxml::parser::parse_fcpxml(&xml).expect("parse should succeed");
        let loaded_clip = &loaded.video_tracks().next().unwrap().clips[0];

        // Scale: vendor attrs take priority, so they should be clip-local
        assert_eq!(loaded_clip.scale_keyframes.len(), 2);
        assert_eq!(loaded_clip.scale_keyframes[0].time_ns, 0);
        assert_eq!(loaded_clip.scale_keyframes[1].time_ns, 2_000_000_000);
        assert!((loaded_clip.scale_keyframes[0].value - 0.5).abs() < 0.001);
        assert!((loaded_clip.scale_keyframes[1].value - 1.5).abs() < 0.001);

        // Rotation
        assert_eq!(loaded_clip.rotate_keyframes.len(), 2);
        assert_eq!(loaded_clip.rotate_keyframes[0].time_ns, 0);
        assert_eq!(loaded_clip.rotate_keyframes[1].time_ns, 2_000_000_000);

        // Opacity
        assert_eq!(loaded_clip.opacity_keyframes.len(), 2);
        assert_eq!(loaded_clip.opacity_keyframes[0].time_ns, 0);
        assert_eq!(loaded_clip.opacity_keyframes[1].time_ns, 2_000_000_000);
    }

    #[test]
    fn test_export_project_with_media_copies_and_rewrites_paths() {
        let root = unique_test_dir("package-export");
        let source_a_dir = root.join("source-a");
        let source_b_dir = root.join("source-b");
        std::fs::create_dir_all(&source_a_dir).expect("create source-a dir");
        std::fs::create_dir_all(&source_b_dir).expect("create source-b dir");

        let source_a = source_a_dir.join("clip.mp4");
        let source_b = source_b_dir.join("clip.mp4");
        std::fs::write(&source_a, b"source-a").expect("write source-a");
        std::fs::write(&source_b, b"source-b").expect("write source-b");

        let output = root.join("Packaged.uspxml");

        let mut project = Project::new("Packaged");
        project.tracks.clear();
        project.source_fcpxml = Some("<fcpxml version=\"1.14\"/>".to_string());
        project.dirty = false;
        let mut track = Track::new_video("Video 1");
        let clip_a = Clip::new(
            source_a.to_string_lossy().to_string(),
            1_000_000_000,
            0,
            ClipKind::Video,
        );
        let mut clip_b = Clip::new(
            source_b.to_string_lossy().to_string(),
            1_000_000_000,
            1_000_000_000,
            ClipKind::Video,
        );
        clip_b.fcpxml_original_source_path = Some("/Volumes/original/clip.mp4".to_string());
        let clip_a_duplicate = Clip::new(
            source_a.to_string_lossy().to_string(),
            1_000_000_000,
            2_000_000_000,
            ClipKind::Video,
        );
        track.add_clip(clip_a);
        track.add_clip(clip_b);
        track.add_clip(clip_a_duplicate);
        project.tracks.push(track);

        let library_dir =
            export_project_with_media(&project, &output).expect("export project with media");
        assert_eq!(library_dir, root.join("Packaged.Library"));
        assert!(output.exists(), "expected packaged .uspxml output");

        let copied_files: Vec<_> = std::fs::read_dir(&library_dir)
            .expect("read library dir")
            .filter_map(|entry| entry.ok())
            .collect();
        assert_eq!(
            copied_files.len(),
            2,
            "expected deduped copy count with collision handling"
        );

        let xml = std::fs::read_to_string(&output).expect("read packaged xml");
        assert!(!xml.contains("/Volumes/original/clip.mp4"));
        assert!(!xml.contains(source_a.to_string_lossy().as_ref()));
        assert!(!xml.contains(source_b.to_string_lossy().as_ref()));
        let srcs = media_rep_src_values(&xml);
        assert_eq!(
            srcs.len(),
            2,
            "two unique source files should produce two media-rep refs (deduplicated)"
        );
        let library_uri_prefix = format!(
            "file://{}/",
            std::fs::canonicalize(&library_dir)
                .unwrap_or(library_dir.clone())
                .to_string_lossy()
        );
        assert!(
            srcs.iter().all(|src| src.starts_with(&library_uri_prefix)),
            "all media-rep sources should point into packaged library: {srcs:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_collect_files_timeline_used_only_excludes_unused_library_media() {
        let root = unique_test_dir("collect-timeline-only");
        std::fs::create_dir_all(&root).expect("create root");
        let used_source = root.join("used.mp4");
        let unused_source = root.join("unused.mp4");
        std::fs::write(&used_source, b"used-media").expect("write used source");
        std::fs::write(&unused_source, b"unused-media").expect("write unused source");

        let destination = root.join("Collected");
        let mut project = Project::new("CollectTimelineOnly");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        track.add_clip(Clip::new(
            used_source.to_string_lossy().to_string(),
            1_000_000_000,
            0,
            ClipKind::Video,
        ));
        project.tracks.push(track);

        let library = vec![
            crate::model::media_library::MediaItem::new(
                used_source.to_string_lossy().to_string(),
                1_000_000_000,
            ),
            crate::model::media_library::MediaItem::new(
                unused_source.to_string_lossy().to_string(),
                1_000_000_000,
            ),
        ];

        let summary = collect_files(
            &project,
            &library,
            &destination,
            CollectFilesMode::TimelineUsedOnly,
        )
        .expect("timeline-used collection should succeed");
        assert_eq!(summary.media_files_copied, 1);
        assert_eq!(summary.lut_files_copied, 0);
        assert_eq!(summary.total_files_copied(), 1);
        assert!(
            destination.join("used.mp4").exists(),
            "used media should be copied"
        );
        assert!(
            !destination.join("unused.mp4").exists(),
            "unused library media should not be copied in timeline-used mode"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_collect_files_entire_library_includes_unused_library_media() {
        let root = unique_test_dir("collect-entire-library");
        std::fs::create_dir_all(&root).expect("create root");
        let used_source = root.join("used.mp4");
        let unused_source = root.join("unused.mp4");
        std::fs::write(&used_source, b"used-media").expect("write used source");
        std::fs::write(&unused_source, b"unused-media").expect("write unused source");

        let destination = root.join("Collected");
        let mut project = Project::new("CollectEntireLibrary");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        track.add_clip(Clip::new(
            used_source.to_string_lossy().to_string(),
            1_000_000_000,
            0,
            ClipKind::Video,
        ));
        project.tracks.push(track);

        let library = vec![
            crate::model::media_library::MediaItem::new(
                used_source.to_string_lossy().to_string(),
                1_000_000_000,
            ),
            crate::model::media_library::MediaItem::new(
                unused_source.to_string_lossy().to_string(),
                1_000_000_000,
            ),
        ];

        let summary = collect_files(
            &project,
            &library,
            &destination,
            CollectFilesMode::EntireLibrary,
        )
        .expect("entire-library collection should succeed");
        assert_eq!(summary.media_files_copied, 2);
        assert_eq!(summary.lut_files_copied, 0);
        assert_eq!(summary.total_files_copied(), 2);
        assert!(
            destination.join("used.mp4").exists(),
            "used media should be copied"
        );
        assert!(
            destination.join("unused.mp4").exists(),
            "unused library media should be copied in entire-library mode"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_collect_files_deduplicates_media_lut_name_collisions() {
        let root = unique_test_dir("collect-lut-collision");
        let media_dir = root.join("media");
        let lut_dir = root.join("luts");
        std::fs::create_dir_all(&media_dir).expect("create media dir");
        std::fs::create_dir_all(&lut_dir).expect("create lut dir");

        let media_path = media_dir.join("shared.bin");
        let lut_path = lut_dir.join("shared.bin");
        std::fs::write(&media_path, b"media-bytes").expect("write media file");
        std::fs::write(&lut_path, b"lut-bytes").expect("write lut file");

        let destination = root.join("Collected");
        let mut project = Project::new("CollectCollision");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new(
            media_path.to_string_lossy().to_string(),
            1_000_000_000,
            0,
            ClipKind::Video,
        );
        clip.lut_paths.push(lut_path.to_string_lossy().to_string());
        track.add_clip(clip);
        project.tracks.push(track);

        let summary = collect_files(
            &project,
            &[],
            &destination,
            CollectFilesMode::TimelineUsedOnly,
        )
        .expect("collection should succeed");
        assert_eq!(summary.media_files_copied, 1);
        assert_eq!(summary.lut_files_copied, 1);
        assert_eq!(summary.total_files_copied(), 2);

        let mut collected: Vec<(String, Vec<u8>)> = std::fs::read_dir(&destination)
            .expect("read destination")
            .map(|entry| {
                let path = entry.expect("entry").path();
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .expect("utf8 file name")
                    .to_string();
                let bytes = std::fs::read(&path).expect("read collected file");
                (name, bytes)
            })
            .collect();
        collected.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(collected.len(), 2, "media and LUT should both be present");
        assert_ne!(
            collected[0].0, collected[1].0,
            "collision handling should assign distinct destination names"
        );
        assert!(
            collected
                .iter()
                .any(|(_, bytes)| bytes.as_slice() == b"media-bytes"),
            "expected copied media file contents"
        );
        assert!(
            collected
                .iter()
                .any(|(_, bytes)| bytes.as_slice() == b"lut-bytes"),
            "expected copied LUT file contents"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_collect_files_preserves_existing_destination_files() {
        let root = unique_test_dir("collect-existing-destination");
        std::fs::create_dir_all(&root).expect("create root");
        let source = root.join("clip.mp4");
        std::fs::write(&source, b"new-media").expect("write source");

        let destination = root.join("Collected");
        std::fs::create_dir_all(&destination).expect("create destination");
        std::fs::write(destination.join("clip.mp4"), b"existing-media")
            .expect("write existing destination file");

        let mut project = Project::new("CollectExistingDestination");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        track.add_clip(Clip::new(
            source.to_string_lossy().to_string(),
            1_000_000_000,
            0,
            ClipKind::Video,
        ));
        project.tracks.push(track);

        let summary = collect_files(
            &project,
            &[],
            &destination,
            CollectFilesMode::TimelineUsedOnly,
        )
        .expect("collection should succeed");
        assert_eq!(summary.media_files_copied, 1);

        let mut collected: Vec<(String, Vec<u8>)> = std::fs::read_dir(&destination)
            .expect("read destination")
            .map(|entry| {
                let path = entry.expect("entry").path();
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .expect("utf8 file name")
                    .to_string();
                let bytes = std::fs::read(&path).expect("read collected file");
                (name, bytes)
            })
            .collect();
        collected.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(collected.len(), 2, "existing file should be preserved");
        assert!(
            collected
                .iter()
                .any(|(name, bytes)| name == "clip.mp4" && bytes.as_slice() == b"existing-media"),
            "existing destination file should not be overwritten"
        );
        assert!(
            collected
                .iter()
                .any(|(name, bytes)| name != "clip.mp4" && bytes.as_slice() == b"new-media"),
            "newly collected file should be copied with a distinct name"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_apply_collected_files_manifest_updates_next_save_paths() {
        let root = unique_test_dir("collect-apply-next-save");
        std::fs::create_dir_all(&root).expect("create root");
        let used_source = root.join("used.mp4");
        let unused_source = root.join("unused.mp4");
        let lut_path = root.join("look.cube");
        std::fs::write(&used_source, b"used-media").expect("write used source");
        std::fs::write(&unused_source, b"unused-media").expect("write unused source");
        std::fs::write(&lut_path, b"lut-bytes").expect("write lut");

        let destination = root.join("Collected");
        let mut project = Project::new("CollectApplyNextSave");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new(
            used_source.to_string_lossy().to_string(),
            1_000_000_000,
            0,
            ClipKind::Video,
        );
        clip.fcpxml_original_source_path = Some("/Volumes/original/used.mp4".to_string());
        clip.lut_paths.push(lut_path.to_string_lossy().to_string());
        track.add_clip(clip);
        project.tracks.push(track);

        let mut library = vec![
            crate::model::media_library::MediaItem::new(
                used_source.to_string_lossy().to_string(),
                1_000_000_000,
            ),
            crate::model::media_library::MediaItem::new(
                unused_source.to_string_lossy().to_string(),
                1_000_000_000,
            ),
        ];

        let manifest = collect_files_with_manifest(
            &project,
            &library,
            &destination,
            CollectFilesMode::EntireLibrary,
            |_| {},
        )
        .expect("collection manifest should succeed");
        let summary =
            apply_collected_files_manifest(&mut project, library.as_mut_slice(), &manifest);
        assert!(
            summary.updated_any(),
            "project/library references should update"
        );
        assert_eq!(summary.project_media_references_updated, 1);
        assert_eq!(summary.project_lut_references_updated, 1);
        assert_eq!(summary.library_items_updated, 2);
        assert!(
            project.dirty,
            "relinking collected paths should dirty the project"
        );

        let collected_used = manifest
            .source_to_destination_path
            .get(&used_source.to_string_lossy().to_string())
            .expect("collected used source")
            .to_string_lossy()
            .to_string();
        let collected_unused = manifest
            .source_to_destination_path
            .get(&unused_source.to_string_lossy().to_string())
            .expect("collected unused source")
            .to_string_lossy()
            .to_string();
        let collected_lut = manifest
            .lut_source_to_destination_path
            .get(&lut_path.to_string_lossy().to_string())
            .expect("collected lut")
            .to_string_lossy()
            .to_string();

        let clip = &project.tracks[0].clips[0];
        assert_eq!(clip.source_path, collected_used);
        assert_eq!(clip.lut_paths, vec![collected_lut.clone()]);
        assert_eq!(clip.fcpxml_original_source_path, None);
        assert_eq!(library[0].source_path, collected_used);
        assert_eq!(library[1].source_path, collected_unused);

        let xml = write_fcpxml_for_path(&project, Path::new("/tmp/collect-apply.uspxml"))
            .expect("write updated project xml");
        assert!(
            xml.contains(&collected_used),
            "saved project should reference collected media path"
        );
        assert!(
            xml.contains(&collected_lut),
            "saved project should reference collected LUT path"
        );
        assert!(
            !xml.contains("/Volumes/original/used.mp4"),
            "saved project should stop using preserved original FCPXML source path"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_normalize_packaged_path_for_portability_external_mounts() {
        assert_eq!(
            normalize_packaged_path_for_portability(Path::new(
                "/media/alex/LEXAR/Show/Packaged.Library/clip.mp4"
            )),
            PathBuf::from("/Volumes/LEXAR/Show/Packaged.Library/clip.mp4")
        );
        assert_eq!(
            normalize_packaged_path_for_portability(Path::new(
                "/run/media/alex/SSD_A/Packaged.Library/audio.wav"
            )),
            PathBuf::from("/Volumes/SSD_A/Packaged.Library/audio.wav")
        );
        assert_eq!(
            normalize_packaged_path_for_portability(Path::new(
                "/mnt/DriveB/Packaged.Library/shot.mov"
            )),
            PathBuf::from("/Volumes/DriveB/Packaged.Library/shot.mov")
        );
    }

    #[test]
    fn test_normalize_packaged_path_for_portability_noop_for_non_external_paths() {
        let home_path = PathBuf::from("/home/alex/Projects/Packaged.Library/clip.mp4");
        assert_eq!(
            normalize_packaged_path_for_portability(&home_path),
            home_path
        );
        let already_volumes = PathBuf::from("/Volumes/LEXAR/Packaged.Library/clip.mp4");
        assert_eq!(
            normalize_packaged_path_for_portability(&already_volumes),
            already_volumes
        );
    }

    #[test]
    fn test_export_project_with_media_reports_progress() {
        let root = unique_test_dir("package-progress");
        std::fs::create_dir_all(&root).expect("create root");
        let source_a = root.join("clip-a.mp4");
        let source_b = root.join("clip-b.mp4");
        std::fs::write(&source_a, b"source-a").expect("write source-a");
        std::fs::write(&source_b, b"source-b").expect("write source-b");
        let output = root.join("Progress.uspxml");

        let mut project = Project::new("Progress");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        track.add_clip(Clip::new(
            source_a.to_string_lossy().to_string(),
            1_000_000_000,
            0,
            ClipKind::Video,
        ));
        track.add_clip(Clip::new(
            source_b.to_string_lossy().to_string(),
            1_000_000_000,
            1_000_000_000,
            ClipKind::Video,
        ));
        project.tracks.push(track);

        let mut progress_events = Vec::new();
        let library_dir = export_project_with_media_with_progress(&project, &output, |progress| {
            progress_events.push(progress);
        })
        .expect("export with progress should succeed");
        assert!(library_dir.exists(), "expected packaged library to exist");
        assert!(
            progress_events.iter().any(|event| matches!(
                event,
                ExportProjectWithMediaProgress::Copying {
                    copied_files: 2,
                    total_files: 2,
                    ..
                }
            )),
            "expected final copy progress event"
        );
        assert!(
            progress_events.iter().any(|event| {
                matches!(event, ExportProjectWithMediaProgress::WritingProjectXml)
            }),
            "expected writing-xml progress event"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_export_project_with_media_writes_strict_xml_without_vendor_attrs() {
        let root = unique_test_dir("package-strict");
        std::fs::create_dir_all(&root).expect("create root");
        let source_a = root.join("clip-a.mp4");
        let source_b = root.join("clip-b.mp4");
        std::fs::write(&source_a, b"source-a").expect("write source-a");
        std::fs::write(&source_b, b"source-b").expect("write source-b");
        let output = root.join("Strict.fcpxml");

        let mut project = Project::new("Strict");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip_a = Clip::new(
            source_a.to_string_lossy().to_string(),
            1_000_000_000,
            0,
            ClipKind::Video,
        );
        clip_a.opacity = 0.75;
        clip_a.crop_left = 1;
        clip_a.crop_right = 2;
        clip_a.crop_top = 3;
        clip_a.crop_bottom = 4;
        clip_a.opacity_keyframes = vec![
            crate::model::clip::NumericKeyframe {
                time_ns: 0,
                value: 1.0,
                interpolation: crate::model::clip::KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            crate::model::clip::NumericKeyframe {
                time_ns: 500_000_000,
                value: 0.75,
                interpolation: crate::model::clip::KeyframeInterpolation::EaseInOut,
                bezier_controls: None,
            },
        ];
        let mut clip_b = Clip::new(
            source_b.to_string_lossy().to_string(),
            1_000_000_000,
            1_000_000_000,
            ClipKind::Video,
        );
        clip_b.scale = 1.2;
        clip_b.position_x = 0.25;
        clip_b.position_y = -0.15;
        clip_b.scale_keyframes = vec![
            crate::model::clip::NumericKeyframe {
                time_ns: 0,
                value: 1.0,
                interpolation: crate::model::clip::KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            crate::model::clip::NumericKeyframe {
                time_ns: 750_000_000,
                value: 1.2,
                interpolation: crate::model::clip::KeyframeInterpolation::EaseOut,
                bezier_controls: None,
            },
        ];
        track.add_clip(clip_a);
        track.add_clip(clip_b);
        project.tracks.push(track);

        let _library =
            export_project_with_media(&project, &output).expect("packaged export should succeed");
        let xml = std::fs::read_to_string(&output).expect("read packaged xml");
        assert!(!xml.contains("xmlns:us="));
        assert!(!xml.contains("us:"));
        assert!(
            xml.matches("<asset-clip ").count() >= 2,
            "expected multiple clips in strict packaged output"
        );
        assert!(xml.contains("<adjust-blend amount=\"0.75\""));
        assert!(xml.contains("<adjust-crop mode=\"trim\">"));
        assert!(xml.contains("<crop-rect left=\"1\" right=\"2\" top=\"3\" bottom=\"4\""));
        assert!(xml.contains("<param name=\"scale\""));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_export_project_with_media_strict_preserves_multitrack_layout_via_lane() {
        let root = unique_test_dir("package-strict-multitrack");
        std::fs::create_dir_all(&root).expect("create root");
        let source_v1 = root.join("v1.mp4");
        let source_v2 = root.join("v2.mp4");
        let source_a1 = root.join("a1.wav");
        std::fs::write(&source_v1, b"source-v1").expect("write v1");
        std::fs::write(&source_v2, b"source-v2").expect("write v2");
        std::fs::write(&source_a1, b"source-a1").expect("write a1");
        let output = root.join("StrictMultiTrack.fcpxml");

        let mut project = Project::new("StrictMultiTrack");
        project.tracks.clear();
        let mut video_1 = Track::new_video("Video 1");
        video_1.add_clip(Clip::new(
            source_v1.to_string_lossy().to_string(),
            1_000_000_000,
            0,
            ClipKind::Video,
        ));
        let mut video_2 = Track::new_video("Video 2");
        video_2.add_clip(Clip::new(
            source_v2.to_string_lossy().to_string(),
            1_000_000_000,
            0,
            ClipKind::Video,
        ));
        let mut audio_1 = Track::new_audio("Audio 1");
        audio_1.add_clip(Clip::new(
            source_a1.to_string_lossy().to_string(),
            1_000_000_000,
            0,
            ClipKind::Audio,
        ));
        project.tracks.push(video_1);
        project.tracks.push(video_2);
        project.tracks.push(audio_1);

        let _library =
            export_project_with_media(&project, &output).expect("packaged export should succeed");
        let xml = std::fs::read_to_string(&output).expect("read packaged xml");
        assert!(
            xml.contains("lane=\"1\""),
            "expected lane for second video track"
        );
        assert!(
            xml.contains("lane=\"-1\""),
            "expected lane for first audio track"
        );
        let parsed = parse_fcpxml(&xml).expect("parse exported xml");
        let video_tracks_with_clips = parsed
            .tracks
            .iter()
            .filter(|t| t.kind == crate::model::track::TrackKind::Video && !t.clips.is_empty())
            .count();
        let audio_tracks_with_clips = parsed
            .tracks
            .iter()
            .filter(|t| t.kind == crate::model::track::TrackKind::Audio && !t.clips.is_empty())
            .count();
        assert!(
            video_tracks_with_clips >= 2,
            "expected at least two populated video tracks after round-trip"
        );
        assert!(
            audio_tracks_with_clips >= 1,
            "expected at least one populated audio track after round-trip"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_packaged_export_strict_xml_validates_with_external_dtd_when_available() {
        let dtd_path = match std::env::var("ULTIMATESLICE_FCPXML_DTD_PATH") {
            Ok(path) if !path.trim().is_empty() => path,
            _ => return,
        };
        if !std::path::Path::new(&dtd_path).exists() {
            return;
        }
        let xmllint_available = std::process::Command::new("xmllint")
            .arg("--version")
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !xmllint_available {
            return;
        }

        let root = unique_test_dir("package-dtd");
        std::fs::create_dir_all(&root).expect("create root");
        let source_a = root.join("clip-a.mp4");
        let source_b = root.join("clip-b.mp4");
        std::fs::write(&source_a, b"source-a").expect("write source-a");
        std::fs::write(&source_b, b"source-b").expect("write source-b");
        let output = root.join("Validate.fcpxml");

        let mut project = Project::new("Validate");
        project.tracks.clear();
        let mut video_1 = Track::new_video("Video 1");
        let mut clip_a = Clip::new(
            source_a.to_string_lossy().to_string(),
            1_000_000_000,
            0,
            ClipKind::Video,
        );
        clip_a.opacity = 0.8;
        clip_a.opacity_keyframes = vec![
            crate::model::clip::NumericKeyframe {
                time_ns: 0,
                value: 1.0,
                interpolation: crate::model::clip::KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            crate::model::clip::NumericKeyframe {
                time_ns: 500_000_000,
                value: 0.8,
                interpolation: crate::model::clip::KeyframeInterpolation::EaseIn,
                bezier_controls: None,
            },
        ];
        video_1.add_clip(clip_a);

        let mut video_2 = Track::new_video("Video 2");
        let mut clip_b = Clip::new(
            source_b.to_string_lossy().to_string(),
            1_000_000_000,
            0,
            ClipKind::Video,
        );
        clip_b.position_x = 0.1;
        clip_b.position_y = -0.2;
        clip_b.scale = 1.15;
        clip_b.scale_keyframes = vec![
            crate::model::clip::NumericKeyframe {
                time_ns: 0,
                value: 1.0,
                interpolation: crate::model::clip::KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            crate::model::clip::NumericKeyframe {
                time_ns: 750_000_000,
                value: 1.15,
                interpolation: crate::model::clip::KeyframeInterpolation::EaseOut,
                bezier_controls: None,
            },
        ];
        video_2.add_clip(clip_b);

        let source_audio = root.join("audio-a.wav");
        std::fs::write(&source_audio, b"audio-a").expect("write source-audio");
        let mut audio_1 = Track::new_audio("Audio 1");
        let mut clip_c = Clip::new(
            source_audio.to_string_lossy().to_string(),
            1_000_000_000,
            0,
            ClipKind::Audio,
        );
        clip_c.pan = -0.25;
        clip_c.pan_keyframes = vec![crate::model::clip::NumericKeyframe {
            time_ns: 250_000_000,
            value: -0.25,
            interpolation: crate::model::clip::KeyframeInterpolation::Linear,
            bezier_controls: None,
        }];
        audio_1.add_clip(clip_c);

        project.tracks.push(video_1);
        project.tracks.push(video_2);
        project.tracks.push(audio_1);
        export_project_with_media(&project, &output).expect("packaged export should succeed");

        let validation = std::process::Command::new("xmllint")
            .arg("--noout")
            .arg("--dtdvalid")
            .arg(&dtd_path)
            .arg(&output)
            .output()
            .expect("run xmllint");
        assert!(
            validation.status.success(),
            "xmllint validation failed: {}",
            String::from_utf8_lossy(&validation.stderr)
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_export_project_with_media_errors_on_missing_source() {
        let root = unique_test_dir("package-missing");
        std::fs::create_dir_all(&root).expect("create root");
        let output = root.join("Missing.uspxml");

        let mut project = Project::new("Missing");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        track.add_clip(Clip::new(
            root.join("does-not-exist.mp4")
                .to_string_lossy()
                .to_string(),
            1_000_000_000,
            0,
            ClipKind::Video,
        ));
        project.tracks.push(track);

        let error = export_project_with_media(&project, &output)
            .expect_err("expected missing source error");
        assert!(
            error.to_string().contains("Source media not found"),
            "unexpected error: {error}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_export_project_with_media_uspxml_uses_rich_writer_and_copies_luts() {
        let root = unique_test_dir("package-rich-lut");
        std::fs::create_dir_all(&root).expect("create root");
        let source = root.join("clip.mp4");
        std::fs::write(&source, b"video-data").expect("write source");
        let lut = root.join("grade.cube");
        std::fs::write(&lut, b"LUT3D_DATA").expect("write lut");
        let output = root.join("RichLut.uspxml");

        let mut project = Project::new("RichLut");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new(
            source.to_string_lossy().to_string(),
            1_000_000_000,
            0,
            ClipKind::Video,
        );
        clip.lut_paths = vec![lut.to_string_lossy().to_string()];
        clip.exposure = 0.5;
        track.add_clip(clip);
        project.tracks.push(track);

        let library =
            export_project_with_media(&project, &output).expect("rich export should succeed");
        let xml = std::fs::read_to_string(&output).expect("read packaged xml");

        // Rich mode: should contain vendor namespace and us: attributes
        assert!(
            xml.contains("xmlns:us="),
            "expected vendor namespace in rich export"
        );
        assert!(
            xml.contains("us:exposure="),
            "expected us:exposure in rich export"
        );

        // LUT should be copied into Library
        let lut_in_library = library.join("grade.cube");
        assert!(lut_in_library.exists(), "LUT should be copied to Library");
        assert_eq!(
            std::fs::read(&lut_in_library).unwrap(),
            b"LUT3D_DATA",
            "LUT content should match"
        );

        // us:lut-path in XML should reference the packaged path
        assert!(
            xml.contains("us:lut-path="),
            "expected us:lut-path in rich export"
        );
        assert!(
            !xml.contains(&lut.to_string_lossy().to_string()),
            "should not contain original absolute LUT path"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_export_project_with_media_fcpxml_strips_lut_and_vendor_attrs() {
        let root = unique_test_dir("package-strict-lut");
        std::fs::create_dir_all(&root).expect("create root");
        let source = root.join("clip.mp4");
        std::fs::write(&source, b"video-data").expect("write source");
        let lut = root.join("grade.cube");
        std::fs::write(&lut, b"LUT3D_DATA").expect("write lut");
        let output = root.join("StrictLut.fcpxml");

        let mut project = Project::new("StrictLut");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new(
            source.to_string_lossy().to_string(),
            1_000_000_000,
            0,
            ClipKind::Video,
        );
        clip.lut_paths = vec![lut.to_string_lossy().to_string()];
        track.add_clip(clip);
        project.tracks.push(track);

        let _library =
            export_project_with_media(&project, &output).expect("strict export should succeed");
        let xml = std::fs::read_to_string(&output).expect("read packaged xml");

        // Strict mode: no vendor namespace or us: attributes
        assert!(
            !xml.contains("xmlns:us="),
            "strict export should not have vendor namespace"
        );
        assert!(
            !xml.contains("us:lut-path"),
            "strict export should not have us:lut-path"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_ns_to_fcpxml_time_ntsc_roundtrip() {
        use crate::model::project::FrameRate;
        let fps = FrameRate {
            numerator: 24000,
            denominator: 1001,
        };

        // GoPro timecode 20:13:33:07 = 1,747,519 frames
        // Stored as ns: 1747519 * 1001 * 1e9 / 24000 = 72886104958333
        let ns = 72_886_104_958_333u64;
        let result = ns_to_fcpxml_time(ns, &fps);
        // Should round to exactly 1749266519/24000s (= 1747519 frames * 1001)
        assert_eq!(result, "1749266519/24000s");
    }

    #[test]
    fn test_ns_to_fcpxml_time_integer_fps() {
        use crate::model::project::FrameRate;
        let fps = FrameRate {
            numerator: 24,
            denominator: 1,
        };
        // 48 frames = 2 seconds
        let ns = 2_000_000_000u64;
        assert_eq!(ns_to_fcpxml_time(ns, &fps), "48/24s");
    }

    #[test]
    fn test_known_fcpxml_format_name_ntsc() {
        let mut project = Project::new("Test");
        project.width = 1920;
        project.height = 1080;
        project.frame_rate.numerator = 24000;
        project.frame_rate.denominator = 1001;
        assert_eq!(
            known_fcpxml_format_name(&project),
            Some("FFVideoFormat1080p2398")
        );
    }

    #[test]
    fn test_write_fcpxml_strict_emits_filter_video_color_adjustments() {
        let mut project = Project::new("ColorAdj");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/source.mov", 2_000_000_000, 0, ClipKind::Video);
        clip.exposure = 0.5;
        clip.brightness = -0.1;
        clip.contrast = 1.25;
        clip.saturation = 0.5;
        clip.highlights = -0.2;
        clip.black_point = 0.15;
        clip.shadows = 0.3;
        clip.highlights_warmth = 0.4;
        clip.highlights_tint = -0.1;
        clip.midtones_warmth = 0.6;
        clip.midtones_tint = 0.05;
        clip.shadows_warmth = -0.25;
        clip.shadows_tint = 0.8;
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml_strict(&project).expect("strict write should succeed");

        // Should have the effect resource
        assert!(
            xml.contains(r#"<effect id="r_fcp_color_adj" name="Color Adjustments""#),
            "should emit Color Adjustments effect resource"
        );

        // Should have filter-video element
        assert!(
            xml.contains(r#"<filter-video ref="r_fcp_color_adj" name="Color Adjustments">"#),
            "should emit filter-video element"
        );

        // Verify param values (US → FCP: ×100, contrast/saturation: (v-1)*100)
        assert!(xml.contains(r#"<param name="Exposure" key="3" value="50""#));
        assert!(xml.contains(r#"<param name="Brightness" key="2" value="-10""#));
        assert!(xml.contains(r#"<param name="Contrast" key="17" value="25""#));
        assert!(xml.contains(r#"<param name="Saturation" key="16" value="-50""#));
        assert!(xml.contains(r#"<param name="Highlights" key="7" value="-20""#));
        assert!(xml.contains(r#"<param name="Black Point" key="1" value="15""#));
        assert!(xml.contains(r#"<param name="Shadows" key="4" value="30""#));
        assert!(xml.contains(r#"<param name="Highlights Warmth" key="10" value="40""#));
        assert!(xml.contains(r#"<param name="Highlights Tint" key="11" value="-10""#));
        assert!(xml.contains(r#"<param name="Midtones Warmth" key="12" value="60""#));
        assert!(xml.contains(r#"<param name="Midtones Tint" key="13" value="5""#));
        assert!(xml.contains(r#"<param name="Shadows Warmth" key="14" value="-25""#));
        assert!(xml.contains(r#"<param name="Shadows Tint" key="15" value="80""#));

        // filter-video should come after audio-channel-source if present
        let clip_start = xml.find("<asset-clip ").unwrap();
        let clip_end = xml[clip_start..].find("</asset-clip>").unwrap() + clip_start;
        let clip_xml = &xml[clip_start..clip_end];
        let fv_idx = clip_xml.find("<filter-video").expect("filter-video");
        // No audio-channel-source in this clip, so filter-video just needs to exist
        assert!(fv_idx > 0);
    }

    #[test]
    fn test_write_fcpxml_strict_skips_filter_video_when_defaults() {
        let mut project = Project::new("NoColor");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let clip = Clip::new("/tmp/source.mov", 2_000_000_000, 0, ClipKind::Video);
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml_strict(&project).expect("strict write should succeed");
        assert!(
            !xml.contains("filter-video"),
            "should not emit filter-video when all color values are defaults"
        );
        assert!(
            !xml.contains("Color Adjustments"),
            "should not emit Color Adjustments effect when no color changes"
        );
    }

    #[test]
    fn test_write_fcpxml_strict_filter_video_after_audio_channel_source() {
        let mut project = Project::new("OrderTest");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/source.mov", 2_000_000_000, 0, ClipKind::Video);
        clip.exposure = 0.5;
        clip.volume_keyframes = vec![crate::model::clip::NumericKeyframe {
            time_ns: 0,
            value: 0.8,
            interpolation: crate::model::clip::KeyframeInterpolation::Linear,
            bezier_controls: None,
        }];
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml_strict(&project).expect("strict write should succeed");
        let clip_start = xml.find("<asset-clip ").unwrap();
        let clip_end = xml[clip_start..].find("</asset-clip>").unwrap() + clip_start;
        let clip_xml = &xml[clip_start..clip_end];

        let acs_idx = clip_xml
            .find("<audio-channel-source")
            .expect("audio-channel-source");
        let fv_idx = clip_xml.find("<filter-video").expect("filter-video");
        assert!(
            acs_idx < fv_idx,
            "filter-video must come after audio-channel-source per DTD"
        );
    }

    #[test]
    fn test_color_adjustments_round_trip() {
        // Create a project with color adjustments, write strict, parse back, verify.
        let mut project = Project::new("RoundTrip");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/source.mov", 2_000_000_000, 0, ClipKind::Video);
        clip.exposure = 0.5;
        clip.brightness = -0.1;
        clip.contrast = 1.25;
        clip.saturation = 0.5;
        clip.highlights = -0.2;
        clip.black_point = 0.15;
        clip.shadows = 0.3;
        clip.highlights_warmth = 0.4;
        clip.highlights_tint = -0.1;
        clip.midtones_warmth = 0.6;
        clip.midtones_tint = 0.05;
        clip.shadows_warmth = -0.25;
        clip.shadows_tint = 0.8;
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml_strict(&project).expect("write should succeed");
        let parsed = crate::fcpxml::parser::parse_fcpxml(&xml).expect("parse should succeed");
        let clip2 = &parsed.video_tracks().next().unwrap().clips[0];

        assert!((clip2.exposure - 0.5).abs() < 1e-3, "exposure round-trip");
        assert!(
            (clip2.brightness - (-0.1)).abs() < 1e-3,
            "brightness round-trip"
        );
        assert!((clip2.contrast - 1.25).abs() < 1e-3, "contrast round-trip");
        assert!(
            (clip2.saturation - 0.5).abs() < 1e-3,
            "saturation round-trip"
        );
        assert!(
            (clip2.highlights - (-0.2)).abs() < 1e-3,
            "highlights round-trip"
        );
        assert!(
            (clip2.black_point - 0.15).abs() < 1e-3,
            "black_point round-trip"
        );
        assert!((clip2.shadows - 0.3).abs() < 1e-3, "shadows round-trip");
        assert!(
            (clip2.highlights_warmth - 0.4).abs() < 1e-3,
            "highlights_warmth round-trip"
        );
        assert!(
            (clip2.highlights_tint - (-0.1)).abs() < 1e-3,
            "highlights_tint round-trip"
        );
        assert!(
            (clip2.midtones_warmth - 0.6).abs() < 1e-3,
            "midtones_warmth round-trip"
        );
        assert!(
            (clip2.midtones_tint - 0.05).abs() < 1e-3,
            "midtones_tint round-trip"
        );
        assert!(
            (clip2.shadows_warmth - (-0.25)).abs() < 1e-3,
            "shadows_warmth round-trip"
        );
        assert!(
            (clip2.shadows_tint - 0.8).abs() < 1e-3,
            "shadows_tint round-trip"
        );
    }
}
