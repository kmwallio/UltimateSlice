use crate::model::clip::{
    BezierControls, Clip, ClipColorLabel, ClipKind, KeyframeInterpolation, NumericKeyframe,
    SlowMotionInterp,
};
use crate::model::project::{FrameRate, Project};
use crate::model::track::{Track, TrackHeightPreset};
use anyhow::{anyhow, bail, Result};
use quick_xml::escape::unescape;
use quick_xml::events::Event;
use quick_xml::Reader;
use quick_xml::Writer;
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;
use std::path::{Component, Path, PathBuf};

const MAX_IMPORTED_LINEAR_VOLUME: f64 = 3.981_071_705_5; // +12 dB

/// Represents a parsed FCPXML asset
struct Asset {
    #[allow(dead_code)]
    id: String,
    src: String,
    name: String,
    duration_ns: u64,
    start_ns: u64,
    has_video: bool,
    has_audio: bool,
    unknown_attrs: Vec<(String, String)>,
    unknown_children: Vec<String>,
}

#[derive(Clone)]
struct FormatSpec {
    width: u32,
    height: u32,
    frame_rate: FrameRate,
    unknown_attrs: Vec<(String, String)>,
}

struct AssetBuilder {
    id: String,
    src: Option<String>,
    name: String,
    duration_ns: u64,
    start_ns: u64,
    has_video: bool,
    has_audio: bool,
    unknown_attrs: Vec<(String, String)>,
    unknown_children: Vec<String>,
}

#[derive(Clone, Copy)]
struct ActiveClipContext {
    track_key: (u8, usize),
    clip_index: usize,
    timeline_start: u64,
    source_in: u64,
    /// Raw value of the clip's `start` attribute (timecoded source start).
    /// Used to map connected clip offsets from parent source time to timeline.
    raw_source_start_ns: u64,
    has_us_position: bool,
    has_us_scale: bool,
    has_us_rotate: bool,
    has_us_position_keyframes: bool,
    has_us_scale_keyframes: bool,
    has_us_rotate_keyframes: bool,
    has_us_opacity_keyframes: bool,
    has_us_volume_keyframes: bool,
    has_us_pan_keyframes: bool,
    has_us_speed: bool,
    has_us_speed_keyframes: bool,
    has_us_reverse: bool,
    has_us_freeze_frame: bool,
    has_us_freeze_source_ns: bool,
    has_us_freeze_hold_duration_ns: bool,
}

/// Parse an FCPXML string into a `Project`.
#[allow(dead_code)]
pub fn parse_fcpxml(xml: &str) -> Result<Project> {
    parse_fcpxml_with_path(xml, None)
}

/// Parse an FCPXML string into a `Project` with source file path context.
pub fn parse_fcpxml_with_path(xml: &str, fcpxml_path: Option<&Path>) -> Result<Project> {
    let sanitized_xml = sanitize_unescaped_keyframe_attr_json(xml);
    let mut reader = Reader::from_str(sanitized_xml.as_ref());
    reader.config_mut().trim_text(true);

    let mut assets: HashMap<String, Asset> = HashMap::new();
    let mut format_specs: HashMap<String, FormatSpec> = HashMap::new();
    let mut default_format: Option<FormatSpec> = None;
    let mut project = Project::new("Imported Project");
    // Clear default tracks — we'll add them from the FCPXML
    project.tracks.clear();

    // (kind_bucket, track_idx) → Track (kind_bucket: 0=video, 1=audio)
    let mut track_map: BTreeMap<(u8, usize), Track> = BTreeMap::new();
    let mut buf = Vec::new();
    let mut in_spine = false;
    let mut in_fcpxml = false;
    let mut in_resources = false;
    let mut in_library = false;
    let mut in_selected_event = false;
    let mut in_selected_project = false;
    let mut selected_event_seen = false;
    let mut selected_project_seen = false;
    let mut in_selected_sequence = false;
    let mut selected_sequence_seen = false;
    let mut selected_spine_seen = false;
    let mut selected_sequence_format_applied = false;
    let mut selected_sequence_format_ref: Option<String> = None;
    let mut current_asset: Option<AssetBuilder> = None;
    let mut clip_stack: Vec<ActiveClipContext> = Vec::new();
    let mut last_spine_clip_ctx: Option<ActiveClipContext> = None;
    let mount_root = fcpxml_path.and_then(fcpxml_mount_root);
    let mount_users = fcpxml_path
        .map(linux_mount_users_for_fcpxml)
        .unwrap_or_default();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(ref e) => {
                let name_local = e.local_name();
                let name = std::str::from_utf8(name_local.as_ref())?;

                match name {
                    "fcpxml" => {
                        let attrs = parse_attrs(e)?;
                        if let Some(version) = attrs.get("version") {
                            validate_fcpxml_version(version)?;
                        }
                        in_fcpxml = true;
                        project.fcpxml_unknown_root.attrs =
                            collect_unknown_attrs(&attrs, is_known_fcpxml_attr);
                    }
                    "resources" => {
                        let attrs = parse_attrs(e)?;
                        in_resources = true;
                        project.fcpxml_unknown_resources.attrs =
                            collect_unknown_attrs(&attrs, is_known_resources_attr);
                    }
                    "library" => {
                        let attrs = parse_attrs(e)?;
                        in_library = true;
                        project.fcpxml_unknown_library.attrs =
                            collect_unknown_attrs(&attrs, is_known_library_attr);
                    }
                    "event" if in_library => {
                        let attrs = parse_attrs(e)?;
                        if !selected_event_seen {
                            selected_event_seen = true;
                            in_selected_event = true;
                            project.fcpxml_unknown_event.attrs =
                                collect_unknown_attrs(&attrs, is_known_event_attr);
                        } else {
                            let fragment = collect_unknown_start_fragment(&mut reader, e)?;
                            project.fcpxml_unknown_library.children.push(fragment);
                        }
                    }
                    "project" => {
                        let attrs = parse_attrs(e)?;
                        if !selected_project_seen {
                            selected_project_seen = true;
                            in_selected_project = true;
                            if let Some(n) = attrs.get("name") {
                                project.title = n.clone();
                            }
                            project.fcpxml_unknown_project.attrs =
                                collect_unknown_attrs(&attrs, is_known_project_attr);
                        } else if in_selected_event {
                            let fragment = collect_unknown_start_fragment(&mut reader, e)?;
                            project.fcpxml_unknown_event.children.push(fragment);
                        } else if in_library {
                            let fragment = collect_unknown_start_fragment(&mut reader, e)?;
                            project.fcpxml_unknown_library.children.push(fragment);
                        } else {
                            let fragment = collect_unknown_start_fragment(&mut reader, e)?;
                            project.fcpxml_unknown_root.children.push(fragment);
                        }
                    }
                    "sequence" if in_selected_project => {
                        let attrs = parse_attrs(e)?;
                        if in_selected_project && !selected_sequence_seen {
                            selected_sequence_seen = true;
                            in_selected_sequence = true;
                            selected_sequence_format_ref = attrs.get("format").cloned();
                            if let Some(fmt_ref) = attrs.get("format") {
                                if let Some(spec) = format_specs.get(fmt_ref) {
                                    project.width = spec.width;
                                    project.height = spec.height;
                                    project.frame_rate = spec.frame_rate.clone();
                                    project.fcpxml_unknown_format.attrs =
                                        spec.unknown_attrs.clone();
                                    selected_sequence_format_applied = true;
                                }
                            }
                            project.fcpxml_unknown_sequence.attrs =
                                collect_unknown_attrs(&attrs, is_known_sequence_attr);
                        } else {
                            let fragment = collect_unknown_start_fragment(&mut reader, e)?;
                            project.fcpxml_unknown_project.children.push(fragment);
                        }
                    }
                    "format" => {
                        let attrs = parse_attrs(e)?;
                        if let Some((id, spec)) = parse_format_spec(&attrs) {
                            if selected_sequence_format_ref.as_deref() == Some(id.as_str()) {
                                project.fcpxml_unknown_format.attrs = spec.unknown_attrs.clone();
                            }
                            format_specs.insert(id, spec.clone());
                            if default_format.is_none() {
                                default_format = Some(spec);
                            }
                        }
                    }
                    "asset" => {
                        let attrs = parse_attrs(e)?;
                        current_asset = build_asset_builder(&attrs);
                        if let Some(asset) = current_asset.as_mut() {
                            if let Some(src) = attrs.get("src") {
                                asset.src = Some(parse_fcpxml_src_path(src));
                            }
                        }
                    }
                    "media-rep" => {
                        let attrs = parse_attrs(e)?;
                        if let Some(asset) = current_asset.as_mut() {
                            if asset.src.is_none() {
                                if let Some(src) = attrs.get("src") {
                                    asset.src = Some(parse_fcpxml_src_path(src));
                                }
                            }
                        }
                    }
                    "spine" if in_selected_sequence && !in_spine => {
                        let attrs = parse_attrs(e)?;
                        if !selected_spine_seen {
                            in_spine = true;
                            selected_spine_seen = true;
                            project.fcpxml_unknown_spine.attrs =
                                collect_unknown_attrs(&attrs, is_known_spine_attr);
                        } else {
                            let fragment = collect_unknown_start_fragment(&mut reader, e)?;
                            project.fcpxml_unknown_sequence.children.push(fragment);
                        }
                    }
                    "asset-clip" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        if let Some(ctx) = parse_asset_clip(
                            &attrs,
                            &assets,
                            &mut track_map,
                            mount_root.as_deref(),
                            &mount_users,
                            clip_stack.last(),
                        ) {
                            clip_stack.push(ctx);
                            last_spine_clip_ctx = Some(ctx);
                        }
                    }
                    "ref-clip" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        if let Some(ctx) = parse_asset_clip(
                            &attrs,
                            &assets,
                            &mut track_map,
                            mount_root.as_deref(),
                            &mount_users,
                            clip_stack.last(),
                        ) {
                            clip_stack.push(ctx);
                            last_spine_clip_ctx = Some(ctx);
                        }
                    }
                    "sync-clip" | "spine" if in_spine => {
                        // Transparent container: keep scanning nested clip items inside the
                        // selected sequence spine instead of treating the whole subtree as unknown.
                    }
                    "transition" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        apply_transition(&attrs, last_spine_clip_ctx.as_ref(), &mut track_map);
                        let _ = collect_unknown_start_fragment(&mut reader, e)?;
                    }
                    "timeMap" if in_spine => {
                        let fragment = collect_unknown_start_fragment(&mut reader, e)?;
                        let applied = parse_native_time_map_fragment(&fragment)
                            .map(|native| {
                                apply_native_time_map(&native, clip_stack.last(), &mut track_map)
                            })
                            .unwrap_or(false);
                        if !applied {
                            append_unknown_clip_child(fragment, clip_stack.last(), &mut track_map);
                        }
                    }
                    "adjust-transform" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        apply_adjust_transform(
                            &attrs,
                            clip_stack.last(),
                            &mut track_map,
                            project.width,
                            project.height,
                        );
                        // Parse native <param>/<keyframeAnimation>/<keyframe> children
                        let native = parse_adjust_transform_children(&mut reader)?;
                        if let Some(ctx) = clip_stack.last() {
                            apply_native_transform_keyframes(
                                &native,
                                ctx,
                                &mut track_map,
                                project.width,
                                project.height,
                            );
                            if let Some(clip) = current_clip_mut(&mut track_map, Some(ctx)) {
                                clip.fcpxml_unknown_children
                                    .extend(native.unknown_fragments);
                            }
                        }
                    }
                    "adjust-compositing" | "adjust-blend" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        apply_adjust_compositing(&attrs, clip_stack.last(), &mut track_map);
                        // Parse native opacity keyframes from children
                        let native = parse_adjust_blend_children(&mut reader)?;
                        if let Some(ctx) = clip_stack.last() {
                            apply_native_opacity_keyframes(&native, ctx, &mut track_map);
                            if let Some(clip) = current_clip_mut(&mut track_map, Some(ctx)) {
                                clip.fcpxml_unknown_children
                                    .extend(native.unknown_fragments);
                            }
                        }
                    }
                    "adjust-volume" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        apply_adjust_volume(&attrs, clip_stack.last(), &mut track_map);
                        // Parse native volume keyframes from children
                        let native = parse_adjust_volume_children(&mut reader)?;
                        if let Some(ctx) = clip_stack.last() {
                            apply_native_volume_keyframes(&native, ctx, &mut track_map);
                            if let Some(clip) = current_clip_mut(&mut track_map, Some(ctx)) {
                                clip.fcpxml_unknown_children
                                    .extend(native.unknown_fragments);
                            }
                        }
                    }
                    "adjust-panner" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        apply_adjust_panner(&attrs, clip_stack.last(), &mut track_map);
                        let native = parse_adjust_panner_children(&mut reader)?;
                        if let Some(ctx) = clip_stack.last() {
                            apply_native_pan_keyframes(&native, ctx, &mut track_map);
                            if let Some(clip) = current_clip_mut(&mut track_map, Some(ctx)) {
                                clip.fcpxml_unknown_children
                                    .extend(native.unknown_fragments);
                            }
                        }
                    }
                    "adjust-crop" | "crop-rect" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        apply_adjust_crop(&attrs, clip_stack.last(), &mut track_map);
                    }
                    "audio-channel-source" if in_spine && clip_stack.last().is_some() => {
                        // FCP nests volume/pan keyframes inside audio-channel-source.
                        let native = parse_audio_channel_source_children(&mut reader)?;
                        if let Some(ctx) = clip_stack.last() {
                            apply_native_volume_keyframes(&native, ctx, &mut track_map);
                            apply_native_pan_keyframes(&native, ctx, &mut track_map);
                        }
                    }
                    "marker" | "chapter-marker" if in_selected_sequence => {
                        let attrs = parse_attrs(e)?;
                        parse_sequence_marker(
                            &attrs,
                            clip_stack.last().map(|c| (c.timeline_start, c.source_in)),
                            &mut project,
                        );
                    }
                    "filter-video" if in_spine && clip_stack.last().is_some() => {
                        let fv_attrs = parse_attrs(e)?;
                        let fv_name = fv_attrs.get("name").map(|s| s.as_str()).unwrap_or("");
                        if fv_name == "Color Adjustments" {
                            parse_fcp_color_adjustments_filter(
                                &mut reader,
                                clip_stack.last(),
                                &mut track_map,
                            )?;
                        } else {
                            let fragment = collect_unknown_start_fragment(&mut reader, e)?;
                            append_unknown_clip_child(fragment, clip_stack.last(), &mut track_map);
                        }
                    }
                    _ if in_spine && clip_stack.last().is_some() => {
                        let fragment = collect_unknown_start_fragment(&mut reader, e)?;
                        append_unknown_clip_child(fragment, clip_stack.last(), &mut track_map);
                    }
                    _ if current_asset.is_some() => {
                        let fragment = collect_unknown_start_fragment(&mut reader, e)?;
                        if let Some(asset) = current_asset.as_mut() {
                            asset.unknown_children.push(fragment);
                        }
                    }
                    _ if in_spine => {
                        let fragment = collect_unknown_start_fragment(&mut reader, e)?;
                        project.fcpxml_unknown_spine.children.push(fragment);
                    }
                    _ if in_selected_sequence => {
                        let fragment = collect_unknown_start_fragment(&mut reader, e)?;
                        project.fcpxml_unknown_sequence.children.push(fragment);
                    }
                    _ if in_selected_project => {
                        let fragment = collect_unknown_start_fragment(&mut reader, e)?;
                        project.fcpxml_unknown_project.children.push(fragment);
                    }
                    _ if in_selected_event => {
                        let fragment = collect_unknown_start_fragment(&mut reader, e)?;
                        project.fcpxml_unknown_event.children.push(fragment);
                    }
                    _ if in_library => {
                        let fragment = collect_unknown_start_fragment(&mut reader, e)?;
                        project.fcpxml_unknown_library.children.push(fragment);
                    }
                    _ if in_resources => {
                        let fragment = collect_unknown_start_fragment(&mut reader, e)?;
                        project.fcpxml_unknown_resources.children.push(fragment);
                    }
                    _ if in_fcpxml => {
                        let fragment = collect_unknown_start_fragment(&mut reader, e)?;
                        project.fcpxml_unknown_root.children.push(fragment);
                    }
                    _ => {}
                }
            }
            Event::Empty(ref e) => {
                let name_local = e.local_name();
                let name = std::str::from_utf8(name_local.as_ref())?;

                match name {
                    "fcpxml" => {
                        let attrs = parse_attrs(e)?;
                        if let Some(version) = attrs.get("version") {
                            validate_fcpxml_version(version)?;
                        }
                        project.fcpxml_unknown_root.attrs =
                            collect_unknown_attrs(&attrs, is_known_fcpxml_attr);
                    }
                    "resources" => {
                        let attrs = parse_attrs(e)?;
                        project.fcpxml_unknown_resources.attrs =
                            collect_unknown_attrs(&attrs, is_known_resources_attr);
                    }
                    "library" => {
                        let attrs = parse_attrs(e)?;
                        project.fcpxml_unknown_library.attrs =
                            collect_unknown_attrs(&attrs, is_known_library_attr);
                    }
                    "event" if in_library => {
                        let attrs = parse_attrs(e)?;
                        if !selected_event_seen {
                            selected_event_seen = true;
                            project.fcpxml_unknown_event.attrs =
                                collect_unknown_attrs(&attrs, is_known_event_attr);
                        } else {
                            let fragment = collect_unknown_empty_fragment(e)?;
                            project.fcpxml_unknown_library.children.push(fragment);
                        }
                    }
                    "project" => {
                        let attrs = parse_attrs(e)?;
                        if !selected_project_seen {
                            selected_project_seen = true;
                            if let Some(n) = attrs.get("name") {
                                project.title = n.clone();
                            }
                            project.fcpxml_unknown_project.attrs =
                                collect_unknown_attrs(&attrs, is_known_project_attr);
                        } else if in_selected_event {
                            let fragment = collect_unknown_empty_fragment(e)?;
                            project.fcpxml_unknown_event.children.push(fragment);
                        } else if in_library {
                            let fragment = collect_unknown_empty_fragment(e)?;
                            project.fcpxml_unknown_library.children.push(fragment);
                        } else {
                            let fragment = collect_unknown_empty_fragment(e)?;
                            project.fcpxml_unknown_root.children.push(fragment);
                        }
                    }
                    "sequence" if in_selected_project => {
                        let attrs = parse_attrs(e)?;
                        if !selected_sequence_seen {
                            selected_sequence_seen = true;
                            selected_sequence_format_ref = attrs.get("format").cloned();
                            if let Some(fmt_ref) = attrs.get("format") {
                                if let Some(spec) = format_specs.get(fmt_ref) {
                                    project.width = spec.width;
                                    project.height = spec.height;
                                    project.frame_rate = spec.frame_rate.clone();
                                    project.fcpxml_unknown_format.attrs =
                                        spec.unknown_attrs.clone();
                                    selected_sequence_format_applied = true;
                                }
                            }
                            project.fcpxml_unknown_sequence.attrs =
                                collect_unknown_attrs(&attrs, is_known_sequence_attr);
                        } else {
                            let fragment = collect_unknown_empty_fragment(e)?;
                            project.fcpxml_unknown_project.children.push(fragment);
                        }
                    }
                    "format" => {
                        let attrs = parse_attrs(e)?;
                        if let Some((id, spec)) = parse_format_spec(&attrs) {
                            if selected_sequence_format_ref.as_deref() == Some(id.as_str()) {
                                project.fcpxml_unknown_format.attrs = spec.unknown_attrs.clone();
                            }
                            format_specs.insert(id, spec.clone());
                            if default_format.is_none() {
                                default_format = Some(spec);
                            }
                        }
                    }
                    "asset" => {
                        let attrs = parse_attrs(e)?;
                        if let Some(mut asset) = build_asset_builder(&attrs) {
                            if let Some(src) = attrs.get("src") {
                                asset.src = Some(parse_fcpxml_src_path(src));
                            }
                            finalize_asset(asset, &mut assets);
                        }
                    }
                    "media-rep" => {
                        let attrs = parse_attrs(e)?;
                        if let Some(asset) = current_asset.as_mut() {
                            if asset.src.is_none() {
                                if let Some(src) = attrs.get("src") {
                                    asset.src = Some(parse_fcpxml_src_path(src));
                                }
                            }
                        }
                    }
                    "asset-clip" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        if let Some(ctx) = parse_asset_clip(
                            &attrs,
                            &assets,
                            &mut track_map,
                            mount_root.as_deref(),
                            &mount_users,
                            clip_stack.last(),
                        ) {
                            last_spine_clip_ctx = Some(ctx);
                        }
                    }
                    "ref-clip" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        if let Some(ctx) = parse_asset_clip(
                            &attrs,
                            &assets,
                            &mut track_map,
                            mount_root.as_deref(),
                            &mount_users,
                            clip_stack.last(),
                        ) {
                            last_spine_clip_ctx = Some(ctx);
                        }
                    }
                    "sync-clip" | "spine" if in_spine => {}
                    "transition" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        apply_transition(&attrs, last_spine_clip_ctx.as_ref(), &mut track_map);
                    }
                    "timeMap" if in_spine => {
                        let fragment = collect_unknown_empty_fragment(e)?;
                        let applied = parse_native_time_map_fragment(&fragment)
                            .map(|native| {
                                apply_native_time_map(&native, clip_stack.last(), &mut track_map)
                            })
                            .unwrap_or(false);
                        if !applied {
                            append_unknown_clip_child(fragment, clip_stack.last(), &mut track_map);
                        }
                    }
                    "adjust-transform" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        apply_adjust_transform(
                            &attrs,
                            clip_stack.last(),
                            &mut track_map,
                            project.width,
                            project.height,
                        );
                    }
                    "adjust-compositing" | "adjust-blend" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        apply_adjust_compositing(&attrs, clip_stack.last(), &mut track_map);
                    }
                    "adjust-volume" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        apply_adjust_volume(&attrs, clip_stack.last(), &mut track_map);
                    }
                    "adjust-panner" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        apply_adjust_panner(&attrs, clip_stack.last(), &mut track_map);
                    }
                    "adjust-crop" | "crop-rect" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        apply_adjust_crop(&attrs, clip_stack.last(), &mut track_map);
                    }
                    "marker" | "chapter-marker" if in_selected_sequence => {
                        let attrs = parse_attrs(e)?;
                        parse_sequence_marker(
                            &attrs,
                            clip_stack.last().map(|c| (c.timeline_start, c.source_in)),
                            &mut project,
                        );
                    }
                    _ if in_spine && clip_stack.last().is_some() => {
                        let fragment = collect_unknown_empty_fragment(e)?;
                        append_unknown_clip_child(fragment, clip_stack.last(), &mut track_map);
                    }
                    _ if current_asset.is_some() => {
                        let fragment = collect_unknown_empty_fragment(e)?;
                        if let Some(asset) = current_asset.as_mut() {
                            asset.unknown_children.push(fragment);
                        }
                    }
                    _ if in_spine => {
                        let fragment = collect_unknown_empty_fragment(e)?;
                        project.fcpxml_unknown_spine.children.push(fragment);
                    }
                    _ if in_selected_sequence => {
                        let fragment = collect_unknown_empty_fragment(e)?;
                        project.fcpxml_unknown_sequence.children.push(fragment);
                    }
                    _ if in_selected_project => {
                        let fragment = collect_unknown_empty_fragment(e)?;
                        project.fcpxml_unknown_project.children.push(fragment);
                    }
                    _ if in_selected_event => {
                        let fragment = collect_unknown_empty_fragment(e)?;
                        project.fcpxml_unknown_event.children.push(fragment);
                    }
                    _ if in_library => {
                        let fragment = collect_unknown_empty_fragment(e)?;
                        project.fcpxml_unknown_library.children.push(fragment);
                    }
                    _ if in_resources => {
                        let fragment = collect_unknown_empty_fragment(e)?;
                        project.fcpxml_unknown_resources.children.push(fragment);
                    }
                    _ if in_fcpxml => {
                        let fragment = collect_unknown_empty_fragment(e)?;
                        project.fcpxml_unknown_root.children.push(fragment);
                    }
                    _ => {}
                }
            }
            Event::End(ref e) => {
                let name_local = e.local_name();
                let name = std::str::from_utf8(name_local.as_ref())?;
                match name {
                    "fcpxml" => {
                        in_fcpxml = false;
                    }
                    "resources" => {
                        in_resources = false;
                    }
                    "library" => {
                        in_library = false;
                    }
                    "event" => {
                        if in_selected_event {
                            in_selected_event = false;
                        }
                    }
                    "asset" => {
                        if let Some(asset) = current_asset.take() {
                            finalize_asset(asset, &mut assets);
                        }
                    }
                    "spine" => {
                        if in_spine {
                            in_spine = false;
                            clip_stack.clear();
                            last_spine_clip_ctx = None;
                        }
                    }
                    "asset-clip" | "ref-clip" => {
                        if in_spine {
                            clip_stack.pop();
                        }
                    }
                    "sequence" => {
                        if in_selected_sequence {
                            in_selected_sequence = false;
                        }
                    }
                    "project" => {
                        if in_selected_project {
                            in_selected_project = false;
                        }
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    if !selected_sequence_format_applied {
        if let Some(fmt_ref) = selected_sequence_format_ref.as_deref() {
            if let Some(spec) = format_specs.get(fmt_ref) {
                project.width = spec.width;
                project.height = spec.height;
                project.frame_rate = spec.frame_rate.clone();
                project.fcpxml_unknown_format.attrs = spec.unknown_attrs.clone();
                selected_sequence_format_applied = true;
            }
        }
    }
    if !selected_sequence_format_applied {
        if let Some(spec) = default_format {
            project.width = spec.width;
            project.height = spec.height;
            project.frame_rate = spec.frame_rate;
            project.fcpxml_unknown_format.attrs = spec.unknown_attrs;
        }
    }

    // Add tracks in index order, sorting clips once per track
    for ((_kind, _idx), mut track) in track_map {
        track.sort_clips();
        if !track.clips.is_empty() {
            project.tracks.push(track);
        }
    }

    if project.tracks.is_empty() {
        project.tracks.push(Track::new_video("Video 1"));
        project.tracks.push(Track::new_audio("Audio 1"));
    }

    project.source_fcpxml = Some(xml.to_string());
    Ok(project)
}

fn parse_asset_clip(
    attrs: &HashMap<String, String>,
    assets: &HashMap<String, Asset>,
    track_map: &mut BTreeMap<(u8, usize), Track>,
    mount_root: Option<&Path>,
    mount_users: &[String],
    parent_ctx: Option<&ActiveClipContext>,
) -> Option<ActiveClipContext> {
    if let Some(asset_ref) = attrs.get("ref") {
        if let Some(asset) = assets.get(asset_ref) {
            let raw_offset = attrs
                .get("offset")
                .and_then(|t| parse_fcpxml_time(t))
                .unwrap_or(0);
            let lane = attrs.get("lane").and_then(|s| s.parse::<i32>().ok());

            // Connected clips (lane != 0) use offset in parent's source time
            // space. Convert to timeline: parent_timeline + (offset - parent_start).
            let timeline_start = if lane.is_some() {
                if let Some(parent) = parent_ctx {
                    parent
                        .timeline_start
                        .saturating_add(raw_offset.saturating_sub(parent.raw_source_start_ns))
                } else {
                    raw_offset
                }
            } else {
                raw_offset
            };
            let raw_source_start = attrs
                .get("start")
                .and_then(|t| parse_fcpxml_time(t))
                .unwrap_or(asset.start_ns);
            let vendor_source_timecode_base_ns = attrs
                .get("us:source-timecode-base-ns")
                .and_then(|t| t.parse::<u64>().ok());
            let source_in = if let Some(base_ns) = vendor_source_timecode_base_ns {
                raw_source_start.saturating_sub(base_ns)
            } else if raw_source_start >= asset.start_ns {
                raw_source_start - asset.start_ns
            } else {
                raw_source_start
            };
            let duration = attrs
                .get("duration")
                .and_then(|t| parse_fcpxml_time(t))
                .unwrap_or(asset.duration_ns);
            let label = attrs
                .get("name")
                .cloned()
                .unwrap_or_else(|| asset.name.clone());

            let explicit_track_idx = attrs.get("us:track-idx").and_then(|s| s.parse().ok());
            let clip_kind = match attrs.get("us:track-kind").map(|s| s.as_str()) {
                Some("audio") => ClipKind::Audio,
                Some(_) => ClipKind::Video,
                None => {
                    if !asset.has_video && asset.has_audio {
                        ClipKind::Audio
                    } else if lane.unwrap_or(0) < 0 {
                        ClipKind::Audio
                    } else {
                        ClipKind::Video
                    }
                }
            };
            let inferred_track_idx = match clip_kind {
                ClipKind::Audio => lane
                    .filter(|l| *l < 0)
                    .map(|l| (-l - 1) as usize)
                    .unwrap_or(0),
                ClipKind::Video | ClipKind::Image | ClipKind::Title | ClipKind::Adjustment => {
                    lane.filter(|l| *l > 0).map(|l| l as usize).unwrap_or(0)
                }
            };
            let track_idx = explicit_track_idx.unwrap_or(inferred_track_idx);
            let track_name = attrs.get("us:track-name").cloned().unwrap_or_else(|| {
                if clip_kind == ClipKind::Audio {
                    format!("Audio {}", track_idx + 1)
                } else {
                    format!("Video {}", track_idx + 1)
                }
            });
            let track_muted = attrs
                .get("us:track-muted")
                .and_then(|s| s.parse::<bool>().ok())
                .unwrap_or(false);
            let track_locked = attrs
                .get("us:track-locked")
                .and_then(|s| s.parse::<bool>().ok())
                .unwrap_or(false);
            let track_soloed = attrs
                .get("us:track-soloed")
                .and_then(|s| s.parse::<bool>().ok())
                .unwrap_or(false);
            let track_height_preset = match attrs.get("us:track-height").map(|s| s.as_str()) {
                Some("small") => TrackHeightPreset::Small,
                Some("large") => TrackHeightPreset::Large,
                _ => TrackHeightPreset::Medium,
            };
            let track_key = (if clip_kind == ClipKind::Audio { 1 } else { 0 }, track_idx);

            // Get or create the target track
            let track = track_map.entry(track_key).or_insert_with(|| {
                let mut track = if clip_kind == ClipKind::Audio {
                    Track::new_audio(&track_name)
                } else {
                    Track::new_video(&track_name)
                };
                track.muted = track_muted;
                track.locked = track_locked;
                track.soloed = track_soloed;
                track.height_preset = track_height_preset;
                track
            });
            if attrs.contains_key("us:track-muted") {
                track.muted = track_muted;
            }
            if attrs.contains_key("us:track-locked") {
                track.locked = track_locked;
            }
            if attrs.contains_key("us:track-soloed") {
                track.soloed = track_soloed;
            }
            if attrs.contains_key("us:track-height") {
                track.height_preset = track_height_preset;
            }

            let resolved_source_path =
                resolve_import_source_path(&asset.src, mount_root, mount_users);
            let mut clip = Clip::new(
                &resolved_source_path,
                source_in.saturating_add(duration),
                timeline_start,
                clip_kind,
            );
            clip.source_in = source_in;
            clip.source_out = source_in.saturating_add(duration);
            clip.timeline_start = timeline_start;
            clip.label = label;
            clip.fcpxml_original_source_path = Some(asset.src.clone());
            clip.fcpxml_asset_ref = Some(asset_ref.clone());
            clip.fcpxml_unknown_asset_attrs = asset.unknown_attrs.clone();
            clip.fcpxml_unknown_asset_children = asset.unknown_children.clone();
            // Restore color/effects from vendor attributes
            if let Some(v) = attrs.get("us:brightness") {
                clip.brightness = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:contrast") {
                clip.contrast = v.parse().unwrap_or(1.0);
            }
            if let Some(v) = attrs.get("us:saturation") {
                clip.saturation = v.parse().unwrap_or(1.0);
            }
            if let Some(v) = attrs.get("us:color-label") {
                clip.color_label = match v.as_str() {
                    "red" => ClipColorLabel::Red,
                    "orange" => ClipColorLabel::Orange,
                    "yellow" => ClipColorLabel::Yellow,
                    "green" => ClipColorLabel::Green,
                    "teal" => ClipColorLabel::Teal,
                    "blue" => ClipColorLabel::Blue,
                    "purple" => ClipColorLabel::Purple,
                    "magenta" => ClipColorLabel::Magenta,
                    _ => ClipColorLabel::None,
                };
            }
            if let Some(v) = attrs.get("us:blend-mode") {
                clip.blend_mode = match v.as_str() {
                    "multiply" => crate::model::clip::BlendMode::Multiply,
                    "screen" => crate::model::clip::BlendMode::Screen,
                    "overlay" => crate::model::clip::BlendMode::Overlay,
                    "add" => crate::model::clip::BlendMode::Add,
                    "difference" => crate::model::clip::BlendMode::Difference,
                    "soft_light" => crate::model::clip::BlendMode::SoftLight,
                    _ => crate::model::clip::BlendMode::Normal,
                };
            }
            if let Some(v) = attrs.get("us:temperature") {
                clip.temperature = v.parse().unwrap_or(6500.0);
            }
            if let Some(v) = attrs.get("us:tint") {
                clip.tint = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:brightness-keyframes") {
                clip.brightness_keyframes =
                    serde_json::from_str::<Vec<NumericKeyframe>>(v).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:contrast-keyframes") {
                clip.contrast_keyframes =
                    serde_json::from_str::<Vec<NumericKeyframe>>(v).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:saturation-keyframes") {
                clip.saturation_keyframes =
                    serde_json::from_str::<Vec<NumericKeyframe>>(v).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:temperature-keyframes") {
                clip.temperature_keyframes =
                    serde_json::from_str::<Vec<NumericKeyframe>>(v).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:tint-keyframes") {
                clip.tint_keyframes =
                    serde_json::from_str::<Vec<NumericKeyframe>>(v).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:denoise") {
                clip.denoise = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:sharpness") {
                clip.sharpness = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:blur") {
                clip.blur = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:vidstab-enabled") {
                clip.vidstab_enabled = v == "true";
            }
            if let Some(v) = attrs.get("us:vidstab-smoothing") {
                clip.vidstab_smoothing = v.parse().unwrap_or(0.5);
            }
            if let Some(v) = attrs.get("us:blur-keyframes") {
                let json_str = v.replace("&quot;", "\"");
                clip.blur_keyframes = serde_json::from_str(&json_str).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:frei0r-effects") {
                // The writer escapes " → &quot; then XML serialization escapes
                // & → &amp;, producing &amp;quot; in the file.  quick_xml's
                // unescape decodes &amp;quot; → &quot; but not the second level.
                // Decode any remaining &quot; so JSON parsing succeeds.
                let json_str = v.replace("&quot;", "\"");
                clip.frei0r_effects = serde_json::from_str(&json_str).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:volume") {
                clip.volume = v.parse().unwrap_or(1.0);
            }
            if let Some(v) = attrs.get("us:volume-keyframes") {
                clip.volume_keyframes =
                    serde_json::from_str::<Vec<NumericKeyframe>>(v).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:pan") {
                clip.pan = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:pan-keyframes") {
                clip.pan_keyframes =
                    serde_json::from_str::<Vec<NumericKeyframe>>(v).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:eq-bands") {
                let json_str = v.replace("&quot;", "\"");
                if let Ok(bands) =
                    serde_json::from_str::<[crate::model::clip::EqBand; 3]>(&json_str)
                {
                    clip.eq_bands = bands;
                }
            }
            if let Some(v) = attrs.get("us:eq-low-gain-keyframes") {
                let json_str = v.replace("&quot;", "\"");
                clip.eq_low_gain_keyframes =
                    serde_json::from_str::<Vec<NumericKeyframe>>(&json_str).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:eq-mid-gain-keyframes") {
                let json_str = v.replace("&quot;", "\"");
                clip.eq_mid_gain_keyframes =
                    serde_json::from_str::<Vec<NumericKeyframe>>(&json_str).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:eq-high-gain-keyframes") {
                let json_str = v.replace("&quot;", "\"");
                clip.eq_high_gain_keyframes =
                    serde_json::from_str::<Vec<NumericKeyframe>>(&json_str).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:rotate-keyframes") {
                clip.rotate_keyframes =
                    serde_json::from_str::<Vec<NumericKeyframe>>(v).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:crop-left") {
                clip.crop_left = v.parse().unwrap_or(0);
            }
            if let Some(v) = attrs.get("us:crop-right") {
                clip.crop_right = v.parse().unwrap_or(0);
            }
            if let Some(v) = attrs.get("us:crop-top") {
                clip.crop_top = v.parse().unwrap_or(0);
            }
            if let Some(v) = attrs.get("us:crop-bottom") {
                clip.crop_bottom = v.parse().unwrap_or(0);
            }
            if let Some(v) = attrs.get("us:crop-left-keyframes") {
                clip.crop_left_keyframes =
                    serde_json::from_str::<Vec<NumericKeyframe>>(v).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:crop-right-keyframes") {
                clip.crop_right_keyframes =
                    serde_json::from_str::<Vec<NumericKeyframe>>(v).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:crop-top-keyframes") {
                clip.crop_top_keyframes =
                    serde_json::from_str::<Vec<NumericKeyframe>>(v).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:crop-bottom-keyframes") {
                clip.crop_bottom_keyframes =
                    serde_json::from_str::<Vec<NumericKeyframe>>(v).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:rotate") {
                clip.rotate = v.parse().unwrap_or(0);
            }
            if let Some(v) = attrs.get("us:flip-h") {
                clip.flip_h = v.parse().unwrap_or(false);
            }
            if let Some(v) = attrs.get("us:flip-v") {
                clip.flip_v = v.parse().unwrap_or(false);
            }
            if let Some(v) = attrs.get("us:scale") {
                clip.scale = v.parse().unwrap_or(1.0);
            }
            if let Some(v) = attrs.get("us:scale-keyframes") {
                clip.scale_keyframes =
                    serde_json::from_str::<Vec<NumericKeyframe>>(v).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:opacity") {
                clip.opacity = v.parse().unwrap_or(1.0);
            }
            if let Some(v) = attrs.get("us:opacity-keyframes") {
                clip.opacity_keyframes =
                    serde_json::from_str::<Vec<NumericKeyframe>>(v).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:position-x") {
                clip.position_x = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:position-x-keyframes") {
                clip.position_x_keyframes =
                    serde_json::from_str::<Vec<NumericKeyframe>>(v).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:position-y") {
                clip.position_y = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:position-y-keyframes") {
                clip.position_y_keyframes =
                    serde_json::from_str::<Vec<NumericKeyframe>>(v).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:title-text") {
                clip.title_text = v.clone();
            }
            if let Some(v) = attrs.get("us:title-font") {
                clip.title_font = v.clone();
            }
            if let Some(v) = attrs.get("us:title-color") {
                clip.title_color = u32::from_str_radix(v, 16).unwrap_or(0xFFFFFFFF);
            }
            if let Some(v) = attrs.get("us:title-x") {
                clip.title_x = v.parse().unwrap_or(0.5);
            }
            if let Some(v) = attrs.get("us:title-y") {
                clip.title_y = v.parse().unwrap_or(0.9);
            }
            if let Some(v) = attrs.get("us:title-template") {
                clip.title_template = v.clone();
            }
            if let Some(v) = attrs.get("us:title-outline-color") {
                clip.title_outline_color = u32::from_str_radix(v, 16).unwrap_or(0x000000FF);
            }
            if let Some(v) = attrs.get("us:title-outline-width") {
                clip.title_outline_width = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:title-shadow") {
                clip.title_shadow = v == "true" || v == "1";
            }
            if let Some(v) = attrs.get("us:title-shadow-color") {
                clip.title_shadow_color = u32::from_str_radix(v, 16).unwrap_or(0x000000AA);
            }
            if let Some(v) = attrs.get("us:title-shadow-offset-x") {
                clip.title_shadow_offset_x = v.parse().unwrap_or(2.0);
            }
            if let Some(v) = attrs.get("us:title-shadow-offset-y") {
                clip.title_shadow_offset_y = v.parse().unwrap_or(2.0);
            }
            if let Some(v) = attrs.get("us:title-bg-box") {
                clip.title_bg_box = v == "true" || v == "1";
            }
            if let Some(v) = attrs.get("us:title-bg-box-color") {
                clip.title_bg_box_color = u32::from_str_radix(v, 16).unwrap_or(0x00000088);
            }
            if let Some(v) = attrs.get("us:title-bg-box-padding") {
                clip.title_bg_box_padding = v.parse().unwrap_or(8.0);
            }
            if let Some(v) = attrs.get("us:title-clip-bg-color") {
                clip.title_clip_bg_color = u32::from_str_radix(v, 16).unwrap_or(0);
            }
            if let Some(v) = attrs.get("us:title-secondary-text") {
                clip.title_secondary_text = v.clone();
            }
            if let Some(v) = attrs.get("us:clip-kind") {
                match v.as_str() {
                    "title" => clip.kind = ClipKind::Title,
                    "adjustment" => clip.kind = ClipKind::Adjustment,
                    _ => {}
                }
            }
            if let Some(v) = attrs.get("us:speed") {
                clip.speed = v.parse().unwrap_or(1.0);
            }
            if let Some(v) = attrs.get("us:speed-keyframes") {
                clip.speed_keyframes =
                    serde_json::from_str::<Vec<NumericKeyframe>>(v).unwrap_or_default();
            }
            if let Some(v) = attrs.get("us:reverse") {
                clip.reverse = v.parse().unwrap_or(false);
            }
            if let Some(v) = attrs.get("us:slow-motion-interp") {
                clip.slow_motion_interp = match v.as_str() {
                    "blend" => SlowMotionInterp::Blend,
                    "optical-flow" => SlowMotionInterp::OpticalFlow,
                    _ => SlowMotionInterp::Off,
                };
            }
            if let Some(v) = attrs.get("us:freeze-frame") {
                clip.freeze_frame = v == "true" || v == "1";
            }
            if let Some(v) = attrs.get("us:freeze-source-ns") {
                clip.freeze_frame_source_ns = v.parse().ok();
            }
            if let Some(v) = attrs.get("us:freeze-hold-duration-ns") {
                clip.freeze_frame_hold_duration_ns = v.parse().ok();
            }
            if let Some(v) = attrs.get("us:group-id") {
                clip.group_id = if v.is_empty() { None } else { Some(v.clone()) };
            }
            if let Some(v) = attrs.get("us:link-group-id") {
                clip.link_group_id = if v.is_empty() { None } else { Some(v.clone()) };
            }
            clip.source_timecode_base_ns = vendor_source_timecode_base_ns.or_else(|| {
                if asset.start_ns > 0 {
                    Some(asset.start_ns)
                } else {
                    None
                }
            });
            if let Some(v) = attrs.get("us:shadows") {
                clip.shadows = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:midtones") {
                clip.midtones = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:highlights") {
                clip.highlights = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:exposure") {
                clip.exposure = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:black-point") {
                clip.black_point = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:highlights-warmth") {
                clip.highlights_warmth = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:highlights-tint") {
                clip.highlights_tint = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:midtones-warmth") {
                clip.midtones_warmth = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:midtones-tint") {
                clip.midtones_tint = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:shadows-warmth") {
                clip.shadows_warmth = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:shadows-tint") {
                clip.shadows_tint = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:chroma-key-enabled") {
                clip.chroma_key_enabled = v == "true" || v == "1";
            }
            if let Some(v) = attrs.get("us:chroma-key-color") {
                clip.chroma_key_color =
                    u32::from_str_radix(v.trim_start_matches("0x").trim_start_matches("0X"), 16)
                        .unwrap_or(0x00FF00);
            }
            if let Some(v) = attrs.get("us:chroma-key-tolerance") {
                clip.chroma_key_tolerance = v.parse().unwrap_or(0.3);
            }
            if let Some(v) = attrs.get("us:chroma-key-softness") {
                clip.chroma_key_softness = v.parse().unwrap_or(0.1);
            }
            if let Some(v) = attrs.get("us:bg-removal-enabled") {
                clip.bg_removal_enabled = v == "true" || v == "1";
            }
            if let Some(v) = attrs.get("us:bg-removal-threshold") {
                clip.bg_removal_threshold = v.parse().unwrap_or(0.5);
            }
            if let Some(v) = attrs.get("us:lut-paths") {
                // New multi-LUT format: JSON array of paths
                if let Ok(paths) = serde_json::from_str::<Vec<String>>(v) {
                    clip.lut_paths = paths;
                }
            } else if let Some(v) = attrs.get("us:lut-path") {
                // Backward compat: old single-LUT format
                clip.lut_paths = vec![v.clone()];
            }
            if let Some(v) = attrs.get("us:transition-after") {
                clip.transition_after = v.clone();
            }
            if let Some(v) = attrs.get("us:transition-after-ns") {
                clip.transition_after_ns = v.parse().unwrap_or(0);
            }
            for (k, v) in attrs {
                if !is_known_asset_clip_attr(k) {
                    clip.fcpxml_unknown_attrs.push((k.clone(), v.clone()));
                }
            }
            // The FCPXML `duration` attribute is the *timeline* duration.
            // For sped-up clips, the source range is larger: source_dur = timeline_dur × speed.
            // With speed keyframes, compute via integration over the timeline duration.
            if !clip.speed_keyframes.is_empty() {
                let timeline_dur = duration; // = source_out - source_in as originally parsed
                let source_dur =
                    clip.integrated_source_distance_for_local_timeline_ns(timeline_dur) as u64;
                clip.source_out = clip.source_in.saturating_add(source_dur);
            } else if (clip.speed - 1.0).abs() > 0.001 {
                let timeline_dur = duration as f64;
                let source_dur = (timeline_dur * clip.speed) as u64;
                clip.source_out = clip.source_in.saturating_add(source_dur);
            }

            let clip_index = track.clips.len();
            track.push_unsorted(clip);
            return Some(ActiveClipContext {
                track_key,
                clip_index,
                timeline_start,
                source_in,
                raw_source_start_ns: raw_source_start,
                has_us_position: attrs.contains_key("us:position-x")
                    || attrs.contains_key("us:position-y"),
                has_us_scale: attrs.contains_key("us:scale"),
                has_us_rotate: attrs.contains_key("us:rotate"),
                has_us_position_keyframes: attrs.contains_key("us:position-x-keyframes")
                    || attrs.contains_key("us:position-y-keyframes"),
                has_us_scale_keyframes: attrs.contains_key("us:scale-keyframes"),
                has_us_rotate_keyframes: attrs.contains_key("us:rotate-keyframes"),
                has_us_opacity_keyframes: attrs.contains_key("us:opacity-keyframes"),
                has_us_volume_keyframes: attrs.contains_key("us:volume-keyframes"),
                has_us_pan_keyframes: attrs.contains_key("us:pan-keyframes"),
                has_us_speed: attrs.contains_key("us:speed"),
                has_us_speed_keyframes: attrs.contains_key("us:speed-keyframes"),
                has_us_reverse: attrs.contains_key("us:reverse"),
                has_us_freeze_frame: attrs.contains_key("us:freeze-frame"),
                has_us_freeze_source_ns: attrs.contains_key("us:freeze-source-ns"),
                has_us_freeze_hold_duration_ns: attrs.contains_key("us:freeze-hold-duration-ns"),
            });
        }
    }
    None
}

fn current_clip_mut<'a>(
    track_map: &'a mut BTreeMap<(u8, usize), Track>,
    active_ctx: Option<&ActiveClipContext>,
) -> Option<&'a mut Clip> {
    let ctx = active_ctx?;
    track_map
        .get_mut(&ctx.track_key)
        .and_then(|track| track.clips.get_mut(ctx.clip_index))
}

fn append_unknown_clip_child(
    fragment: String,
    active_ctx: Option<&ActiveClipContext>,
    track_map: &mut BTreeMap<(u8, usize), Track>,
) {
    if let Some(clip) = current_clip_mut(track_map, active_ctx) {
        clip.fcpxml_unknown_children.push(fragment);
    }
}

/// Parse FCP's `<filter-video name="Color Adjustments">` and apply param values
/// to the active clip's color fields. FCP values are −100..100; UltimateSlice
/// uses −1.0..1.0 for most fields, 0.0..2.0 for contrast/saturation.
fn parse_fcp_color_adjustments_filter(
    reader: &mut Reader<&[u8]>,
    active_ctx: Option<&ActiveClipContext>,
    track_map: &mut BTreeMap<(u8, usize), Track>,
) -> Result<()> {
    let mut buf = Vec::new();
    let mut depth = 1usize;
    // Collect param key→value pairs first, then apply to clip.
    let mut params: HashMap<String, f32> = HashMap::new();
    while depth > 0 {
        match reader.read_event_into(&mut buf)? {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::Empty(ref e) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag == "param" {
                    let attrs = parse_attrs(e)?;
                    if let (Some(key), Some(val)) = (attrs.get("key"), attrs.get("value")) {
                        if let Ok(v) = val.parse::<f32>() {
                            params.insert(key.clone(), v);
                        }
                    }
                }
            }
            Event::Eof => bail!("Unexpected EOF in filter-video Color Adjustments"),
            _ => {}
        }
        buf.clear();
    }
    if let Some(clip) = current_clip_mut(track_map, active_ctx) {
        // Map FCP params by key number to clip fields.
        // FCP range: −100..100; US range: /100 (most) or /100+1 (contrast, saturation).
        if let Some(&v) = params.get("3") {
            clip.exposure = (v / 100.0).clamp(-1.0, 1.0);
        }
        if let Some(&v) = params.get("2") {
            clip.brightness = (v / 100.0).clamp(-1.0, 1.0);
        }
        if let Some(&v) = params.get("17") {
            clip.contrast = (v / 100.0 + 1.0).clamp(0.0, 2.0);
        }
        if let Some(&v) = params.get("16") {
            clip.saturation = (v / 100.0 + 1.0).clamp(0.0, 2.0);
        }
        if let Some(&v) = params.get("7") {
            clip.highlights = (v / 100.0).clamp(-1.0, 1.0);
        }
        if let Some(&v) = params.get("1") {
            clip.black_point = (v / 100.0).clamp(-1.0, 1.0);
        }
        if let Some(&v) = params.get("4") {
            clip.shadows = (v / 100.0).clamp(-1.0, 1.0);
        }
        if let Some(&v) = params.get("10") {
            clip.highlights_warmth = (v / 100.0).clamp(-1.0, 1.0);
        }
        if let Some(&v) = params.get("11") {
            clip.highlights_tint = (v / 100.0).clamp(-1.0, 1.0);
        }
        if let Some(&v) = params.get("12") {
            clip.midtones_warmth = (v / 100.0).clamp(-1.0, 1.0);
        }
        if let Some(&v) = params.get("13") {
            clip.midtones_tint = (v / 100.0).clamp(-1.0, 1.0);
        }
        if let Some(&v) = params.get("14") {
            clip.shadows_warmth = (v / 100.0).clamp(-1.0, 1.0);
        }
        if let Some(&v) = params.get("15") {
            clip.shadows_tint = (v / 100.0).clamp(-1.0, 1.0);
        }
    }
    Ok(())
}

fn apply_adjust_transform(
    attrs: &HashMap<String, String>,
    active_ctx: Option<&ActiveClipContext>,
    track_map: &mut BTreeMap<(u8, usize), Track>,
    project_width: u32,
    project_height: u32,
) {
    let Some(ctx) = active_ctx else {
        return;
    };
    if let Some(clip) = current_clip_mut(track_map, Some(ctx)) {
        let parsed_scale = attrs.get("scale").and_then(|s| parse_vec2(s)).map(|s| s.0);
        let effective_scale_for_position = if ctx.has_us_scale {
            clip.scale
        } else {
            parsed_scale.unwrap_or(clip.scale)
        };
        if !ctx.has_us_position {
            if let Some(pos) = attrs.get("position").and_then(|s| parse_vec2(s)) {
                let (x, y) = fcpxml_position_to_internal(
                    pos.0,
                    pos.1,
                    project_width,
                    project_height,
                    effective_scale_for_position,
                );
                clip.position_x = x;
                clip.position_y = y;
            }
        }
        if !ctx.has_us_scale {
            if let Some(scale) = parsed_scale {
                clip.scale = scale;
            }
        }
        if !ctx.has_us_rotate {
            if let Some(rot) = attrs.get("rotation").and_then(|s| s.parse::<f64>().ok()) {
                clip.rotate = rot.round() as i32;
            }
        }
    }
}

fn fcpxml_position_to_internal(
    x: f64,
    y: f64,
    project_width: u32,
    project_height: u32,
    scale: f64,
) -> (f64, f64) {
    if project_width == 0 || project_height == 0 {
        return (x, y);
    }
    // FCPXML position values are frame-percentage offsets based on frame height
    // for both axes (center-origin): 100 means one frame-height of shift.
    let shift_x_px = x * (project_height as f64) / 100.0;
    let shift_y_px = -y * (project_height as f64) / 100.0;
    let range_x = (project_width as f64) * (1.0 - scale) / 2.0;
    let range_y = (project_height as f64) * (1.0 - scale) / 2.0;
    if range_x.abs() < f64::EPSILON || range_y.abs() < f64::EPSILON {
        let fallback_range_x = project_width as f64 / 2.0;
        let fallback_range_y = project_height as f64 / 2.0;
        return (shift_x_px / fallback_range_x, shift_y_px / fallback_range_y);
    }
    (shift_x_px / range_x, shift_y_px / range_y)
}

fn apply_adjust_compositing(
    attrs: &HashMap<String, String>,
    active_ctx: Option<&ActiveClipContext>,
    track_map: &mut BTreeMap<(u8, usize), Track>,
) {
    if let Some(clip) = current_clip_mut(track_map, active_ctx) {
        if let Some(opacity) = attrs.get("opacity").and_then(|s| s.parse::<f64>().ok()) {
            clip.opacity = opacity;
        }
    }
}

fn apply_adjust_volume(
    attrs: &HashMap<String, String>,
    active_ctx: Option<&ActiveClipContext>,
    track_map: &mut BTreeMap<(u8, usize), Track>,
) {
    if let Some(clip) = current_clip_mut(track_map, active_ctx) {
        if let Some(volume) = attrs
            .get("amount")
            .and_then(|s| parse_fcpxml_volume_amount(s))
        {
            clip.volume = volume as f32;
        }
    }
}

fn apply_adjust_panner(
    attrs: &HashMap<String, String>,
    active_ctx: Option<&ActiveClipContext>,
    track_map: &mut BTreeMap<(u8, usize), Track>,
) {
    if let Some(clip) = current_clip_mut(track_map, active_ctx) {
        if let Some(pan) = attrs.get("amount").and_then(|s| parse_fcpxml_pan_amount(s)) {
            clip.pan = pan as f32;
        }
    }
}

fn transition_kind_from_name(name: &str) -> Option<&'static str> {
    let token: String = name
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect();
    match token.as_str() {
        "crossdissolve" | "dissolve" | "fade" => Some("cross_dissolve"),
        "fadetoblack" | "fadeblack" => Some("fade_to_black"),
        "wiperight" => Some("wipe_right"),
        "wipeleft" => Some("wipe_left"),
        _ => None,
    }
}

fn apply_transition(
    attrs: &HashMap<String, String>,
    last_spine_clip_ctx: Option<&ActiveClipContext>,
    track_map: &mut BTreeMap<(u8, usize), Track>,
) {
    let Some(clip) = current_clip_mut(track_map, last_spine_clip_ctx) else {
        return;
    };
    let duration_ns = attrs
        .get("duration")
        .and_then(|s| parse_fcpxml_time(s))
        .unwrap_or(0);
    if duration_ns == 0 {
        return;
    }
    let kind = attrs
        .get("name")
        .and_then(|name| transition_kind_from_name(name))
        .unwrap_or("cross_dissolve");
    clip.transition_after = kind.to_string();
    clip.transition_after_ns = duration_ns;
}

#[derive(Clone, Copy)]
struct NativeTimeMapPoint {
    time_ns: u64,
    value_ns: u64,
    interp: KeyframeInterpolation,
}

struct NativeTimeMap {
    points: Vec<NativeTimeMapPoint>,
    has_curve_timing: bool,
    has_unsupported_interp: bool,
}

fn time_map_interp_from_fcpxml(interp: &str) -> Option<KeyframeInterpolation> {
    match interp {
        "linear" => Some(KeyframeInterpolation::Linear),
        "smooth2" | "smooth" => Some(KeyframeInterpolation::EaseInOut),
        // Be permissive with non-timeMap values if encountered in wild files.
        "easeIn" => Some(KeyframeInterpolation::EaseIn),
        "easeOut" => Some(KeyframeInterpolation::EaseOut),
        "ease" => Some(KeyframeInterpolation::EaseInOut),
        _ => None,
    }
}

fn parse_native_time_map_fragment(fragment: &str) -> Option<NativeTimeMap> {
    let mut reader = Reader::from_str(fragment);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut points = Vec::new();
    let mut has_curve_timing = false;
    let mut has_unsupported_interp = false;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                if e.local_name().as_ref() == b"timept" {
                    let attrs = parse_attrs(e).ok()?;
                    let time_ns = attrs.get("time").and_then(|s| parse_fcpxml_time(s))?;
                    let value_ns = attrs.get("value").and_then(|s| parse_fcpxml_time(s))?;
                    let interp = attrs
                        .get("interp")
                        .and_then(|s| time_map_interp_from_fcpxml(s))
                        .unwrap_or_else(|| {
                            if attrs.contains_key("interp") {
                                has_unsupported_interp = true;
                            }
                            KeyframeInterpolation::Linear
                        });
                    has_curve_timing |=
                        attrs.contains_key("inTime") || attrs.contains_key("outTime");
                    points.push(NativeTimeMapPoint {
                        time_ns,
                        value_ns,
                        interp,
                    });
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => return None,
        }
        buf.clear();
    }
    Some(NativeTimeMap {
        points,
        has_curve_timing,
        has_unsupported_interp,
    })
}

fn apply_native_time_map(
    native: &NativeTimeMap,
    active_ctx: Option<&ActiveClipContext>,
    track_map: &mut BTreeMap<(u8, usize), Track>,
) -> bool {
    const EPS: f64 = 1e-9;
    let Some(ctx) = active_ctx else {
        return false;
    };
    if native.has_curve_timing || native.has_unsupported_interp {
        return false;
    }
    if ctx.has_us_speed
        || ctx.has_us_speed_keyframes
        || ctx.has_us_reverse
        || ctx.has_us_freeze_frame
        || ctx.has_us_freeze_source_ns
        || ctx.has_us_freeze_hold_duration_ns
    {
        return false;
    }
    if native.points.len() < 2 {
        return false;
    }
    let mut points = native.points.clone();
    points.sort_by_key(|p| p.time_ns);
    for window in points.windows(2) {
        if window[1].time_ns <= window[0].time_ns {
            return false;
        }
    }
    let Some(clip) = current_clip_mut(track_map, Some(ctx)) else {
        return false;
    };
    let first = points[0];
    let last = *points.last().unwrap_or(&first);

    let mut segment_speeds = Vec::with_capacity(points.len().saturating_sub(1));
    let mut segment_signs = Vec::with_capacity(points.len().saturating_sub(1));
    for window in points.windows(2) {
        let p0 = window[0];
        let p1 = window[1];
        let dt = p1.time_ns.saturating_sub(p0.time_ns);
        if dt == 0 {
            return false;
        }
        let dv = p1.value_ns as i128 - p0.value_ns as i128;
        let speed = (dv.unsigned_abs() as f64) / dt as f64;
        if !speed.is_finite() {
            return false;
        }
        segment_speeds.push(speed);
        segment_signs.push(dv.signum());
    }

    let has_nonzero_segments = segment_signs.iter().any(|sign| *sign != 0);
    if !has_nonzero_segments {
        if clip.kind == ClipKind::Audio {
            return false;
        }
        let source_ns = if first.value_ns >= clip.source_in && first.value_ns <= clip.source_out {
            first.value_ns
        } else {
            clip.source_in.saturating_add(first.value_ns)
        };
        clip.speed = 1.0;
        clip.speed_keyframes.clear();
        clip.reverse = false;
        clip.freeze_frame = true;
        clip.freeze_frame_source_ns = Some(source_ns);
        clip.freeze_frame_hold_duration_ns = Some(last.time_ns.saturating_sub(first.time_ns));
        return true;
    }

    if segment_signs.iter().any(|sign| *sign == 0) {
        return false;
    }
    let reverse = segment_signs[0] < 0;
    if segment_signs
        .iter()
        .any(|sign| (*sign < 0) != reverse || *sign == 0)
    {
        return false;
    }
    if segment_speeds
        .iter()
        .any(|speed| *speed <= 0.0 || !speed.is_finite())
    {
        return false;
    }

    let first_speed = segment_speeds[0];
    let all_same_speed = segment_speeds
        .iter()
        .all(|speed| (*speed - first_speed).abs() <= EPS);
    clip.speed = first_speed;
    if all_same_speed {
        clip.speed_keyframes.clear();
    } else {
        let base_time_ns = first.time_ns;
        let mut speed_keyframes = Vec::new();
        speed_keyframes.push(NumericKeyframe {
            time_ns: 0,
            value: first_speed,
            interpolation: points[0].interp,
            bezier_controls: None,
        });
        for segment_idx in 1..segment_speeds.len() {
            let boundary_time_ns = points[segment_idx].time_ns.saturating_sub(base_time_ns);
            let incoming = segment_speeds[segment_idx - 1];
            let outgoing = segment_speeds[segment_idx];
            if (incoming - outgoing).abs() <= EPS {
                continue;
            }
            speed_keyframes.push(NumericKeyframe {
                time_ns: boundary_time_ns,
                value: incoming,
                interpolation: points[segment_idx - 1].interp,
                bezier_controls: None,
            });
            speed_keyframes.push(NumericKeyframe {
                time_ns: boundary_time_ns,
                value: outgoing,
                interpolation: points[segment_idx].interp,
                bezier_controls: None,
            });
        }
        clip.speed_keyframes = speed_keyframes;
    }
    clip.reverse = reverse;
    clip.freeze_frame = false;
    clip.freeze_frame_source_ns = None;
    clip.freeze_frame_hold_duration_ns = None;
    true
}

fn parse_fcpxml_volume_amount(value: &str) -> Option<f64> {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();
    if let Some(db) = lower.strip_suffix("db") {
        let db: f64 = db.trim().parse().ok()?;
        if db <= -95.0 {
            return Some(0.0);
        }
        return Some(
            (10.0f64)
                .powf(db / 20.0)
                .clamp(0.0, MAX_IMPORTED_LINEAR_VOLUME),
        );
    }
    trimmed
        .parse::<f64>()
        .ok()
        .map(|v| v.clamp(0.0, MAX_IMPORTED_LINEAR_VOLUME))
}

fn parse_fcpxml_pan_amount(value: &str) -> Option<f64> {
    let trimmed = value.trim();
    if let Some(percent) = trimmed.strip_suffix('%') {
        let p = percent.trim().parse::<f64>().ok()?;
        return Some((p / 100.0).clamp(-1.0, 1.0));
    }
    trimmed.parse::<f64>().ok().map(|v| v.clamp(-1.0, 1.0))
}

fn apply_adjust_crop(
    attrs: &HashMap<String, String>,
    active_ctx: Option<&ActiveClipContext>,
    track_map: &mut BTreeMap<(u8, usize), Track>,
) {
    if let Some(clip) = current_clip_mut(track_map, active_ctx) {
        if let Some(v) = attrs.get("left").and_then(|s| parse_crop_value(s)) {
            clip.crop_left = v;
        }
        if let Some(v) = attrs.get("right").and_then(|s| parse_crop_value(s)) {
            clip.crop_right = v;
        }
        if let Some(v) = attrs.get("top").and_then(|s| parse_crop_value(s)) {
            clip.crop_top = v;
        }
        if let Some(v) = attrs.get("bottom").and_then(|s| parse_crop_value(s)) {
            clip.crop_bottom = v;
        }
    }
}

/// Keyframe data parsed from native FCPXML `<param>/<keyframeAnimation>/<keyframe>` elements.
#[derive(Default)]
struct NativeKeyframeParams {
    position_keyframes: Vec<(u64, f64, f64, KeyframeInterpolation, Option<BezierControls>)>, // (time_ns, fcpxml_x, fcpxml_y, interp, bezier_controls)
    scale_keyframes: Vec<NumericKeyframe>,
    rotation_keyframes: Vec<NumericKeyframe>,
    opacity_keyframes: Vec<NumericKeyframe>,
    volume_keyframes: Vec<NumericKeyframe>,
    pan_keyframes: Vec<NumericKeyframe>,
    unknown_fragments: Vec<String>,
}

/// Parse children of an `<adjust-transform>` Start element until its matching End.
/// Extracts `<param name="position">` and `<param name="scale">` keyframes;
/// collects other children as unknown fragments for round-trip preservation.
fn parse_adjust_transform_children(reader: &mut Reader<&[u8]>) -> Result<NativeKeyframeParams> {
    let mut result = NativeKeyframeParams::default();
    let mut buf = Vec::new();
    let mut depth = 1usize;

    while depth > 0 {
        match reader.read_event_into(&mut buf)? {
            Event::Start(ref e) => {
                let local_name = e.local_name();
                let name = std::str::from_utf8(local_name.as_ref())?;
                if name == "param" && depth == 1 {
                    let attrs = parse_attrs(e)?;
                    let param_name = attrs.get("name").cloned().unwrap_or_default();
                    let param_name_lower = param_name.to_ascii_lowercase();
                    if param_name_lower == "position" {
                        let kfs = parse_keyframe_animation_children(reader)?;
                        for (time_ns, val_str, interp, bezier_controls) in &kfs {
                            if let Some((x, y)) = parse_vec2(val_str) {
                                result
                                    .position_keyframes
                                    .push((*time_ns, x, y, *interp, *bezier_controls));
                            }
                        }
                    } else if param_name_lower == "scale" {
                        let kfs = parse_keyframe_animation_children(reader)?;
                        for (time_ns, val_str, interp, bezier_controls) in &kfs {
                            if let Some((sx, _sy)) = parse_vec2(val_str) {
                                result.scale_keyframes.push(NumericKeyframe {
                                    time_ns: *time_ns,
                                    value: sx,
                                    interpolation: *interp,
                                    bezier_controls: *bezier_controls,
                                });
                            } else if let Ok(s) = val_str.parse::<f64>() {
                                result.scale_keyframes.push(NumericKeyframe {
                                    time_ns: *time_ns,
                                    value: s,
                                    interpolation: *interp,
                                    bezier_controls: *bezier_controls,
                                });
                            }
                        }
                    } else if param_name_lower == "rotation" {
                        let kfs = parse_keyframe_animation_children(reader)?;
                        for (time_ns, val_str, interp, bezier_controls) in &kfs {
                            if let Ok(r) = val_str.parse::<f64>() {
                                result.rotation_keyframes.push(NumericKeyframe {
                                    time_ns: *time_ns,
                                    value: r,
                                    interpolation: *interp,
                                    bezier_controls: *bezier_controls,
                                });
                            }
                        }
                    } else {
                        // Unknown param — collect as fragment
                        let fragment = collect_unknown_start_fragment_from_attrs(reader, e)?;
                        result.unknown_fragments.push(fragment);
                    }
                } else {
                    depth += 1;
                    let fragment = collect_remaining_start_fragment(reader, e, depth)?;
                    depth = 1; // collect_remaining consumed to matching depth
                    result.unknown_fragments.push(fragment);
                }
            }
            Event::Empty(ref e) => {
                if depth == 1 {
                    let fragment = collect_unknown_empty_fragment(e)?;
                    result.unknown_fragments.push(fragment);
                }
            }
            Event::End(ref _e) => {
                depth = depth.saturating_sub(1);
            }
            Event::Eof => bail!("Unexpected EOF inside <adjust-transform>"),
            _ => {}
        }
        buf.clear();
    }
    Ok(result)
}

/// Parse children of an `<adjust-blend>` or `<adjust-compositing>` Start element.
/// Extracts `<param name="amount">` keyframes for opacity.
fn parse_adjust_blend_children(reader: &mut Reader<&[u8]>) -> Result<NativeKeyframeParams> {
    let mut result = NativeKeyframeParams::default();
    let mut buf = Vec::new();
    let mut depth = 1usize;

    while depth > 0 {
        match reader.read_event_into(&mut buf)? {
            Event::Start(ref e) => {
                let local_name = e.local_name();
                let name = std::str::from_utf8(local_name.as_ref())?;
                if name == "param" && depth == 1 {
                    let attrs = parse_attrs(e)?;
                    let param_name = attrs.get("name").cloned().unwrap_or_default();
                    let param_name_lower = param_name.to_ascii_lowercase();
                    if param_name_lower == "amount" || param_name_lower == "opacity" {
                        let kfs = parse_keyframe_animation_children(reader)?;
                        for (time_ns, val_str, interp, bezier_controls) in &kfs {
                            if let Ok(v) = val_str.parse::<f64>() {
                                result.opacity_keyframes.push(NumericKeyframe {
                                    time_ns: *time_ns,
                                    value: v,
                                    interpolation: *interp,
                                    bezier_controls: *bezier_controls,
                                });
                            }
                        }
                    } else {
                        let fragment = collect_unknown_start_fragment_from_attrs(reader, e)?;
                        result.unknown_fragments.push(fragment);
                    }
                } else {
                    depth += 1;
                    let fragment = collect_remaining_start_fragment(reader, e, depth)?;
                    depth = 1;
                    result.unknown_fragments.push(fragment);
                }
            }
            Event::Empty(ref e) => {
                if depth == 1 {
                    let fragment = collect_unknown_empty_fragment(e)?;
                    result.unknown_fragments.push(fragment);
                }
            }
            Event::End(ref _e) => {
                depth = depth.saturating_sub(1);
            }
            Event::Eof => bail!("Unexpected EOF inside <adjust-blend>"),
            _ => {}
        }
        buf.clear();
    }
    Ok(result)
}

/// Parse children of `<audio-channel-source>`.
/// FCP nests `<adjust-volume>` and `<adjust-panner>` (with keyframes) inside this element.
fn parse_audio_channel_source_children(reader: &mut Reader<&[u8]>) -> Result<NativeKeyframeParams> {
    let mut result = NativeKeyframeParams::default();
    let mut buf = Vec::new();
    let mut depth = 1usize;

    while depth > 0 {
        match reader.read_event_into(&mut buf)? {
            Event::Start(ref e) => {
                let local_name = e.local_name();
                let name = std::str::from_utf8(local_name.as_ref())?;
                match name {
                    "adjust-volume" if depth == 1 => {
                        let child = parse_adjust_volume_children(reader)?;
                        result.volume_keyframes.extend(child.volume_keyframes);
                    }
                    "adjust-panner" if depth == 1 => {
                        let child = parse_adjust_panner_children(reader)?;
                        result.pan_keyframes.extend(child.pan_keyframes);
                    }
                    _ => {
                        // Skip unknown child element entirely
                        let mut skip_depth = 1usize;
                        let mut skip_buf = Vec::new();
                        while skip_depth > 0 {
                            match reader.read_event_into(&mut skip_buf)? {
                                Event::Start(_) => skip_depth += 1,
                                Event::End(_) => skip_depth -= 1,
                                Event::Eof => {
                                    bail!("Unexpected EOF skipping inside <audio-channel-source>")
                                }
                                _ => {}
                            }
                            skip_buf.clear();
                        }
                    }
                }
            }
            Event::Empty(_) => {}
            Event::End(_) => {
                depth = depth.saturating_sub(1);
            }
            Event::Eof => bail!("Unexpected EOF inside <audio-channel-source>"),
            _ => {}
        }
        buf.clear();
    }
    Ok(result)
}

/// Parse children of an `<adjust-volume>` Start element.
/// Extracts `<param name="amount">` keyframes for volume (dB values).
fn parse_adjust_volume_children(reader: &mut Reader<&[u8]>) -> Result<NativeKeyframeParams> {
    let mut result = NativeKeyframeParams::default();
    let mut buf = Vec::new();
    let mut depth = 1usize;

    while depth > 0 {
        match reader.read_event_into(&mut buf)? {
            Event::Start(ref e) => {
                let local_name = e.local_name();
                let name = std::str::from_utf8(local_name.as_ref())?;
                if name == "param" && depth == 1 {
                    let attrs = parse_attrs(e)?;
                    let param_name = attrs.get("name").cloned().unwrap_or_default();
                    let param_name_lower = param_name.to_ascii_lowercase();
                    if param_name_lower == "amount" || param_name_lower == "volume" {
                        let kfs = parse_keyframe_animation_children(reader)?;
                        for (time_ns, val_str, interp, bezier_controls) in &kfs {
                            if let Some(linear) = parse_fcpxml_volume_amount(val_str) {
                                result.volume_keyframes.push(NumericKeyframe {
                                    time_ns: *time_ns,
                                    value: linear,
                                    interpolation: *interp,
                                    bezier_controls: *bezier_controls,
                                });
                            }
                        }
                    } else {
                        let fragment = collect_unknown_start_fragment_from_attrs(reader, e)?;
                        result.unknown_fragments.push(fragment);
                    }
                } else {
                    depth += 1;
                    let fragment = collect_remaining_start_fragment(reader, e, depth)?;
                    depth = 1;
                    result.unknown_fragments.push(fragment);
                }
            }
            Event::Empty(ref e) => {
                if depth == 1 {
                    let fragment = collect_unknown_empty_fragment(e)?;
                    result.unknown_fragments.push(fragment);
                }
            }
            Event::End(ref _e) => {
                depth = depth.saturating_sub(1);
            }
            Event::Eof => bail!("Unexpected EOF inside <adjust-volume>"),
            _ => {}
        }
        buf.clear();
    }
    Ok(result)
}

/// Parse children of an `<adjust-panner>` Start element.
/// Extracts `<param name="amount">` keyframes for pan values (-1..1).
fn parse_adjust_panner_children(reader: &mut Reader<&[u8]>) -> Result<NativeKeyframeParams> {
    let mut result = NativeKeyframeParams::default();
    let mut buf = Vec::new();
    let mut depth = 1usize;

    while depth > 0 {
        match reader.read_event_into(&mut buf)? {
            Event::Start(ref e) => {
                let local_name = e.local_name();
                let name = std::str::from_utf8(local_name.as_ref())?;
                if name == "param" && depth == 1 {
                    let attrs = parse_attrs(e)?;
                    let param_name = attrs.get("name").cloned().unwrap_or_default();
                    let param_name_lower = param_name.to_ascii_lowercase();
                    if param_name_lower == "amount" || param_name_lower == "pan" {
                        let kfs = parse_keyframe_animation_children(reader)?;
                        for (time_ns, val_str, interp, bezier_controls) in &kfs {
                            if let Some(pan) = parse_fcpxml_pan_amount(val_str) {
                                result.pan_keyframes.push(NumericKeyframe {
                                    time_ns: *time_ns,
                                    value: pan,
                                    interpolation: *interp,
                                    bezier_controls: *bezier_controls,
                                });
                            }
                        }
                    } else {
                        let fragment = collect_unknown_start_fragment_from_attrs(reader, e)?;
                        result.unknown_fragments.push(fragment);
                    }
                } else {
                    depth += 1;
                    let fragment = collect_remaining_start_fragment(reader, e, depth)?;
                    depth = 1;
                    result.unknown_fragments.push(fragment);
                }
            }
            Event::Empty(ref e) => {
                if depth == 1 {
                    let fragment = collect_unknown_empty_fragment(e)?;
                    result.unknown_fragments.push(fragment);
                }
            }
            Event::End(ref _e) => {
                depth = depth.saturating_sub(1);
            }
            Event::Eof => bail!("Unexpected EOF inside <adjust-panner>"),
            _ => {}
        }
        buf.clear();
    }
    Ok(result)
}

/// Parse the children of a `<param>` element, looking for `<keyframeAnimation>/<keyframe>`.
/// Returns a vec of (time_ns, value_string, interpolation) tuples.
/// Consumes events until the matching `</param>` End event.
fn native_curve_to_bezier_controls(
    interpolation: KeyframeInterpolation,
    curve: Option<&str>,
) -> Option<BezierControls> {
    let curve = curve?;
    if !curve.eq_ignore_ascii_case("smooth") {
        return None;
    }
    let interpolation = if interpolation == KeyframeInterpolation::Linear {
        KeyframeInterpolation::EaseInOut
    } else {
        interpolation
    };
    let (x1, y1, x2, y2) = interpolation.control_points();
    Some(BezierControls { x1, y1, x2, y2 })
}

fn parse_keyframe_animation_children(
    reader: &mut Reader<&[u8]>,
) -> Result<Vec<(u64, String, KeyframeInterpolation, Option<BezierControls>)>> {
    let mut keyframes: Vec<(u64, String, KeyframeInterpolation, Option<BezierControls>)> =
        Vec::new();
    let mut buf = Vec::new();
    let mut depth = 1usize; // already inside <param>
    let mut in_keyframe_animation = false;

    while depth > 0 {
        match reader.read_event_into(&mut buf)? {
            Event::Start(ref e) => {
                depth += 1;
                let local_name = e.local_name();
                let name = std::str::from_utf8(local_name.as_ref())?;
                if name == "keyframeAnimation" {
                    in_keyframe_animation = true;
                }
            }
            Event::Empty(ref e) => {
                let local_name = e.local_name();
                let name = std::str::from_utf8(local_name.as_ref())?;
                if name == "keyframe" && in_keyframe_animation {
                    let attrs = parse_attrs(e)?;
                    if let (Some(time_str), Some(value_str)) =
                        (attrs.get("time"), attrs.get("value"))
                    {
                        if let Some(time_ns) = parse_fcpxml_time(time_str) {
                            let interp = attrs
                                .get("interp")
                                .map(|s| KeyframeInterpolation::from_fcpxml(s))
                                .unwrap_or(KeyframeInterpolation::Linear);
                            let bezier_controls = native_curve_to_bezier_controls(
                                interp,
                                attrs.get("curve").map(|s| s.as_str()),
                            );
                            keyframes.push((time_ns, value_str.clone(), interp, bezier_controls));
                        }
                    }
                }
            }
            Event::End(ref e) => {
                let local_name = e.local_name();
                let name = std::str::from_utf8(local_name.as_ref())?;
                if name == "keyframeAnimation" {
                    in_keyframe_animation = false;
                }
                depth = depth.saturating_sub(1);
            }
            Event::Eof => bail!("Unexpected EOF inside <param>"),
            _ => {}
        }
        buf.clear();
    }
    Ok(keyframes)
}

/// Apply native FCPXML keyframes to a clip, only for properties that lack `us:*-keyframes` vendor attrs.
fn apply_native_transform_keyframes(
    params: &NativeKeyframeParams,
    ctx: &ActiveClipContext,
    track_map: &mut BTreeMap<(u8, usize), Track>,
    project_width: u32,
    project_height: u32,
) {
    let clip = match current_clip_mut(track_map, Some(ctx)) {
        Some(c) => c,
        None => return,
    };

    // Position keyframes: only if no us:position-x-keyframes / us:position-y-keyframes
    if !ctx.has_us_position_keyframes && !params.position_keyframes.is_empty() {
        let mut x_kfs = Vec::new();
        let mut y_kfs = Vec::new();
        for &(time_ns, fcpxml_x, fcpxml_y, interp, bezier_controls) in &params.position_keyframes {
            // For position conversion, we need the scale at this keyframe's time.
            // Use scale keyframes if present, otherwise static clip scale.
            // Note: params.scale_keyframes and time_ns are both in absolute source
            // time, so the evaluation is correct before we convert to clip-local.
            let scale_at_time = if !params.scale_keyframes.is_empty() {
                Clip::evaluate_keyframed_value(&params.scale_keyframes, time_ns, clip.scale)
            } else {
                clip.scale
            };
            let (ix, iy) = fcpxml_position_to_internal(
                fcpxml_x,
                fcpxml_y,
                project_width,
                project_height,
                scale_at_time,
            );
            // Convert absolute source time to clip-local time
            x_kfs.push(NumericKeyframe {
                time_ns: time_ns.saturating_sub(ctx.raw_source_start_ns),
                value: ix,
                interpolation: interp,
                bezier_controls,
            });
            y_kfs.push(NumericKeyframe {
                time_ns: time_ns.saturating_sub(ctx.raw_source_start_ns),
                value: iy,
                interpolation: interp,
                bezier_controls,
            });
        }
        clip.position_x_keyframes = x_kfs;
        clip.position_y_keyframes = y_kfs;
    }

    // Scale keyframes: only if no us:scale-keyframes
    // Convert absolute source time to clip-local time
    if !ctx.has_us_scale_keyframes && !params.scale_keyframes.is_empty() {
        clip.scale_keyframes = params
            .scale_keyframes
            .iter()
            .map(|kf| NumericKeyframe {
                time_ns: kf.time_ns.saturating_sub(ctx.raw_source_start_ns),
                ..*kf
            })
            .collect();
    }
    // Rotation keyframes: only if no us:rotate-keyframes
    // Convert absolute source time to clip-local time
    if !ctx.has_us_rotate_keyframes && !params.rotation_keyframes.is_empty() {
        clip.rotate_keyframes = params
            .rotation_keyframes
            .iter()
            .map(|kf| NumericKeyframe {
                time_ns: kf.time_ns.saturating_sub(ctx.raw_source_start_ns),
                ..*kf
            })
            .collect();
    }
}

/// Apply native opacity keyframes from adjust-blend/adjust-compositing.
/// FCP keyframe times are in absolute source time; convert to clip-local.
fn apply_native_opacity_keyframes(
    params: &NativeKeyframeParams,
    ctx: &ActiveClipContext,
    track_map: &mut BTreeMap<(u8, usize), Track>,
) {
    if ctx.has_us_opacity_keyframes || params.opacity_keyframes.is_empty() {
        return;
    }
    if let Some(clip) = current_clip_mut(track_map, Some(ctx)) {
        clip.opacity_keyframes = params
            .opacity_keyframes
            .iter()
            .map(|kf| NumericKeyframe {
                time_ns: kf.time_ns.saturating_sub(ctx.raw_source_start_ns),
                ..*kf
            })
            .collect();
    }
}

/// Apply native volume keyframes from adjust-volume.
/// FCP keyframe times are in absolute source time; convert to clip-local.
fn apply_native_volume_keyframes(
    params: &NativeKeyframeParams,
    ctx: &ActiveClipContext,
    track_map: &mut BTreeMap<(u8, usize), Track>,
) {
    if ctx.has_us_volume_keyframes || params.volume_keyframes.is_empty() {
        return;
    }
    if let Some(clip) = current_clip_mut(track_map, Some(ctx)) {
        clip.volume_keyframes = params
            .volume_keyframes
            .iter()
            .map(|kf| NumericKeyframe {
                time_ns: kf.time_ns.saturating_sub(ctx.raw_source_start_ns),
                ..*kf
            })
            .collect();
    }
}

fn apply_native_pan_keyframes(
    params: &NativeKeyframeParams,
    ctx: &ActiveClipContext,
    track_map: &mut BTreeMap<(u8, usize), Track>,
) {
    if ctx.has_us_pan_keyframes || params.pan_keyframes.is_empty() {
        return;
    }
    if let Some(clip) = current_clip_mut(track_map, Some(ctx)) {
        clip.pan_keyframes = params
            .pan_keyframes
            .iter()
            .map(|kf| NumericKeyframe {
                time_ns: kf.time_ns.saturating_sub(ctx.raw_source_start_ns),
                ..*kf
            })
            .collect();
    }
}

/// Like `collect_unknown_start_fragment` but we already parsed the start event's attributes.
/// Re-serializes the start tag and then collects everything until matching end.
fn collect_unknown_start_fragment_from_attrs(
    reader: &mut Reader<&[u8]>,
    start: &quick_xml::events::BytesStart,
) -> Result<String> {
    collect_unknown_start_fragment(reader, start)
}

/// Collect an unknown fragment starting from a nested Start event.
/// `depth` is the current nesting level. Consumes until depth returns to the
/// level before this element started (depth - 1).
fn collect_remaining_start_fragment(
    reader: &mut Reader<&[u8]>,
    start: &quick_xml::events::BytesStart,
    start_depth: usize,
) -> Result<String> {
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    writer.write_event(Event::Start(start.to_owned()))?;

    let mut depth = start_depth;
    let target_depth = start_depth - 1;
    let mut buf = Vec::new();
    while depth > target_depth {
        match reader.read_event_into(&mut buf)? {
            Event::Start(ref e) => {
                depth += 1;
                writer.write_event(Event::Start(e.to_owned()))?;
            }
            Event::End(ref e) => {
                depth = depth.saturating_sub(1);
                writer.write_event(Event::End(e.to_owned()))?;
            }
            Event::Empty(ref e) => {
                writer.write_event(Event::Empty(e.to_owned()))?;
            }
            Event::Text(ref e) => {
                writer.write_event(Event::Text(e.to_owned()))?;
            }
            Event::CData(ref e) => {
                writer.write_event(Event::CData(e.to_owned()))?;
            }
            Event::Comment(ref e) => {
                writer.write_event(Event::Comment(e.to_owned()))?;
            }
            Event::Eof => bail!("Unexpected EOF while capturing unknown fragment"),
            _ => {}
        }
        buf.clear();
    }
    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

fn parse_vec2(value: &str) -> Option<(f64, f64)> {
    let mut parts = value.split_whitespace();
    let x = parts.next()?.parse().ok()?;
    let y = parts.next()?.parse().ok()?;
    Some((x, y))
}

fn is_known_asset_clip_attr(key: &str) -> bool {
    matches!(
        key,
        "ref"
            | "offset"
            | "start"
            | "duration"
            | "name"
            | "lane"
            | "us:track-idx"
            | "us:track-kind"
            | "us:track-name"
            | "us:track-muted"
            | "us:track-locked"
            | "us:track-soloed"
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
            | "us:vidstab-enabled"
            | "us:vidstab-smoothing"
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
            | "us:exposure"
            | "us:black-point"
            | "us:highlights-warmth"
            | "us:highlights-tint"
            | "us:midtones-warmth"
            | "us:midtones-tint"
            | "us:shadows-warmth"
            | "us:shadows-tint"
            | "us:chroma-key-enabled"
            | "us:chroma-key-color"
            | "us:chroma-key-tolerance"
            | "us:chroma-key-softness"
            | "us:bg-removal-enabled"
            | "us:bg-removal-threshold"
            | "us:lut-path"
            | "us:transition-after"
            | "us:transition-after-ns"
            | "us:blend-mode"
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
    )
}

fn is_known_asset_attr(key: &str) -> bool {
    matches!(
        key,
        "id" | "src" | "name" | "duration" | "start" | "hasVideo" | "hasAudio"
    )
}

fn collect_unknown_attrs(
    attrs: &HashMap<String, String>,
    is_known: fn(&str) -> bool,
) -> Vec<(String, String)> {
    attrs
        .iter()
        .filter(|(k, _)| !is_known(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn is_known_fcpxml_attr(key: &str) -> bool {
    matches!(key, "version" | "xmlns:us")
}

fn is_known_resources_attr(_key: &str) -> bool {
    false
}

fn is_known_library_attr(_key: &str) -> bool {
    false
}

fn is_known_event_attr(_key: &str) -> bool {
    false
}

fn is_known_project_attr(key: &str) -> bool {
    matches!(key, "name")
}

fn is_known_sequence_attr(key: &str) -> bool {
    matches!(key, "duration" | "format")
}

fn is_known_spine_attr(_key: &str) -> bool {
    false
}

fn is_known_format_attr(key: &str) -> bool {
    matches!(key, "id" | "name" | "frameDuration" | "width" | "height")
}

fn parse_crop_value(value: &str) -> Option<i32> {
    value.parse::<f64>().ok().map(|v| v.round() as i32)
}

fn fcpxml_mount_root(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut components = path.components();
    if !matches!(components.next(), Some(Component::RootDir)) {
        return None;
    }
    let first = components.find_map(|c| match c {
        Component::Normal(p) => Some(p.to_os_string()),
        _ => None,
    })?;
    let mut root = PathBuf::from("/");
    root.push(first);
    Some(root)
}

fn parse_fcpxml_src_path(src: &str) -> String {
    let raw_path = src.strip_prefix("file://").unwrap_or(src);
    decode_percent_encoded_path(raw_path)
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

fn resolve_import_source_path(
    original: &str,
    mount_root: Option<&Path>,
    mount_users: &[String],
) -> String {
    for candidate in remap_candidates_for_volumes_path(original, mount_root, mount_users) {
        if candidate.exists() {
            return candidate.to_string_lossy().to_string();
        }
    }
    original.to_string()
}

fn remap_candidates_for_volumes_path(
    original: &str,
    mount_root: Option<&Path>,
    mount_users: &[String],
) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let original_path = PathBuf::from(original);
    push_unique_path(&mut candidates, original_path.clone());
    let Some(suffix) = original.strip_prefix("/Volumes/") else {
        return candidates;
    };

    if let Some(root) = mount_root {
        push_unique_path(&mut candidates, root.join(suffix));
    }

    for user in mount_users {
        push_unique_path(
            &mut candidates,
            PathBuf::from("/media").join(user).join(suffix),
        );
        push_unique_path(
            &mut candidates,
            PathBuf::from("/run/media").join(user).join(suffix),
        );
    }

    push_unique_path(&mut candidates, PathBuf::from("/media").join(suffix));
    push_unique_path(&mut candidates, PathBuf::from("/run/media").join(suffix));
    push_unique_path(&mut candidates, PathBuf::from("/mnt").join(suffix));

    candidates
}

fn push_unique_path(paths: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !paths.iter().any(|p| p == &candidate) {
        paths.push(candidate);
    }
}

fn linux_mount_users_for_fcpxml(path: &Path) -> Vec<String> {
    let mut users = Vec::new();
    if let Ok(user) = std::env::var("USER") {
        if !user.is_empty() {
            users.push(user);
        }
    }
    if let Some(from_path) = user_from_mount_path(path) {
        if !users.iter().any(|u| u == &from_path) {
            users.push(from_path);
        }
    }
    users
}

fn user_from_mount_path(path: &Path) -> Option<String> {
    let mut comps = path.components();
    if !matches!(comps.next(), Some(Component::RootDir)) {
        return None;
    }
    let first = match comps.next() {
        Some(Component::Normal(c)) => c.to_string_lossy().to_string(),
        _ => return None,
    };
    let second = match comps.next() {
        Some(Component::Normal(c)) => c.to_string_lossy().to_string(),
        _ => return None,
    };
    if first == "media" {
        return Some(second);
    }
    if first == "run" {
        let third = match comps.next() {
            Some(Component::Normal(c)) => c.to_string_lossy().to_string(),
            _ => return None,
        };
        if second == "media" {
            return Some(third);
        }
    }
    None
}

fn collect_unknown_empty_fragment(e: &quick_xml::events::BytesStart) -> Result<String> {
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    writer.write_event(Event::Empty(e.to_owned()))?;
    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

fn collect_unknown_start_fragment(
    reader: &mut Reader<&[u8]>,
    start: &quick_xml::events::BytesStart,
) -> Result<String> {
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    writer.write_event(Event::Start(start.to_owned()))?;

    let mut depth = 1usize;
    let mut buf = Vec::new();
    while depth > 0 {
        match reader.read_event_into(&mut buf)? {
            Event::Start(ref e) => {
                depth += 1;
                writer.write_event(Event::Start(e.to_owned()))?;
            }
            Event::End(ref e) => {
                depth = depth.saturating_sub(1);
                writer.write_event(Event::End(e.to_owned()))?;
            }
            Event::Empty(ref e) => {
                writer.write_event(Event::Empty(e.to_owned()))?;
            }
            Event::Text(ref e) => {
                writer.write_event(Event::Text(e.to_owned()))?;
            }
            Event::CData(ref e) => {
                writer.write_event(Event::CData(e.to_owned()))?;
            }
            Event::Comment(ref e) => {
                writer.write_event(Event::Comment(e.to_owned()))?;
            }
            Event::Eof => bail!("Unexpected EOF while capturing unknown FCPXML tag"),
            _ => {}
        }
        buf.clear();
    }

    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

fn build_asset_builder(attrs: &HashMap<String, String>) -> Option<AssetBuilder> {
    let id = attrs.get("id")?.clone();
    let duration_ns = attrs
        .get("duration")
        .and_then(|d| parse_fcpxml_time(d))
        .unwrap_or(0);
    let start_ns = attrs
        .get("start")
        .and_then(|d| parse_fcpxml_time(d))
        .unwrap_or(0);
    Some(AssetBuilder {
        id,
        src: None,
        name: attrs.get("name").cloned().unwrap_or_default(),
        duration_ns,
        start_ns,
        has_video: parse_flag(attrs.get("hasVideo"), true),
        has_audio: parse_flag(attrs.get("hasAudio"), true),
        unknown_attrs: attrs
            .iter()
            .filter(|(k, _)| !is_known_asset_attr(k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        unknown_children: Vec::new(),
    })
}

fn finalize_asset(asset: AssetBuilder, assets: &mut HashMap<String, Asset>) {
    if let Some(src) = asset.src {
        assets.insert(
            asset.id.clone(),
            Asset {
                id: asset.id,
                src,
                name: asset.name,
                duration_ns: asset.duration_ns,
                start_ns: asset.start_ns,
                has_video: asset.has_video,
                has_audio: asset.has_audio,
                unknown_attrs: asset.unknown_attrs,
                unknown_children: asset.unknown_children,
            },
        );
    }
}

fn parse_format_spec(attrs: &HashMap<String, String>) -> Option<(String, FormatSpec)> {
    let id = attrs.get("id")?.clone();
    let preset = attrs
        .get("name")
        .and_then(|name| parse_format_name_preset(name));
    let width = attrs
        .get("width")
        .and_then(|s| s.parse().ok())
        .or_else(|| preset.as_ref().map(|p| p.width))
        .unwrap_or(1920);
    let height = attrs
        .get("height")
        .and_then(|s| s.parse().ok())
        .or_else(|| preset.as_ref().map(|p| p.height))
        .unwrap_or(1080);
    let frame_rate = attrs
        .get("frameDuration")
        .map(|fd| parse_frame_duration(fd))
        .or_else(|| preset.as_ref().map(|p| p.frame_rate.clone()))
        .unwrap_or_else(FrameRate::fps_24);
    let unknown_attrs = attrs
        .iter()
        .filter(|(k, _)| !is_known_format_attr(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Some((
        id,
        FormatSpec {
            width,
            height,
            frame_rate,
            unknown_attrs,
        },
    ))
}

fn parse_format_name_preset(name: &str) -> Option<FormatSpec> {
    let n = name.to_ascii_lowercase();
    if n.contains("1080p30") {
        return Some(FormatSpec {
            width: 1920,
            height: 1080,
            frame_rate: FrameRate {
                numerator: 30,
                denominator: 1,
            },
            unknown_attrs: Vec::new(),
        });
    }
    if n.contains("1080p24") {
        return Some(FormatSpec {
            width: 1920,
            height: 1080,
            frame_rate: FrameRate::fps_24(),
            unknown_attrs: Vec::new(),
        });
    }
    if n.contains("2160p24") {
        return Some(FormatSpec {
            width: 3840,
            height: 2160,
            frame_rate: FrameRate::fps_24(),
            unknown_attrs: Vec::new(),
        });
    }
    None
}

fn parse_sequence_marker(
    attrs: &HashMap<String, String>,
    clip_ctx: Option<(u64, u64)>,
    project: &mut Project,
) {
    if let Some(start_str) = attrs.get("start") {
        if let Some(raw_pos_ns) = parse_fcpxml_time(start_str) {
            let pos_ns = if let Some((timeline_start, source_in)) = clip_ctx {
                timeline_start + raw_pos_ns.saturating_sub(source_in)
            } else {
                raw_pos_ns
            };
            let label = attrs.get("value").cloned().unwrap_or_default();
            let color = attrs
                .get("us:color")
                .and_then(|s| u32::from_str_radix(s, 16).ok())
                .unwrap_or(0xFF8C00FF);
            use crate::model::project::Marker;
            let mut m = Marker::new(pos_ns, label);
            m.color = color;
            project.markers.push(m);
        }
    }
}

fn parse_flag(value: Option<&String>, default: bool) -> bool {
    match value.map(|v| v.as_str()) {
        Some("1") | Some("true") | Some("TRUE") => true,
        Some("0") | Some("false") | Some("FALSE") => false,
        _ => default,
    }
}

fn parse_attrs(e: &quick_xml::events::BytesStart) -> Result<HashMap<String, String>> {
    let attrs = e.attributes();
    let mut map = HashMap::with_capacity(attrs.size_hint().0);
    for attr in attrs {
        let attr = attr?;
        let key = std::str::from_utf8(attr.key.as_ref())?.to_string();
        let raw_value = std::str::from_utf8(attr.value.as_ref())?;
        let value = unescape(raw_value)?.into_owned();
        map.insert(key, value);
    }
    Ok(map)
}

fn sanitize_unescaped_keyframe_attr_json(xml: &str) -> Cow<'_, str> {
    const KEYFRAME_ATTR_PREFIXES: [&str; 17] = [
        "us:brightness-keyframes=\"",
        "us:contrast-keyframes=\"",
        "us:saturation-keyframes=\"",
        "us:temperature-keyframes=\"",
        "us:tint-keyframes=\"",
        "us:scale-keyframes=\"",
        "us:opacity-keyframes=\"",
        "us:position-x-keyframes=\"",
        "us:position-y-keyframes=\"",
        "us:volume-keyframes=\"",
        "us:pan-keyframes=\"",
        "us:rotate-keyframes=\"",
        "us:crop-left-keyframes=\"",
        "us:crop-right-keyframes=\"",
        "us:crop-top-keyframes=\"",
        "us:crop-bottom-keyframes=\"",
        "us:frei0r-effects=\"",
    ];

    let mut cursor = 0usize;
    let mut changed = false;
    let mut out = String::new();

    while cursor < xml.len() {
        let remainder = &xml[cursor..];
        let next = KEYFRAME_ATTR_PREFIXES
            .iter()
            .filter_map(|prefix| remainder.find(prefix).map(|idx| (cursor + idx, *prefix)))
            .min_by_key(|(idx, _)| *idx);

        let Some((attr_start, attr_prefix)) = next else {
            break;
        };

        let value_start = attr_start + attr_prefix.len();
        out.push_str(&xml[cursor..value_start]);

        let Some(rel_end) = xml[value_start..].find("]\"") else {
            cursor = value_start;
            break;
        };

        let value_end = value_start + rel_end;
        let value = &xml[value_start..=value_end];

        if value.contains('\"') {
            changed = true;
            for ch in value.chars() {
                if ch == '\"' {
                    out.push_str("&quot;");
                } else {
                    out.push(ch);
                }
            }
        } else {
            out.push_str(value);
        }

        cursor = value_end + 1;
    }

    if !changed {
        return Cow::Borrowed(xml);
    }

    out.push_str(&xml[cursor..]);
    Cow::Owned(out)
}

/// Parse an FCPXML time string like "48/24s" or "48048/24000s" into nanoseconds
fn parse_fcpxml_time(s: &str) -> Option<u64> {
    let s = s.trim_end_matches('s');
    if let Some((num, den)) = s.split_once('/') {
        let num: u128 = num.parse().ok()?;
        let den: u128 = den.parse().ok()?;
        if den == 0 {
            return None;
        }
        // Use u128 to avoid overflow with large FCP time values
        let ns = num * 1_000_000_000 / den;
        Some(ns as u64)
    } else {
        // Plain seconds
        let secs: f64 = s.parse().ok()?;
        Some((secs * 1_000_000_000.0) as u64)
    }
}

/// Parse frameDuration like "1001/30000" into a FrameRate
fn parse_frame_duration(s: &str) -> FrameRate {
    let s = s.trim_end_matches('s');
    if let Some((num, den)) = s.split_once('/') {
        // frameDuration = 1/fps → fps = den/num
        let num: u32 = num.parse().unwrap_or(1);
        let den: u32 = den.parse().unwrap_or(24);
        FrameRate {
            numerator: den,
            denominator: num,
        }
    } else {
        FrameRate::fps_24()
    }
}

fn validate_fcpxml_version(version: &str) -> Result<()> {
    let parsed = parse_fcpxml_version(version)
        .ok_or_else(|| anyhow!("Unsupported FCPXML version format: {version}"))?;
    if parsed < crate::fcpxml::FCPXML_MIN_VERSION || parsed > crate::fcpxml::FCPXML_MAX_VERSION {
        bail!(
            "Unsupported FCPXML version {version}; supported range is {}.{} through {}.{}",
            crate::fcpxml::FCPXML_MIN_VERSION.0,
            crate::fcpxml::FCPXML_MIN_VERSION.1,
            crate::fcpxml::FCPXML_MAX_VERSION.0,
            crate::fcpxml::FCPXML_MAX_VERSION.1
        );
    }
    Ok(())
}

fn parse_fcpxml_version(version: &str) -> Option<(u32, u32)> {
    let (major, minor) = version.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_fcpxml_time ──────────────────────────────────────────────────────

    #[test]
    fn test_parse_time_fraction() {
        // 48 frames at 24 fps = 2 seconds = 2_000_000_000 ns
        assert_eq!(parse_fcpxml_time("48/24s"), Some(2_000_000_000));
    }

    #[test]
    fn test_parse_time_fraction_no_s_suffix() {
        assert_eq!(parse_fcpxml_time("24/24"), Some(1_000_000_000));
    }

    #[test]
    fn test_parse_time_ntsc() {
        // 48048 / 24000 ≈ 2.002 seconds; integer math: 48048 * 1e9 / 24000 = 2_002_000_000
        assert_eq!(parse_fcpxml_time("48048/24000s"), Some(2_002_000_000));
    }

    #[test]
    fn test_parse_time_large_fcp_keyframe_values() {
        // FCP uses large numerators like 52335350310/720000s — must not overflow
        let ns = parse_fcpxml_time("52335350310/720000s");
        assert!(ns.is_some(), "large FCP time value should not overflow");
        assert_eq!(ns.unwrap(), 72_687_986_541_666);

        let ns2 = parse_fcpxml_time("1416023070/35280000s");
        assert!(ns2.is_some());
        assert_eq!(ns2.unwrap(), 40_136_708_333);
    }

    #[test]
    fn test_parse_time_zero() {
        assert_eq!(parse_fcpxml_time("0/24s"), Some(0));
    }

    #[test]
    fn test_parse_time_zero_denominator_returns_none() {
        assert_eq!(parse_fcpxml_time("1/0s"), None);
    }

    #[test]
    fn test_parse_time_plain_seconds() {
        // "2" → 2_000_000_000 ns
        let result = parse_fcpxml_time("2");
        assert_eq!(result, Some(2_000_000_000));
    }

    #[test]
    fn test_parse_time_invalid_returns_none() {
        assert_eq!(parse_fcpxml_time("abcs"), None);
    }

    // ── parse_frame_duration ──────────────────────────────────────────────────

    #[test]
    fn test_parse_frame_duration_24fps() {
        let fps = parse_frame_duration("1/24s");
        assert_eq!(fps.numerator, 24);
        assert_eq!(fps.denominator, 1);
    }

    #[test]
    fn test_parse_frame_duration_ntsc() {
        // frameDuration "1001/30000s" → fps = 30000/1001 ≈ 29.97
        let fps = parse_frame_duration("1001/30000s");
        assert_eq!(fps.numerator, 30000);
        assert_eq!(fps.denominator, 1001);
    }

    #[test]
    fn test_parse_frame_duration_fallback() {
        let fps = parse_frame_duration("bad");
        assert_eq!(fps.numerator, 24);
        assert_eq!(fps.denominator, 1);
    }

    // ── parse_fcpxml (full document) ─────────────────────────────────────────

    #[test]
    fn test_parse_fcpxml_empty_spine_gets_default_tracks() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.10" xmlns:us="urn:ultimateslice">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
  </resources>
  <library>
    <event>
      <project name="EmptyProject">
        <sequence duration="0/24s" format="r1">
          <spine/>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        assert_eq!(project.title, "EmptyProject");
        assert_eq!(project.width, 1920);
        assert_eq!(project.height, 1080);
        // Empty spine gets default tracks
        assert!(!project.tracks.is_empty());
    }

    #[test]
    fn test_parse_fcpxml_single_clip() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.10" xmlns:us="urn:ultimateslice">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///footage.mp4" name="footage" duration="240/24s"/>
  </resources>
  <library>
    <event>
      <project name="TestProject">
        <sequence duration="240/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0/24s" duration="240/24s" start="0/24s"
                        name="footage" us:track-idx="0" us:track-kind="video" us:track-name="Video 1"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        assert_eq!(project.title, "TestProject");
        // Should have at least one track with one clip
        let video_tracks: Vec<_> = project.video_tracks().collect();
        assert_eq!(video_tracks.len(), 1);
        assert_eq!(video_tracks[0].clips.len(), 1);
        let clip = &video_tracks[0].clips[0];
        assert_eq!(clip.source_out, 10_000_000_000); // 240/24s = 10s
        assert_eq!(clip.timeline_start, 0);
    }

    #[test]
    fn test_parse_fcpxml_frame_rate_set() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.10" xmlns:us="urn:ultimateslice">
  <resources>
    <format id="r1" frameDuration="1001/30000s" width="1280" height="720"/>
  </resources>
  <library>
    <event>
      <project name="NTSC">
        <sequence duration="0/30000s" format="r1">
          <spine/>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        assert_eq!(project.width, 1280);
        assert_eq!(project.height, 720);
        // NTSC frame rate parsed correctly
        assert_eq!(project.frame_rate.numerator, 30000);
        assert_eq!(project.frame_rate.denominator, 1001);
    }

    #[test]
    fn test_parse_fcpxml_marker() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.10" xmlns:us="urn:ultimateslice">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
  </resources>
  <library>
    <event>
      <project name="Marked">
        <sequence duration="0/24s" format="r1">
          <spine/>
          <marker start="24/24s" duration="1/24s" value="Chapter 1" us:color="FF0000FF"/>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        assert_eq!(project.markers.len(), 1);
        assert_eq!(project.markers[0].label, "Chapter 1");
        assert_eq!(project.markers[0].position_ns, 1_000_000_000); // 24/24s = 1s
        assert_eq!(project.markers[0].color, 0xFF0000FF);
    }

    #[test]
    fn test_parse_fcpxml_clip_color_attributes() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.10" xmlns:us="urn:ultimateslice">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///footage.mp4" name="footage" duration="240/24s"/>
  </resources>
  <library>
    <event>
      <project name="ColorTest">
        <sequence duration="240/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0/24s" duration="240/24s" start="0/24s"
                        name="footage" us:track-idx="0" us:track-kind="video" us:track-name="Video 1"
                        us:brightness="0.5" us:contrast="1.2" us:saturation="0.8"
                        us:temperature="7200" us:tint="0.2"
                        us:opacity="0.9" us:speed="2.0"
                        us:brightness-keyframes='[{"time_ns":0,"value":0.1,"interpolation":"linear"},{"time_ns":1000000000,"value":0.6,"interpolation":"linear"}]'
                        us:contrast-keyframes='[{"time_ns":0,"value":1.0,"interpolation":"linear"},{"time_ns":1000000000,"value":1.8,"interpolation":"linear"}]'
                        us:saturation-keyframes='[{"time_ns":0,"value":0.8,"interpolation":"linear"},{"time_ns":1000000000,"value":1.4,"interpolation":"linear"}]'
                        us:temperature-keyframes='[{"time_ns":0,"value":3000.0,"interpolation":"linear"},{"time_ns":1000000000,"value":8200.0,"interpolation":"linear"}]'
                        us:tint-keyframes='[{"time_ns":0,"value":-0.3,"interpolation":"linear"},{"time_ns":1000000000,"value":0.4,"interpolation":"linear"}]'
                        us:scale-keyframes='[{"time_ns":0,"value":1.0,"interpolation":"linear"},{"time_ns":1000000000,"value":1.5,"interpolation":"linear"}]'
                        us:opacity-keyframes='[{"time_ns":0,"value":1.0,"interpolation":"linear"},{"time_ns":500000000,"value":0.4,"interpolation":"linear"}]'
                        us:position-x-keyframes='[{"time_ns":0,"value":-0.5,"interpolation":"linear"},{"time_ns":1000000000,"value":0.5,"interpolation":"linear"}]'
                        us:position-y-keyframes='[{"time_ns":0,"value":0.2,"interpolation":"linear"},{"time_ns":1000000000,"value":-0.2,"interpolation":"linear"}]'
                        us:volume-keyframes='[{"time_ns":0,"value":1.0,"interpolation":"linear"},{"time_ns":1000000000,"value":0.6,"interpolation":"linear"}]'
                        us:freeze-frame="true" us:freeze-source-ns="1200000000"
                        us:freeze-hold-duration-ns="3000000000"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        assert!((clip.brightness - 0.5).abs() < 1e-5);
        assert!((clip.contrast - 1.2).abs() < 1e-5);
        assert!((clip.saturation - 0.8).abs() < 1e-5);
        assert!((clip.temperature - 7200.0).abs() < 1e-5);
        assert!((clip.tint - 0.2).abs() < 1e-5);
        assert!((clip.opacity - 0.9).abs() < 1e-5);
        assert!((clip.speed - 2.0).abs() < 1e-5);
        assert_eq!(clip.brightness_keyframes.len(), 2);
        assert_eq!(clip.contrast_keyframes.len(), 2);
        assert_eq!(clip.saturation_keyframes.len(), 2);
        assert_eq!(clip.temperature_keyframes.len(), 2);
        assert_eq!(clip.tint_keyframes.len(), 2);
        assert_eq!(clip.scale_keyframes.len(), 2);
        assert_eq!(clip.opacity_keyframes.len(), 2);
        assert_eq!(clip.position_x_keyframes.len(), 2);
        assert_eq!(clip.position_y_keyframes.len(), 2);
        assert_eq!(clip.volume_keyframes.len(), 2);
        assert!(clip.freeze_frame);
        assert_eq!(clip.freeze_frame_source_ns, Some(1_200_000_000));
        assert_eq!(clip.freeze_frame_hold_duration_ns, Some(3_000_000_000));
    }

    #[test]
    fn test_parse_fcpxml_escaped_keyframe_json_attrs() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14" xmlns:us="urn:ultimateslice">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///footage.mp4" name="footage" duration="240/24s"/>
  </resources>
  <library>
    <event>
      <project name="EscapedKeyframes">
        <sequence duration="240/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0/24s" duration="240/24s" start="0/24s"
                        name="footage" us:track-idx="0" us:track-kind="video"
                        us:track-name="Video 1" us:position-x-keyframes="[{&quot;time_ns&quot;:617642015,&quot;value&quot;:-0.8200659196027513,&quot;interpolation&quot;:&quot;linear&quot;},{&quot;time_ns&quot;:2129974732,&quot;value&quot;:-0.8200659196027513,&quot;interpolation&quot;:&quot;linear&quot;},{&quot;time_ns&quot;:4709284968,&quot;value&quot;:0.67,&quot;interpolation&quot;:&quot;linear&quot;}]"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        assert_eq!(clip.position_x_keyframes.len(), 3);
        assert!((clip.position_x_keyframes[0].value - (-0.8200659196027513)).abs() < 1e-9);
        assert!((clip.position_x_keyframes[2].value - 0.67).abs() < 1e-9);
    }

    #[test]
    fn test_parse_fcpxml_recovers_unescaped_keyframe_json_attrs() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14" xmlns:us="urn:ultimateslice">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///footage.mp4" name="footage" duration="240/24s"/>
  </resources>
  <library>
    <event>
      <project name="UnescapedKeyframes">
        <sequence duration="240/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0/24s" duration="240/24s" start="0/24s"
                        name="footage" us:track-idx="0" us:track-kind="video"
                        us:track-name="Video 1" us:position-x-keyframes="[{"time_ns":617642015,"value":-0.8200659196027513,"interpolation":"linear"},{"time_ns":2129974732,"value":-0.8200659196027513,"interpolation":"linear"},{"time_ns":4709284968,"value":0.67,"interpolation":"linear"}]"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        assert_eq!(clip.position_x_keyframes.len(), 3);
        assert_eq!(clip.position_x_keyframes[0].time_ns, 617_642_015);
        assert_eq!(clip.position_x_keyframes[2].time_ns, 4_709_284_968);
    }

    #[test]
    fn test_parse_fcpxml_link_group_attr() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.10" xmlns:us="urn:ultimateslice">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///footage.mp4" name="footage" duration="240/24s"/>
  </resources>
  <library>
    <event>
      <project name="LinkTest">
        <sequence duration="240/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0/24s" duration="240/24s" start="0/24s"
                        name="footage" us:track-idx="0" us:track-kind="video" us:track-name="Video 1"
                        us:link-group-id="link-1"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        assert_eq!(clip.link_group_id.as_deref(), Some("link-1"));
    }

    #[test]
    fn test_parse_fcpxml_source_timecode_base_attr() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.10" xmlns:us="urn:ultimateslice">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///footage.mp4" name="footage" duration="240/24s"/>
  </resources>
  <library>
    <event>
      <project name="TimecodeTest">
        <sequence duration="240/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0/24s" duration="240/24s" start="120/24s"
                        name="footage" us:track-idx="0" us:track-kind="video" us:track-name="Video 1"
                        us:source-timecode-base-ns="4000000000"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        assert_eq!(clip.source_timecode_base_ns, Some(4_000_000_000));
        assert_eq!(clip.source_in, 1_000_000_000);
        assert_eq!(clip.source_timecode_start_ns(), Some(5_000_000_000));
    }

    #[test]
    fn test_parse_fcpxml_version_1_14_supported() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14" xmlns:us="urn:ultimateslice">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
  </resources>
  <library>
    <event>
      <project name="Version114">
        <sequence duration="0/24s" format="r1">
          <spine/>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("1.14 should be supported");
        assert_eq!(project.title, "Version114");
    }

    #[test]
    fn test_parse_fcpxml_version_above_supported_rejected() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.15" xmlns:us="urn:ultimateslice">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
  </resources>
  <library>
    <event>
      <project name="TooNew">
        <sequence duration="0/24s" format="r1">
          <spine/>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let err = parse_fcpxml(xml).expect_err("1.15 should be rejected");
        assert!(err.to_string().contains("Unsupported FCPXML version 1.15"));
    }

    #[test]
    fn test_parse_fcpxml_asset_media_rep_src_fallback() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" name="fallback-src" duration="240/24s" hasVideo="1" hasAudio="1">
      <media-rep kind="original-media" src="file:///tmp/fallback.mov"/>
    </asset>
  </resources>
  <library>
    <event>
      <project name="MediaRepFallback">
        <sequence duration="240/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0s" start="0s" duration="240/24s" name="fallback-src"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        assert_eq!(clip.source_path, "/tmp/fallback.mov");
    }

    #[test]
    fn test_parse_fcpxml_first_project_only() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///first.mov" name="first" duration="48/24s"/>
    <asset id="a2" src="file:///second.mov" name="second" duration="48/24s"/>
  </resources>
  <library>
    <event>
      <project name="FirstProject">
        <sequence duration="48/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0s" start="0s" duration="48/24s" name="first"/>
          </spine>
        </sequence>
      </project>
      <project name="SecondProject">
        <sequence duration="48/24s" format="r1">
          <spine>
            <asset-clip ref="a2" offset="0s" start="0s" duration="48/24s" name="second"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        assert_eq!(project.title, "FirstProject");
        let clips: Vec<_> = project
            .video_tracks()
            .flat_map(|t| t.clips.iter())
            .map(|c| c.label.clone())
            .collect();
        assert_eq!(clips, vec!["first".to_string()]);
    }

    #[test]
    fn test_parse_fcpxml_native_transition_maps_to_clip_transition_after() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///a.mov" name="a" duration="240/24s"/>
    <asset id="a2" src="file:///b.mov" name="b" duration="240/24s"/>
  </resources>
  <library>
    <event>
      <project name="TransitionMap">
        <sequence duration="480/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0s" start="0s" duration="240/24s" name="a"/>
            <transition name="Cross Dissolve" duration="24/24s"/>
            <asset-clip ref="a2" offset="216/24s" start="0s" duration="240/24s" name="b"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let video = project.video_tracks().next().expect("video track");
        assert_eq!(video.clips.len(), 2);
        assert_eq!(video.clips[0].transition_after, "cross_dissolve");
        assert_eq!(video.clips[0].transition_after_ns, 1_000_000_000);
    }

    #[test]
    fn test_parse_fcpxml_native_time_map_sets_constant_speed() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///a.mov" name="a" duration="480/24s"/>
  </resources>
  <library>
    <event>
      <project name="TimeMapSpeed">
        <sequence duration="480/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0s" start="0s" duration="240/24s" name="a">
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

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().expect("video").clips[0];
        assert!((clip.speed - 2.0).abs() < 1e-6);
        assert!(!clip.reverse);
        assert!(!clip.freeze_frame);
    }

    #[test]
    fn test_parse_fcpxml_native_time_map_sets_reverse_speed() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///a.mov" name="a" duration="240/24s"/>
  </resources>
  <library>
    <event>
      <project name="TimeMapReverse">
        <sequence duration="240/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0s" start="0s" duration="120/24s" name="a">
              <timeMap>
                <timept time="0s" value="120/24s" interp="linear"/>
                <timept time="120/24s" value="0s" interp="linear"/>
              </timeMap>
            </asset-clip>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().expect("video").clips[0];
        assert!((clip.speed - 1.0).abs() < 1e-6);
        assert!(clip.reverse);
        assert!(!clip.freeze_frame);
    }

    #[test]
    fn test_parse_fcpxml_native_time_map_sets_freeze_frame() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///a.mov" name="a" duration="240/24s"/>
  </resources>
  <library>
    <event>
      <project name="TimeMapFreeze">
        <sequence duration="240/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0s" start="0s" duration="120/24s" name="a">
              <timeMap>
                <timept time="0s" value="48/24s" interp="linear"/>
                <timept time="120/24s" value="48/24s" interp="linear"/>
              </timeMap>
            </asset-clip>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().expect("video").clips[0];
        assert!(clip.freeze_frame);
        assert_eq!(clip.freeze_frame_source_ns, Some(2_000_000_000));
        assert_eq!(clip.freeze_frame_hold_duration_ns, Some(5_000_000_000));
    }

    #[test]
    fn test_parse_fcpxml_native_time_map_preserved_when_vendor_speed_exists() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14" xmlns:us="urn:ultimateslice">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///a.mov" name="a" duration="240/24s"/>
  </resources>
  <library>
    <event>
      <project name="TimeMapVendorPriority">
        <sequence duration="240/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0s" start="0s" duration="120/24s" name="a" us:speed="3.0">
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

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().expect("video").clips[0];
        assert!((clip.speed - 3.0).abs() < 1e-6);
        assert!(
            clip.fcpxml_unknown_children
                .iter()
                .any(|f| f.contains("<timeMap")),
            "original native timeMap should be preserved when vendor speed attrs are present"
        );
    }

    #[test]
    fn test_parse_fcpxml_native_time_map_multi_point_sets_speed_keyframes() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///a.mov" name="a" duration="480/24s"/>
  </resources>
  <library>
    <event>
      <project name="TimeMapRamp">
        <sequence duration="480/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0s" start="0s" duration="240/24s" name="a">
              <timeMap>
                <timept time="0s" value="0s" interp="linear"/>
                <timept time="48/24s" value="48/24s" interp="linear"/>
                <timept time="96/24s" value="144/24s" interp="linear"/>
              </timeMap>
            </asset-clip>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().expect("video").clips[0];
        assert!(!clip.reverse);
        assert!(!clip.freeze_frame);
        assert!((clip.speed - 1.0).abs() < 1e-6);
        assert!(
            clip.speed_keyframes.len() >= 2,
            "expected step keyframes for multi-point timeMap"
        );
    }

    #[test]
    fn test_parse_fcpxml_native_time_map_mixed_direction_is_preserved_unknown() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///a.mov" name="a" duration="480/24s"/>
  </resources>
  <library>
    <event>
      <project name="TimeMapUnsupported">
        <sequence duration="480/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0s" start="0s" duration="240/24s" name="a">
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

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().expect("video").clips[0];
        assert!(
            clip.fcpxml_unknown_children
                .iter()
                .any(|f| f.contains("<timeMap")),
            "unsupported mixed-direction native timeMap should be preserved"
        );
    }

    #[test]
    fn test_parse_fcpxml_native_time_map_smooth2_maps_to_ease_keyframes() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///a.mov" name="a" duration="480/24s"/>
  </resources>
  <library>
    <event>
      <project name="TimeMapSmooth2">
        <sequence duration="480/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0s" start="0s" duration="240/24s" name="a">
              <timeMap>
                <timept time="0s" value="0s" interp="smooth2"/>
                <timept time="48/24s" value="48/24s" interp="smooth2"/>
                <timept time="96/24s" value="144/24s" interp="smooth2"/>
              </timeMap>
            </asset-clip>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().expect("video").clips[0];
        assert!(
            clip.speed_keyframes
                .iter()
                .any(|kf| kf.interpolation == KeyframeInterpolation::EaseInOut),
            "smooth2 timept interpolation should map to ease interpolation"
        );
    }

    #[test]
    fn test_parse_fcpxml_native_time_map_with_in_out_time_is_preserved_unknown() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///a.mov" name="a" duration="480/24s"/>
  </resources>
  <library>
    <event>
      <project name="TimeMapInOut">
        <sequence duration="480/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0s" start="0s" duration="240/24s" name="a">
              <timeMap>
                <timept time="0s" value="0s" interp="smooth2" outTime="12/24s"/>
                <timept time="96/24s" value="144/24s" interp="smooth2" inTime="84/24s"/>
              </timeMap>
            </asset-clip>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().expect("video").clips[0];
        assert!(clip.speed_keyframes.is_empty());
        assert!(
            clip.fcpxml_unknown_children
                .iter()
                .any(|f| f.contains("inTime=") || f.contains("outTime=")),
            "timeMap with inTime/outTime should be preserved as unknown passthrough"
        );
    }

    #[test]
    fn test_parse_fcpxml_ref_clip_in_spine_maps_to_timeline_clip() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///ref.mov" name="ref-src" duration="240/24s"/>
  </resources>
  <library>
    <event>
      <project name="RefClipImport">
        <sequence duration="240/24s" format="r1">
          <spine>
            <ref-clip ref="a1" offset="0s" start="0s" duration="48/24s" name="ref-clip"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().expect("video").clips[0];
        assert_eq!(clip.label, "ref-clip");
        assert_eq!(clip.source_path, "/ref.mov");
    }

    #[test]
    fn test_parse_fcpxml_sync_clip_nested_spine_imports_child_clips() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///sync-a.mov" name="sync-a" duration="240/24s"/>
    <asset id="a2" src="file:///sync-b.mov" name="sync-b" duration="240/24s"/>
  </resources>
  <library>
    <event>
      <project name="SyncClipImport">
        <sequence duration="240/24s" format="r1">
          <spine>
            <sync-clip offset="0s" start="0s" duration="96/24s" name="sync-wrapper">
              <spine>
                <asset-clip ref="a1" offset="0s" start="0s" duration="48/24s" name="sync-a"/>
                <ref-clip ref="a2" offset="48/24s" start="0s" duration="48/24s" name="sync-b"/>
              </spine>
            </sync-clip>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let labels: Vec<String> = project
            .video_tracks()
            .flat_map(|t| t.clips.iter().map(|c| c.label.clone()))
            .collect();
        assert_eq!(labels, vec!["sync-a".to_string(), "sync-b".to_string()]);
    }

    #[test]
    fn test_parse_fcpxml_lane_audio_fallback_track_routing() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="v1" src="file:///video.mov" name="video" duration="240/24s" hasVideo="1" hasAudio="1"/>
    <asset id="a1" src="file:///audio.wav" name="audio" duration="240/24s" hasVideo="0" hasAudio="1"/>
  </resources>
  <library>
    <event>
      <project name="LaneFallback">
        <sequence duration="240/24s" format="r1">
          <spine>
            <asset-clip ref="v1" offset="0s" start="0s" duration="240/24s" name="video"/>
            <asset-clip ref="a1" lane="-1" offset="0s" start="0s" duration="240/24s" name="audio"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let video_tracks: Vec<_> = project.video_tracks().collect();
        let audio_tracks: Vec<_> = project.audio_tracks().collect();
        assert_eq!(video_tracks.len(), 1);
        assert_eq!(audio_tracks.len(), 1);
        assert_eq!(video_tracks[0].clips[0].label, "video");
        assert_eq!(audio_tracks[0].clips[0].label, "audio");
    }

    #[test]
    fn test_parse_fcpxml_asset_start_rebases_clip_start_for_lane_clips() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///video.mov" name="video" start="2400/24s" duration="480/24s" hasVideo="1" hasAudio="1"/>
  </resources>
  <library>
    <event>
      <project name="AssetStartRebase">
        <sequence duration="480/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0s" start="2400/24s" duration="48/24s" name="video1"/>
            <asset-clip ref="a1" lane="1" offset="0s" start="2448/24s" duration="48/24s" name="video2"/>
            <asset-clip ref="a1" lane="-1" offset="0s" start="2448/24s" duration="48/24s" name="audio1"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let video_tracks: Vec<_> = project.video_tracks().collect();
        let audio_tracks: Vec<_> = project.audio_tracks().collect();
        assert_eq!(video_tracks.len(), 2);
        assert_eq!(audio_tracks.len(), 1);

        let video2 = video_tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.label == "video2")
            .expect("video2 clip should exist");
        let audio1 = audio_tracks[0]
            .clips
            .iter()
            .find(|c| c.label == "audio1")
            .expect("audio1 clip should exist");
        assert_eq!(video2.source_in, 2_000_000_000);
        assert_eq!(video2.source_out, 4_000_000_000);
        assert_eq!(audio1.source_in, 2_000_000_000);
        assert_eq!(audio1.source_out, 4_000_000_000);
    }

    #[test]
    fn test_parse_fcpxml_clip_start_falls_back_to_relative_when_less_than_asset_start() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///video.mov" start="2400/24s" duration="480/24s"/>
  </resources>
  <project name="RelativeStartFallback">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="48/24s" duration="48/24s" name="clip"/>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        assert_eq!(clip.source_in, 2_000_000_000);
        assert_eq!(clip.source_out, 4_000_000_000);
    }

    #[test]
    fn test_parse_fcpxml_sequence_format_reference_applied() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1080" height="1920"/>
    <format id="r2" frameDuration="1/30s" width="3840" height="2160"/>
    <asset id="a1" src="file:///clip.mov" name="clip" duration="48/24s"/>
  </resources>
  <library>
    <event>
      <project name="FormatRef">
        <sequence duration="48/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0s" start="0s" duration="48/24s" name="clip"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        assert_eq!(project.width, 1080);
        assert_eq!(project.height, 1920);
        assert_eq!(project.frame_rate.numerator, 24);
        assert_eq!(project.frame_rate.denominator, 1);
    }

    #[test]
    fn test_parse_fcpxml_marker_in_clip_uses_offset_and_start() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.10">
  <resources>
    <format id="r1" width="1920" height="1080" frameDuration="1/24s"/>
    <asset id="a1" src="file:///clip.mov" duration="100/24s"/>
  </resources>
  <project name="Markers">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="10s" start="5s" duration="5s">
          <marker start="8s" duration="1/24s" value="Converted"/>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        assert_eq!(project.markers.len(), 1);
        assert_eq!(project.markers[0].position_ns, 13_000_000_000);
    }

    #[test]
    fn test_parse_fcpxml_chapter_marker_supported() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.10">
  <resources>
    <format id="r1" width="1920" height="1080" frameDuration="1/24s"/>
    <asset id="a1" src="file:///clip.mov" duration="200/24s"/>
  </resources>
  <project name="ChapterMarkers">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="5s" start="15s" duration="5s">
          <chapter-marker start="18s" duration="1/24s" value="Chapter 2"/>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        assert_eq!(project.markers.len(), 1);
        assert_eq!(project.markers[0].label, "Chapter 2");
        assert_eq!(project.markers[0].position_ns, 8_000_000_000);
    }

    #[test]
    fn test_parse_fcpxml_format_name_fallback_1080p30() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.10">
  <resources>
    <format id="r1" name="FFVideoFormat1080p30"/>
    <asset id="a1" src="file:///clip.mov" duration="10s"/>
  </resources>
  <project name="FormatNameOnly">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="1s"/>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        assert_eq!(project.width, 1920);
        assert_eq!(project.height, 1080);
        assert_eq!(project.frame_rate.numerator, 30);
        assert_eq!(project.frame_rate.denominator, 1);
    }

    #[test]
    fn test_parse_fcpxml_standard_transform_opacity_crop() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///clip.mov" duration="10s"/>
  </resources>
  <project name="StdAdjust">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="5s">
          <adjust-transform position="22.2222 -25" scale="0.5 0.5" rotation="90"/>
          <adjust-compositing opacity="0.6"/>
          <adjust-crop left="11" right="22" top="33" bottom="44"/>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        assert!((clip.position_x - 0.5).abs() < 1e-4);
        assert!((clip.position_y - 1.0).abs() < 1e-6);
        assert!((clip.scale - 0.5).abs() < 1e-6);
        assert_eq!(clip.rotate, 90);
        assert!((clip.opacity - 0.6).abs() < 1e-6);
        assert_eq!(clip.crop_left, 11);
        assert_eq!(clip.crop_right, 22);
        assert_eq!(clip.crop_top, 33);
        assert_eq!(clip.crop_bottom, 44);
        assert_eq!(project.width, 1920);
        assert_eq!(project.height, 1080);
    }

    #[test]
    fn test_parse_fcpxml_adjust_volume_db_maps_to_linear_volume() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///clip.mov" duration="10s"/>
    <asset id="a2" src="file:///clip2.mov" duration="10s"/>
  </resources>
  <project name="VolumeDb">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="5s">
          <adjust-volume amount="-6dB"/>
        </asset-clip>
        <asset-clip ref="a2" lane="1" offset="0s" start="0s" duration="5s">
          <adjust-volume amount="-96dB"/>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clips: Vec<_> = project
            .video_tracks()
            .flat_map(|t| t.clips.iter())
            .collect();
        assert_eq!(clips.len(), 2);
        let louder = clips
            .iter()
            .find(|c| c.source_path.ends_with("clip.mov"))
            .expect("clip a1 exists");
        let muted = clips
            .iter()
            .find(|c| c.source_path.ends_with("clip2.mov"))
            .expect("clip a2 exists");
        assert!(louder.volume > 0.49 && louder.volume < 0.51);
        assert_eq!(muted.volume, 0.0);
    }

    #[test]
    fn test_parse_fcpxml_crop_rect_variant() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///clip.mov" duration="10s"/>
  </resources>
  <project name="CropRect">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="5s">
          <adjust-crop>
            <crop-rect left="7" right="8" top="9" bottom="10"/>
          </adjust-crop>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        assert_eq!(clip.crop_left, 7);
        assert_eq!(clip.crop_right, 8);
        assert_eq!(clip.crop_top, 9);
        assert_eq!(clip.crop_bottom, 10);
    }

    #[test]
    fn test_parse_fcpxml_prefers_us_transform_attrs_when_present() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///clip.mov" duration="10s"/>
  </resources>
  <project name="UsTransformPriority">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="5s"
                    us:position-x="0.1" us:position-y="-0.2" us:scale="1.1" us:rotate="45">
          <adjust-transform position="960 -540" scale="2 2" rotation="90"/>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        assert!((clip.position_x - 0.1).abs() < 1e-6);
        assert!((clip.position_y + 0.2).abs() < 1e-6);
        assert!((clip.scale - 1.1).abs() < 1e-6);
        assert_eq!(clip.rotate, 45);
    }

    #[test]
    fn test_parse_fcpxml_scale_aware_position_conversion() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1001/30000s" width="1280" height="720"/>
    <asset id="a1" src="file:///clip.mov" duration="10s"/>
  </resources>
  <project name="ScaleAwarePosition">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="5s">
          <adjust-transform position="27.7778 -27.7778" scale="0.51 0.51" rotation="0"/>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        assert!((clip.position_x - 0.637755).abs() < 1e-4);
        assert!((clip.position_y - 1.133788).abs() < 1e-4);
        assert!((clip.scale - 0.51).abs() < 1e-6);
        assert_eq!(project.width, 1280);
        assert_eq!(project.height, 720);
    }

    #[test]
    fn test_parse_fcpxml_position_uses_sequence_dimensions() {
        let xml_1280 = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1001/30000s" width="1280" height="720"/>
    <asset id="a1" src="file:///clip.mov" duration="10s"/>
  </resources>
  <project name="Seq1280x720">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="5s">
          <adjust-transform position="27.7778 -27.7778" scale="0.51 0.51"/>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;
        let xml_1920 = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1001/30000s" width="1920" height="720"/>
    <asset id="a1" src="file:///clip.mov" duration="10s"/>
  </resources>
  <project name="Seq1920x720">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="5s">
          <adjust-transform position="27.7778 -27.7778" scale="0.51 0.51"/>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let p1280 = parse_fcpxml(xml_1280).expect("parse 1280x720");
        let p1920 = parse_fcpxml(xml_1920).expect("parse 1920x720");
        let c1280 = &p1280.video_tracks().next().unwrap().clips[0];
        let c1920 = &p1920.video_tracks().next().unwrap().clips[0];
        assert!(c1280.position_x > c1920.position_x);
        assert!((c1280.position_y - c1920.position_y).abs() < 1e-6);
    }

    #[test]
    fn test_parse_fcpxml_position_import_not_clamped_to_one() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1280" height="720"/>
    <asset id="a1" src="file:///clip.mov" duration="10s"/>
  </resources>
  <project name="LargeOffset">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="5s">
          <adjust-transform position="80 -80" scale="0.51 0.51" rotation="0"/>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        assert!(clip.position_x > 1.0);
        assert!(clip.position_y > 1.0);
    }

    #[test]
    fn test_parse_fcpxml_stores_original_source_xml() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
  </resources>
  <library>
    <event>
      <project name="SourceCapture">
        <sequence duration="0/24s" format="r1">
          <spine/>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        assert_eq!(project.source_fcpxml.as_deref(), Some(xml));
    }

    #[test]
    fn test_parse_fcpxml_remaps_missing_volumes_to_fcpxml_mount_root() {
        let unique = format!("ultimateslice-remap-{}.mp4", uuid::Uuid::new_v4());
        let remapped_target = format!("/tmp/{unique}");
        std::fs::write(&remapped_target, b"test").expect("should create remap target");
        let original_path = format!("/Volumes/{unique}");
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file://{original_path}" duration="10s"/>
  </resources>
  <project name="Remap">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="1s"/>
      </spine>
    </sequence>
  </project>
</fcpxml>"#
        );

        let project =
            parse_fcpxml_with_path(&xml, Some(std::path::Path::new("/tmp/project.fcpxml")))
                .expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        assert_eq!(clip.source_path, remapped_target);
        assert_eq!(
            clip.fcpxml_original_source_path.as_deref(),
            Some(original_path.as_str())
        );
        let _ = std::fs::remove_file(remapped_target);
    }

    #[test]
    fn test_parse_fcpxml_decodes_percent_encoded_media_rep_path_before_remap() {
        let unique = uuid::Uuid::new_v4().to_string();
        let folder = format!("Final Cut Original Media {unique}");
        let mount_dir = format!("/tmp/LEXAR/{folder}");
        std::fs::create_dir_all(&mount_dir).expect("should create remap directory");
        let remapped_target = format!("{mount_dir}/C0378.mp4");
        std::fs::write(&remapped_target, b"test").expect("should create remap target");

        let encoded_folder = folder.replace(' ', "%20");
        let encoded_original = format!("/Volumes/LEXAR/{encoded_folder}/C0378.mp4");
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file://{encoded_original}" duration="10s"/>
  </resources>
  <project name="RemapEncoded">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="1s"/>
      </spine>
    </sequence>
  </project>
</fcpxml>"#
        );

        let project =
            parse_fcpxml_with_path(&xml, Some(std::path::Path::new("/tmp/project.fcpxml")))
                .expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        let decoded_original = format!("/Volumes/LEXAR/{folder}/C0378.mp4");
        assert_eq!(clip.source_path, remapped_target);
        assert_eq!(
            clip.fcpxml_original_source_path.as_deref(),
            Some(decoded_original.as_str())
        );

        let _ = std::fs::remove_file(&remapped_target);
        let _ = std::fs::remove_dir_all(&mount_dir);
    }

    #[test]
    fn test_remap_candidates_include_common_linux_mount_paths() {
        let users = vec!["alice".to_string()];
        let candidates = remap_candidates_for_volumes_path(
            "/Volumes/DriveA/folder/file.mp4",
            Some(std::path::Path::new("/media")),
            &users,
        );
        let as_strings: Vec<String> = candidates
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert!(as_strings.contains(&"/Volumes/DriveA/folder/file.mp4".to_string()));
        assert!(as_strings.contains(&"/media/DriveA/folder/file.mp4".to_string()));
        assert!(as_strings.contains(&"/media/alice/DriveA/folder/file.mp4".to_string()));
        assert!(as_strings.contains(&"/run/media/alice/DriveA/folder/file.mp4".to_string()));
        assert!(as_strings.contains(&"/run/media/DriveA/folder/file.mp4".to_string()));
        assert!(as_strings.contains(&"/mnt/DriveA/folder/file.mp4".to_string()));
    }

    #[test]
    fn test_remap_candidates_non_volumes_path_unchanged() {
        let users = vec!["alice".to_string()];
        let candidates = remap_candidates_for_volumes_path(
            "/home/alice/file.mp4",
            Some(std::path::Path::new("/media")),
            &users,
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0], std::path::Path::new("/home/alice/file.mp4"));
    }

    #[test]
    fn test_decode_percent_encoded_path() {
        let decoded =
            decode_percent_encoded_path("/Volumes/LEXAR/Final%20Cut%20Original%20Media/C0378.mp4");
        assert_eq!(decoded, "/Volumes/LEXAR/Final Cut Original Media/C0378.mp4");
    }

    #[test]
    fn test_parse_track_state_vendor_attrs() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///tmp/clip.mp4" duration="2s"/>
  </resources>
  <project name="TrackState">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="1s"
          us:track-idx="0" us:track-kind="video" us:track-name="Video 1"
          us:track-muted="true" us:track-locked="true" us:track-soloed="true"
          us:track-height="large" us:color-label="purple"/>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let track = project
            .video_tracks()
            .next()
            .expect("video track should exist");
        assert!(track.muted);
        assert!(track.locked);
        assert!(track.soloed);
        assert_eq!(track.height_preset, TrackHeightPreset::Large);
        assert_eq!(
            track.clips[0].color_label,
            crate::model::clip::ClipColorLabel::Purple
        );
    }

    #[test]
    fn test_parse_native_fcp_position_keyframes() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///tmp/clip.mp4" duration="10s"/>
  </resources>
  <project name="FCP Keyframes">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="5s">
          <adjust-transform position="0 0" scale="0.5 0.5">
            <param name="position" value="0 0">
              <keyframeAnimation>
                <keyframe time="0s" value="-50 0" interp="linear"/>
                <keyframe time="5s" value="50 0" interp="linear"/>
              </keyframeAnimation>
            </param>
          </adjust-transform>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];

        assert_eq!(
            clip.position_x_keyframes.len(),
            2,
            "expected 2 position_x keyframes"
        );
        assert_eq!(
            clip.position_y_keyframes.len(),
            2,
            "expected 2 position_y keyframes"
        );

        // First keyframe at t=0s: FCPXML position (-50, 0)
        assert_eq!(clip.position_x_keyframes[0].time_ns, 0);
        assert_eq!(clip.position_y_keyframes[0].time_ns, 0);

        // Second keyframe at t=5s = 5_000_000_000 ns: FCPXML position (50, 0)
        assert_eq!(clip.position_x_keyframes[1].time_ns, 5_000_000_000);
        assert_eq!(clip.position_y_keyframes[1].time_ns, 5_000_000_000);

        // Position values should be opposite signs (left vs right)
        assert!(
            clip.position_x_keyframes[0].value < 0.0,
            "first kf should have negative x (left side)"
        );
        assert!(
            clip.position_x_keyframes[1].value > 0.0,
            "second kf should have positive x (right side)"
        );
    }

    #[test]
    fn test_parse_native_fcp_opacity_keyframes() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///tmp/clip.mp4" duration="10s"/>
  </resources>
  <project name="FCP Opacity">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="5s">
          <adjust-blend amount="1.0">
            <param name="amount">
              <keyframeAnimation>
                <keyframe time="0s" value="1.0"/>
                <keyframe time="2s" value="0.0"/>
              </keyframeAnimation>
            </param>
          </adjust-blend>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];

        assert_eq!(
            clip.opacity_keyframes.len(),
            2,
            "expected 2 opacity keyframes"
        );
        assert_eq!(clip.opacity_keyframes[0].time_ns, 0);
        assert!((clip.opacity_keyframes[0].value - 1.0).abs() < 0.001);
        assert_eq!(clip.opacity_keyframes[1].time_ns, 2_000_000_000);
        assert!((clip.opacity_keyframes[1].value - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_native_fcp_volume_keyframes() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///tmp/clip.mp4" duration="10s" hasAudio="1"/>
  </resources>
  <project name="FCP Volume">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="5s">
          <adjust-volume amount="0dB">
            <param name="amount">
              <keyframeAnimation>
                <keyframe time="0s" value="0dB"/>
                <keyframe time="3s" value="-96dB"/>
              </keyframeAnimation>
            </param>
          </adjust-volume>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let track = project.tracks.iter().find(|t| !t.clips.is_empty()).unwrap();
        let clip = &track.clips[0];

        assert_eq!(
            clip.volume_keyframes.len(),
            2,
            "expected 2 volume keyframes"
        );
        assert_eq!(clip.volume_keyframes[0].time_ns, 0);
        // 0dB = 1.0 linear
        assert!((clip.volume_keyframes[0].value - 1.0).abs() < 0.01);
        assert_eq!(clip.volume_keyframes[1].time_ns, 3_000_000_000);
        // -96dB = 0.0 linear (silence)
        assert!((clip.volume_keyframes[1].value - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_native_fcp_pan_keyframes() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///tmp/clip.mp4" duration="10s" hasAudio="1"/>
  </resources>
  <project name="FCP Pan">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="5s">
          <adjust-panner amount="0.0">
            <param name="amount">
              <keyframeAnimation>
                <keyframe time="0s" value="-1.0"/>
                <keyframe time="3s" value="1.0"/>
              </keyframeAnimation>
            </param>
          </adjust-panner>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let track = project.tracks.iter().find(|t| !t.clips.is_empty()).unwrap();
        let clip = &track.clips[0];
        assert_eq!(clip.pan_keyframes.len(), 2, "expected 2 pan keyframes");
        assert_eq!(clip.pan_keyframes[0].time_ns, 0);
        assert!((clip.pan_keyframes[0].value + 1.0).abs() < 0.001);
        assert_eq!(clip.pan_keyframes[1].time_ns, 3_000_000_000);
        assert!((clip.pan_keyframes[1].value - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_native_fcp_rotation_keyframes() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///tmp/clip.mp4" duration="10s"/>
  </resources>
  <project name="FCP Rotation">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="5s">
          <adjust-transform position="0 0" scale="1 1" rotation="0">
            <param name="rotation" value="0">
              <keyframeAnimation>
                <keyframe time="0s" value="-45" interp="linear"/>
                <keyframe time="3s" value="90" interp="easeInOut"/>
              </keyframeAnimation>
            </param>
          </adjust-transform>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        assert_eq!(
            clip.rotate_keyframes.len(),
            2,
            "expected 2 rotate keyframes"
        );
        assert_eq!(clip.rotate_keyframes[0].time_ns, 0);
        assert!((clip.rotate_keyframes[0].value - (-45.0)).abs() < 0.001);
        assert_eq!(clip.rotate_keyframes[1].time_ns, 3_000_000_000);
        assert!((clip.rotate_keyframes[1].value - 90.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_vendor_pan_keyframes() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14" xmlns:us="http://ultimateslice.io/ns">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///tmp/clip.mp4" duration="10s"/>
  </resources>
  <project name="Vendor Pan">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="5s"
          us:pan-keyframes="[{&quot;time_ns&quot;:0,&quot;value&quot;:-0.25,&quot;interpolation&quot;:&quot;linear&quot;},{&quot;time_ns&quot;:2000000000,&quot;value&quot;:0.75,&quot;interpolation&quot;:&quot;ease_in_out&quot;}]">
          <adjust-panner amount="0.0">
            <param name="amount">
              <keyframeAnimation>
                <keyframe time="0s" value="-1.0"/>
                <keyframe time="5s" value="1.0"/>
              </keyframeAnimation>
            </param>
          </adjust-panner>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        assert_eq!(clip.pan_keyframes.len(), 2);
        assert_eq!(clip.pan_keyframes[0].time_ns, 0);
        assert!((clip.pan_keyframes[0].value + 0.25).abs() < 0.001);
        assert_eq!(clip.pan_keyframes[1].time_ns, 2_000_000_000);
        assert!((clip.pan_keyframes[1].value - 0.75).abs() < 0.001);
    }

    #[test]
    fn test_parse_vendor_rotate_crop_keyframes() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14" xmlns:us="http://ultimateslice.io/ns">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///tmp/clip.mp4" duration="10s"/>
  </resources>
  <project name="Vendor RotateCrop">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="5s"
          us:rotate-keyframes="[{&quot;time_ns&quot;:0,&quot;value&quot;:0.0,&quot;interpolation&quot;:&quot;linear&quot;},{&quot;time_ns&quot;:2000000000,&quot;value&quot;:45.0,&quot;interpolation&quot;:&quot;ease_in_out&quot;}]"
          us:crop-left-keyframes="[{&quot;time_ns&quot;:0,&quot;value&quot;:0.0,&quot;interpolation&quot;:&quot;linear&quot;},{&quot;time_ns&quot;:2000000000,&quot;value&quot;:120.0,&quot;interpolation&quot;:&quot;linear&quot;}]"
          us:crop-bottom-keyframes="[{&quot;time_ns&quot;:0,&quot;value&quot;:0.0,&quot;interpolation&quot;:&quot;linear&quot;},{&quot;time_ns&quot;:2000000000,&quot;value&quot;:80.0,&quot;interpolation&quot;:&quot;linear&quot;}]"/>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        assert_eq!(clip.rotate_keyframes.len(), 2);
        assert_eq!(clip.crop_left_keyframes.len(), 2);
        assert_eq!(clip.crop_bottom_keyframes.len(), 2);
        assert_eq!(clip.rotate_keyframes[1].time_ns, 2_000_000_000);
        assert!((clip.rotate_keyframes[1].value - 45.0).abs() < 0.001);
        assert!((clip.crop_left_keyframes[1].value - 120.0).abs() < 0.001);
        assert!((clip.crop_bottom_keyframes[1].value - 80.0).abs() < 0.001);
    }

    #[test]
    fn test_vendor_keyframes_take_priority_over_native() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14" xmlns:us="http://ultimateslice.io/ns">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///tmp/clip.mp4" duration="10s"/>
  </resources>
  <project name="Priority Test">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="5s"
          us:opacity="0.8"
          us:opacity-keyframes="[{&quot;time_ns&quot;:0,&quot;value&quot;:0.8,&quot;interpolation&quot;:&quot;linear&quot;},{&quot;time_ns&quot;:1000000000,&quot;value&quot;:0.2,&quot;interpolation&quot;:&quot;linear&quot;}]">
          <adjust-compositing opacity="1.0">
            <param name="amount">
              <keyframeAnimation>
                <keyframe time="0s" value="1.0"/>
                <keyframe time="5s" value="0.0"/>
              </keyframeAnimation>
            </param>
          </adjust-compositing>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];

        // Vendor attrs should win: 2 keyframes from us:opacity-keyframes, NOT the native ones
        assert_eq!(clip.opacity_keyframes.len(), 2);
        assert_eq!(clip.opacity_keyframes[0].time_ns, 0);
        assert!((clip.opacity_keyframes[0].value - 0.8).abs() < 0.001);
        assert_eq!(clip.opacity_keyframes[1].time_ns, 1_000_000_000);
        assert!((clip.opacity_keyframes[1].value - 0.2).abs() < 0.001);
    }

    #[test]
    fn test_parse_native_fcp_scale_keyframes() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///tmp/clip.mp4" duration="10s"/>
  </resources>
  <project name="FCP Scale">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="5s">
          <adjust-transform position="0 0" scale="1 1">
            <param name="Scale" value="1 1">
              <keyframeAnimation>
                <keyframe time="0s" value="0.5 0.5" interp="linear"/>
                <keyframe time="5s" value="2.0 2.0" interp="linear"/>
              </keyframeAnimation>
            </param>
          </adjust-transform>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];

        assert_eq!(clip.scale_keyframes.len(), 2, "expected 2 scale keyframes");
        assert_eq!(clip.scale_keyframes[0].time_ns, 0);
        assert!((clip.scale_keyframes[0].value - 0.5).abs() < 0.001);
        assert_eq!(clip.scale_keyframes[1].time_ns, 5_000_000_000);
        assert!((clip.scale_keyframes[1].value - 2.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_native_fcp_curve_smooth_sets_bezier_controls() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///tmp/clip.mp4" duration="10s"/>
  </resources>
  <project name="FCP Smooth Curve">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="5s">
          <adjust-transform>
            <param name="scale">
              <keyframeAnimation>
                <keyframe time="0s" value="1.0 1.0" interp="easeOut" curve="smooth"/>
                <keyframe time="5s" value="2.0 2.0" interp="linear"/>
              </keyframeAnimation>
            </param>
          </adjust-transform>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        assert_eq!(clip.scale_keyframes.len(), 2);
        let first = &clip.scale_keyframes[0];
        assert_eq!(first.interpolation, KeyframeInterpolation::EaseOut);
        let bezier = first
            .bezier_controls
            .as_ref()
            .expect("curve=smooth should map to bezier controls");
        assert!((bezier.x1 - 0.0).abs() < 1e-9);
        assert!((bezier.y1 - 0.0).abs() < 1e-9);
        assert!((bezier.x2 - 0.58).abs() < 1e-9);
        assert!((bezier.y2 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_parse_native_fcp_transform_keyframes_with_offset() {
        // When start != "0s", keyframe times in FCPXML are absolute source-media
        // times. The parser must subtract source_in to produce clip-local times.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1001/24000s" width="1920" height="1080"/>
    <asset id="a1" src="file:///tmp/clip.mp4" start="1000000/24000s" duration="1200000/24000s"/>
  </resources>
  <project name="FCP Offset">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="1000000/24000s" duration="100100/24000s">
          <adjust-transform>
            <param name="position">
              <keyframeAnimation>
                <keyframe time="1000000/24000s" value="100 50"/>
                <keyframe time="1050000/24000s" value="200 100"/>
              </keyframeAnimation>
            </param>
            <param name="scale">
              <keyframeAnimation>
                <keyframe time="1000000/24000s" value="0.5 0.5"/>
                <keyframe time="1050000/24000s" value="1.5 1.5"/>
              </keyframeAnimation>
            </param>
            <param name="rotation">
              <keyframeAnimation>
                <keyframe time="1000000/24000s" value="0"/>
                <keyframe time="1050000/24000s" value="45"/>
              </keyframeAnimation>
            </param>
          </adjust-transform>
          <adjust-compositing>
            <param name="amount">
              <keyframeAnimation>
                <keyframe time="1000000/24000s" value="1.0" interp="linear"/>
                <keyframe time="1050000/24000s" value="0.5" interp="linear"/>
              </keyframeAnimation>
            </param>
          </adjust-compositing>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];

        // source_in = 1000000/24000s. Keyframes at 1000000 and 1050000.
        // Clip-local times should be 0/24000s and 50000/24000s.
        let source_in_ns = 1_000_000u64 * 1_000_000_000 / 24_000;
        let expected_offset_ns = 50_000u64 * 1_000_000_000 / 24_000;

        // Scale keyframes: clip-local times
        assert_eq!(clip.scale_keyframes.len(), 2);
        assert!(
            clip.scale_keyframes[0].time_ns <= 1,
            "first scale kf should be at ~0, got {}",
            clip.scale_keyframes[0].time_ns
        );
        let delta = clip.scale_keyframes[1].time_ns.abs_diff(expected_offset_ns);
        assert!(
            delta <= 1,
            "second scale kf off by {delta}ns (expected ~{expected_offset_ns})"
        );

        // Rotation keyframes: clip-local times
        assert_eq!(clip.rotate_keyframes.len(), 2);
        assert!(clip.rotate_keyframes[0].time_ns <= 1);
        let delta = clip.rotate_keyframes[1].time_ns.abs_diff(expected_offset_ns);
        assert!(delta <= 1, "second rotation kf off by {delta}ns");

        // Position keyframes: clip-local times
        assert_eq!(clip.position_x_keyframes.len(), 2);
        assert!(clip.position_x_keyframes[0].time_ns <= 1);
        let delta = clip.position_x_keyframes[1].time_ns.abs_diff(expected_offset_ns);
        assert!(delta <= 1, "second position_x kf off by {delta}ns");

        // Opacity keyframes: clip-local times
        assert_eq!(clip.opacity_keyframes.len(), 2);
        assert!(clip.opacity_keyframes[0].time_ns <= 1);
        let delta = clip.opacity_keyframes[1].time_ns.abs_diff(expected_offset_ns);
        assert!(delta <= 1, "second opacity kf off by {delta}ns");

        // Verify all keyframe times are < source_in (i.e. clip-local, not absolute)
        for kf in &clip.scale_keyframes {
            assert!(
                kf.time_ns < source_in_ns,
                "scale kf time {} should be clip-local, not absolute source time",
                kf.time_ns
            );
        }
    }

    #[test]
    fn test_parse_fcp_connected_clips_timeline_offset() {
        // Simulates FCP's connected clip nesting. Connected clips use `offset`
        // in the parent clip's source time space. The parser must convert to
        // absolute timeline positions.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1001/24000s" width="1920" height="1080"/>
    <asset id="a1" src="file:///tmp/primary.mp4" start="1000000/24000s" duration="1200000/24000s" hasVideo="1" hasAudio="1" format="r1"/>
    <asset id="a2" src="file:///tmp/connected_v.mp4" start="500000/24000s" duration="600000/24000s" hasVideo="1" hasAudio="1" format="r1"/>
    <asset id="a3" src="file:///tmp/connected_a.mp3" start="0s" duration="100s" hasVideo="0" hasAudio="1"/>
  </resources>
  <project name="Connected Test">
    <sequence format="r1" duration="200200/24000s">
      <spine>
        <asset-clip ref="a1" offset="0/24000s" name="Primary" start="1000000/24000s" duration="100100/24000s">
          <asset-clip ref="a2" lane="1" offset="1050050/24000s" name="ConnectedVideo" start="500000/24000s" duration="50050/24000s"/>
          <asset-clip ref="a3" lane="-1" offset="1000000/24000s" name="ConnectedAudio" duration="24024/24000s"/>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;
        let project = parse_fcpxml(xml).expect("should parse");
        // Primary clip at timeline 0
        let primary = project
            .video_tracks()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.label == "Primary")
            .expect("primary clip");
        assert_eq!(primary.timeline_start, 0);

        // Connected video (lane=1): offset 1050050 in parent source space.
        // Parent source start = 1000000. So timeline = 0 + (1050050 - 1000000)
        // = 50050/24000s ≈ 2.085s → 50050 * 1_000_000_000 / 24000 ns
        let connected_v = project
            .video_tracks()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.label == "ConnectedVideo")
            .expect("connected video clip");
        let expected_ns = 50050u64 * 1_000_000_000 / 24000;
        let delta = connected_v.timeline_start.abs_diff(expected_ns);
        assert!(
            delta <= 1,
            "connected video timeline_start off by {delta}ns"
        );

        // Connected audio (lane=-1): offset 1000000 = parent start → timeline = 0
        let connected_a = project
            .audio_tracks()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.label == "ConnectedAudio")
            .expect("connected audio clip");
        assert_eq!(connected_a.timeline_start, 0);
    }

    #[test]
    fn test_parse_volume_keyframes_inside_audio_channel_source() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///tmp/clip.mp4" duration="10s" hasAudio="1"/>
  </resources>
  <project name="FCP AudioChannelSource">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="0s" duration="5s">
          <audio-channel-source srcCh="1, 2" role="dialogue.dialogue-1">
            <adjust-volume>
              <param name="amount">
                <keyframeAnimation>
                  <keyframe time="0s" value="0dB"/>
                  <keyframe time="3s" value="-14.5dB"/>
                  <keyframe time="5s" value="-96dB"/>
                </keyframeAnimation>
              </param>
            </adjust-volume>
          </audio-channel-source>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let track = project.tracks.iter().find(|t| !t.clips.is_empty()).unwrap();
        let clip = &track.clips[0];

        assert_eq!(
            clip.volume_keyframes.len(),
            3,
            "expected 3 volume keyframes from audio-channel-source"
        );
        assert_eq!(clip.volume_keyframes[0].time_ns, 0);
        assert!((clip.volume_keyframes[0].value - 1.0).abs() < 0.01);
        assert_eq!(clip.volume_keyframes[1].time_ns, 3_000_000_000);
        // -14.5dB ≈ 0.1884 linear
        assert!(clip.volume_keyframes[1].value > 0.1 && clip.volume_keyframes[1].value < 0.3);
        assert_eq!(clip.volume_keyframes[2].time_ns, 5_000_000_000);
        assert!((clip.volume_keyframes[2].value - 0.0).abs() < 0.01);
    }

    /// Volume keyframes with a non-zero `start` (source timecode offset).
    /// FCP emits keyframe times in absolute source time; the parser must
    /// subtract `source_in` so they become clip-local.
    #[test]
    fn test_parse_volume_keyframes_source_in_subtracted() {
        // start="10s" means source_in = 10s.
        // Keyframes at 10s, 13s, 15s → clip-local 0s, 3s, 5s.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.14">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///tmp/clip.mp4" duration="20s" hasAudio="1"/>
  </resources>
  <project name="SourceIn Keyframes">
    <sequence format="r1">
      <spine>
        <asset-clip ref="a1" offset="0s" start="10s" duration="5s">
          <audio-channel-source srcCh="1, 2" role="dialogue.dialogue-1">
            <adjust-volume>
              <param name="amount">
                <keyframeAnimation>
                  <keyframe time="10s" value="0dB"/>
                  <keyframe time="13s" value="-14.5dB"/>
                  <keyframe time="15s" value="-96dB"/>
                </keyframeAnimation>
              </param>
            </adjust-volume>
          </audio-channel-source>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let track = project.tracks.iter().find(|t| !t.clips.is_empty()).unwrap();
        let clip = &track.clips[0];

        assert_eq!(
            clip.volume_keyframes.len(),
            3,
            "expected 3 volume keyframes"
        );
        // After subtracting source_in (10s), keyframes should be at 0s, 3s, 5s
        assert_eq!(clip.volume_keyframes[0].time_ns, 0);
        assert_eq!(clip.volume_keyframes[1].time_ns, 3_000_000_000);
        assert_eq!(clip.volume_keyframes[2].time_ns, 5_000_000_000);
    }

    #[test]
    fn test_parse_fcp_filter_video_color_adjustments() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.11">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///footage.mp4" name="footage" duration="240/24s"/>
    <effect id="r4" name="Color Adjustments" uid="FxPlug:7E2022A5-202B-4EEB-A311-AC2B585D01B0"/>
  </resources>
  <library>
    <event>
      <project name="ColorAdj">
        <sequence duration="240/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0/24s" duration="240/24s" start="0/24s" name="footage" format="r1" tcFormat="NDF">
              <filter-video ref="r4" name="Color Adjustments">
                <param name="Exposure" key="3" value="-7.5"/>
                <param name="Brightness" key="2" value="10"/>
                <param name="Contrast" key="17" value="25"/>
                <param name="Saturation" key="16" value="-50"/>
                <param name="Highlights" key="7" value="-20"/>
                <param name="Black Point" key="1" value="15"/>
                <param name="Shadows" key="4" value="30"/>
                <param name="Highlights Warmth" key="10" value="40"/>
                <param name="Highlights Tint" key="11" value="-10"/>
                <param name="Midtones Warmth" key="12" value="60"/>
                <param name="Midtones Tint" key="13" value="5"/>
                <param name="Shadows Warmth" key="14" value="-25"/>
                <param name="Shadows Tint" key="15" value="80"/>
              </filter-video>
            </asset-clip>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        assert!((clip.exposure - (-0.075)).abs() < 1e-5, "exposure");
        assert!((clip.brightness - 0.1).abs() < 1e-5, "brightness");
        assert!((clip.contrast - 1.25).abs() < 1e-5, "contrast: /100+1");
        assert!((clip.saturation - 0.5).abs() < 1e-5, "saturation: /100+1");
        assert!((clip.highlights - (-0.2)).abs() < 1e-5, "highlights");
        assert!((clip.black_point - 0.15).abs() < 1e-5, "black_point");
        assert!((clip.shadows - 0.3).abs() < 1e-5, "shadows");
        assert!(
            (clip.highlights_warmth - 0.4).abs() < 1e-5,
            "highlights_warmth"
        );
        assert!(
            (clip.highlights_tint - (-0.1)).abs() < 1e-5,
            "highlights_tint"
        );
        assert!((clip.midtones_warmth - 0.6).abs() < 1e-5, "midtones_warmth");
        assert!((clip.midtones_tint - 0.05).abs() < 1e-5, "midtones_tint");
        assert!(
            (clip.shadows_warmth - (-0.25)).abs() < 1e-5,
            "shadows_warmth"
        );
        assert!((clip.shadows_tint - 0.8).abs() < 1e-5, "shadows_tint");
    }

    #[test]
    fn test_parse_fcp_filter_video_non_color_adj_preserved_as_unknown() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.11">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///footage.mp4" name="footage" duration="240/24s"/>
    <effect id="r5" name="Gaussian Blur" uid="FxPlug:AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE"/>
  </resources>
  <library>
    <event>
      <project name="OtherFilter">
        <sequence duration="240/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0/24s" duration="240/24s" start="0/24s" name="footage" format="r1" tcFormat="NDF">
              <filter-video ref="r5" name="Gaussian Blur">
                <param name="Amount" key="1" value="50"/>
              </filter-video>
            </asset-clip>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        // Non-Color-Adjustments filter-video should be preserved as unknown XML
        assert!(
            clip.fcpxml_unknown_children
                .iter()
                .any(|s: &String| s.contains("Gaussian Blur")),
            "Non-Color-Adjustments filter-video should be preserved as unknown XML"
        );
        // Color fields should remain at defaults
        assert!((clip.exposure - 0.0).abs() < 1e-5);
        assert!((clip.brightness - 0.0).abs() < 1e-5);
    }

    #[test]
    fn test_parse_uspxml_color_adjustment_vendor_attrs() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.10" xmlns:us="urn:ultimateslice">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
    <asset id="a1" src="file:///footage.mp4" name="footage" duration="240/24s"/>
  </resources>
  <library>
    <event>
      <project name="VendorColor">
        <sequence duration="240/24s" format="r1">
          <spine>
            <asset-clip ref="a1" offset="0/24s" duration="240/24s" start="0/24s"
                        name="footage" us:track-idx="0" us:track-kind="video" us:track-name="V1"
                        us:exposure="0.5" us:black-point="-0.3"
                        us:highlights-warmth="0.2" us:highlights-tint="-0.1"
                        us:midtones-warmth="0.4" us:midtones-tint="0.15"
                        us:shadows-warmth="-0.6" us:shadows-tint="0.7"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        assert!((clip.exposure - 0.5).abs() < 1e-5, "exposure");
        assert!((clip.black_point - (-0.3)).abs() < 1e-5, "black_point");
        assert!(
            (clip.highlights_warmth - 0.2).abs() < 1e-5,
            "highlights_warmth"
        );
        assert!(
            (clip.highlights_tint - (-0.1)).abs() < 1e-5,
            "highlights_tint"
        );
        assert!((clip.midtones_warmth - 0.4).abs() < 1e-5, "midtones_warmth");
        assert!((clip.midtones_tint - 0.15).abs() < 1e-5, "midtones_tint");
        assert!(
            (clip.shadows_warmth - (-0.6)).abs() < 1e-5,
            "shadows_warmth"
        );
        assert!((clip.shadows_tint - 0.7).abs() < 1e-5, "shadows_tint");
    }

    #[test]
    fn test_parse_silence_removal_shared_asset_clips() {
        // Regression: silence-removal creates multiple clips from the same source
        // with different start times, sharing a single <asset>.
        // Verify source_in/source_out are parsed correctly (relative to timecode base).
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE fcpxml>
<fcpxml version="1.14" xmlns:us="urn:ultimateslice">
    <resources>
        <format id="r1" name="FFVideoFormat1080p24" frameDuration="1/24s" width="1920" height="1080"/>
        <asset id="a1" name="C0381" start="1700856/24s" format="r1" hasVideo="1" hasAudio="1">
            <media-rep kind="original-media" src="file:///tmp/C0381.MP4"/>
        </asset>
    </resources>
    <library><event><project name="Test">
        <sequence duration="546/24s" format="r1" tcFormat="NDF"><spine>
            <asset-clip ref="a1" offset="0/24s" duration="50/24s" start="1700856/24s" name="C0381"
                us:track-idx="0" us:track-kind="video" us:track-name="Video 1"
                us:source-timecode-base-ns="70869000000000"/>
            <asset-clip ref="a1" offset="50/24s" duration="475/24s" start="1700967/24s" name="C0381"
                us:track-idx="0" us:track-kind="video" us:track-name="Video 1"
                us:source-timecode-base-ns="70869000000000"/>
            <asset-clip ref="a1" offset="525/24s" duration="21/24s" start="1701495/24s" name="C0381"
                us:track-idx="0" us:track-kind="video" us:track-name="Video 1"
                us:source-timecode-base-ns="70869000000000"/>
        </spine></sequence>
    </project></event></library>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse silence-removal XML");
        let clips: Vec<_> = project.tracks[0].clips.iter().collect();
        assert_eq!(clips.len(), 3, "expected 3 clips");

        let base_ns: u64 = 70_869_000_000_000;

        // Clip 1: start=1700856/24s, source_in should be 0 (start == base)
        let c1_expected_source_in = 0u64;
        assert_eq!(clips[0].source_in, c1_expected_source_in,
            "clip 1 source_in: {} != {}", clips[0].source_in, c1_expected_source_in);
        let c1_dur = parse_fcpxml_time("50/24s").unwrap();
        assert_eq!(clips[0].source_out, c1_expected_source_in + c1_dur,
            "clip 1 source_out");
        assert_eq!(clips[0].source_timecode_base_ns, Some(base_ns));

        // Clip 2: start=1700967/24s, source_in = (1700967/24 - 70869) seconds
        let c2_start_ns = parse_fcpxml_time("1700967/24s").unwrap();
        let c2_expected_source_in = c2_start_ns - base_ns;
        assert_eq!(clips[1].source_in, c2_expected_source_in,
            "clip 2 source_in: {} != {} ({:.6}s != {:.6}s)",
            clips[1].source_in, c2_expected_source_in,
            clips[1].source_in as f64 / 1e9, c2_expected_source_in as f64 / 1e9);
        let c2_dur = parse_fcpxml_time("475/24s").unwrap();
        assert_eq!(clips[1].source_out, c2_expected_source_in + c2_dur,
            "clip 2 source_out");

        // Clip 3: start=1701495/24s
        let c3_start_ns = parse_fcpxml_time("1701495/24s").unwrap();
        let c3_expected_source_in = c3_start_ns - base_ns;
        assert_eq!(clips[2].source_in, c3_expected_source_in,
            "clip 3 source_in: {} != {} ({:.6}s != {:.6}s)",
            clips[2].source_in, c3_expected_source_in,
            clips[2].source_in as f64 / 1e9, c3_expected_source_in as f64 / 1e9);
        let c3_dur = parse_fcpxml_time("21/24s").unwrap();
        assert_eq!(clips[2].source_out, c3_expected_source_in + c3_dur,
            "clip 3 source_out");

        // Verify source_in values are reasonable seconds into the file
        assert!(clips[0].source_in == 0, "clip 1 starts at file beginning");
        assert!(clips[1].source_in > 4_000_000_000 && clips[1].source_in < 5_000_000_000,
            "clip 2 source_in should be ~4.625s, got {}s", clips[1].source_in as f64 / 1e9);
        assert!(clips[2].source_in > 26_000_000_000 && clips[2].source_in < 27_000_000_000,
            "clip 3 source_in should be ~26.625s, got {}s", clips[2].source_in as f64 / 1e9);
    }
}
