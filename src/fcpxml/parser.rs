use crate::model::clip::{Clip, ClipKind};
use crate::model::project::{FrameRate, Project};
use crate::model::track::Track;
use anyhow::{anyhow, bail, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::{BTreeMap, HashMap};

/// Represents a parsed FCPXML asset
struct Asset {
    id: String,
    src: String,
    name: String,
    duration_ns: u64,
    has_video: bool,
    has_audio: bool,
}

#[derive(Clone)]
struct FormatSpec {
    width: u32,
    height: u32,
    frame_rate: FrameRate,
}

struct AssetBuilder {
    id: String,
    src: Option<String>,
    name: String,
    duration_ns: u64,
    has_video: bool,
    has_audio: bool,
}

#[derive(Clone, Copy)]
struct ActiveClipContext {
    track_key: (u8, usize),
    clip_index: usize,
    timeline_start: u64,
    source_in: u64,
}

/// Parse an FCPXML string into a `Project`.
pub fn parse_fcpxml(xml: &str) -> Result<Project> {
    let mut reader = Reader::from_str(xml);
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
    let mut in_selected_project = false;
    let mut selected_project_seen = false;
    let mut in_selected_sequence = false;
    let mut selected_sequence_seen = false;
    let mut selected_spine_seen = false;
    let mut selected_sequence_format_applied = false;
    let mut current_asset: Option<AssetBuilder> = None;
    let mut clip_stack: Vec<ActiveClipContext> = Vec::new();

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
                    }
                    "project" => {
                        let attrs = parse_attrs(e)?;
                        if !selected_project_seen {
                            selected_project_seen = true;
                            in_selected_project = true;
                            if let Some(n) = attrs.get("name") {
                                project.title = n.clone();
                            }
                        } else {
                            in_selected_project = false;
                        }
                    }
                    "sequence" => {
                        let attrs = parse_attrs(e)?;
                        if in_selected_project && !selected_sequence_seen {
                            selected_sequence_seen = true;
                            in_selected_sequence = true;
                            if let Some(fmt_ref) = attrs.get("format") {
                                if let Some(spec) = format_specs.get(fmt_ref) {
                                    project.width = spec.width;
                                    project.height = spec.height;
                                    project.frame_rate = spec.frame_rate.clone();
                                    selected_sequence_format_applied = true;
                                }
                            }
                        }
                    }
                    "format" => {
                        let attrs = parse_attrs(e)?;
                        if let Some((id, spec)) = parse_format_spec(&attrs) {
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
                                asset.src = Some(src.replace("file://", ""));
                            }
                        }
                    }
                    "media-rep" => {
                        let attrs = parse_attrs(e)?;
                        if let Some(asset) = current_asset.as_mut() {
                            if asset.src.is_none() {
                                if let Some(src) = attrs.get("src") {
                                    asset.src = Some(src.replace("file://", ""));
                                }
                            }
                        }
                    }
                    "spine" => {
                        if in_selected_sequence && !selected_spine_seen {
                            in_spine = true;
                            selected_spine_seen = true;
                        }
                    }
                    "asset-clip" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        if let Some(ctx) = parse_asset_clip(&attrs, &assets, &mut track_map) {
                            clip_stack.push(ctx);
                        }
                    }
                    "adjust-transform" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        apply_adjust_transform(&attrs, clip_stack.last(), &mut track_map);
                    }
                    "adjust-compositing" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        apply_adjust_compositing(&attrs, clip_stack.last(), &mut track_map);
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
                    }
                    "format" => {
                        let attrs = parse_attrs(e)?;
                        if let Some((id, spec)) = parse_format_spec(&attrs) {
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
                                asset.src = Some(src.replace("file://", ""));
                            }
                            finalize_asset(asset, &mut assets);
                        }
                    }
                    "media-rep" => {
                        let attrs = parse_attrs(e)?;
                        if let Some(asset) = current_asset.as_mut() {
                            if asset.src.is_none() {
                                if let Some(src) = attrs.get("src") {
                                    asset.src = Some(src.replace("file://", ""));
                                }
                            }
                        }
                    }
                    "asset-clip" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        parse_asset_clip(&attrs, &assets, &mut track_map);
                    }
                    "adjust-transform" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        apply_adjust_transform(&attrs, clip_stack.last(), &mut track_map);
                    }
                    "adjust-compositing" if in_spine => {
                        let attrs = parse_attrs(e)?;
                        apply_adjust_compositing(&attrs, clip_stack.last(), &mut track_map);
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
                    _ => {}
                }
            }
            Event::End(ref e) => {
                let name_local = e.local_name();
                let name = std::str::from_utf8(name_local.as_ref())?;
                match name {
                    "asset" => {
                        if let Some(asset) = current_asset.take() {
                            finalize_asset(asset, &mut assets);
                        }
                    }
                    "spine" => {
                        if in_spine {
                            in_spine = false;
                            clip_stack.clear();
                        }
                    }
                    "asset-clip" => {
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
        if let Some(spec) = default_format {
            project.width = spec.width;
            project.height = spec.height;
            project.frame_rate = spec.frame_rate;
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

    Ok(project)
}

fn parse_asset_clip(
    attrs: &HashMap<String, String>,
    assets: &HashMap<String, Asset>,
    track_map: &mut BTreeMap<(u8, usize), Track>,
) -> Option<ActiveClipContext> {
    if let Some(asset_ref) = attrs.get("ref") {
        if let Some(asset) = assets.get(asset_ref) {
            let timeline_start = attrs
                .get("offset")
                .and_then(|t| parse_fcpxml_time(t))
                .unwrap_or(0);
            let source_in = attrs
                .get("start")
                .and_then(|t| parse_fcpxml_time(t))
                .unwrap_or(0);
            let duration = attrs
                .get("duration")
                .and_then(|t| parse_fcpxml_time(t))
                .unwrap_or(asset.duration_ns);
            let label = attrs
                .get("name")
                .cloned()
                .unwrap_or_else(|| asset.name.clone());

            let lane = attrs.get("lane").and_then(|s| s.parse::<i32>().ok());
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
                ClipKind::Video | ClipKind::Image => {
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
            let track_key = (if clip_kind == ClipKind::Audio { 1 } else { 0 }, track_idx);

            // Get or create the target track
            let track = track_map.entry(track_key).or_insert_with(|| {
                if clip_kind == ClipKind::Audio {
                    Track::new_audio(&track_name)
                } else {
                    Track::new_video(&track_name)
                }
            });

            let mut clip = Clip::new(&asset.src, source_in + duration, timeline_start, clip_kind);
            clip.source_in = source_in;
            clip.source_out = source_in + duration;
            clip.timeline_start = timeline_start;
            clip.label = label;
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
            if let Some(v) = attrs.get("us:denoise") {
                clip.denoise = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:sharpness") {
                clip.sharpness = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:volume") {
                clip.volume = v.parse().unwrap_or(1.0);
            }
            if let Some(v) = attrs.get("us:pan") {
                clip.pan = v.parse().unwrap_or(0.0);
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
            if let Some(v) = attrs.get("us:opacity") {
                clip.opacity = v.parse().unwrap_or(1.0);
            }
            if let Some(v) = attrs.get("us:position-x") {
                clip.position_x = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:position-y") {
                clip.position_y = v.parse().unwrap_or(0.0);
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
            if let Some(v) = attrs.get("us:speed") {
                clip.speed = v.parse().unwrap_or(1.0);
            }
            if let Some(v) = attrs.get("us:reverse") {
                clip.reverse = v.parse().unwrap_or(false);
            }
            if let Some(v) = attrs.get("us:shadows") {
                clip.shadows = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:midtones") {
                clip.midtones = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:highlights") {
                clip.highlights = v.parse().unwrap_or(0.0);
            }
            if let Some(v) = attrs.get("us:lut-path") {
                clip.lut_path = Some(v.clone());
            }
            if let Some(v) = attrs.get("us:transition-after") {
                clip.transition_after = v.clone();
            }
            if let Some(v) = attrs.get("us:transition-after-ns") {
                clip.transition_after_ns = v.parse().unwrap_or(0);
            }
            let clip_index = track.clips.len();
            track.push_unsorted(clip);
            return Some(ActiveClipContext {
                track_key,
                clip_index,
                timeline_start,
                source_in,
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

fn apply_adjust_transform(
    attrs: &HashMap<String, String>,
    active_ctx: Option<&ActiveClipContext>,
    track_map: &mut BTreeMap<(u8, usize), Track>,
) {
    if let Some(clip) = current_clip_mut(track_map, active_ctx) {
        if let Some(pos) = attrs.get("position").and_then(|s| parse_vec2(s)) {
            clip.position_x = pos.0;
            clip.position_y = pos.1;
        }
        if let Some(scale) = attrs.get("scale").and_then(|s| parse_vec2(s)) {
            clip.scale = scale.0;
        }
        if let Some(rot) = attrs.get("rotation").and_then(|s| s.parse::<f64>().ok()) {
            clip.rotate = rot.round() as i32;
        }
    }
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

fn parse_vec2(value: &str) -> Option<(f64, f64)> {
    let mut parts = value.split_whitespace();
    let x = parts.next()?.parse().ok()?;
    let y = parts.next()?.parse().ok()?;
    Some((x, y))
}

fn parse_crop_value(value: &str) -> Option<i32> {
    value.parse::<f64>().ok().map(|v| v.round() as i32)
}

fn build_asset_builder(attrs: &HashMap<String, String>) -> Option<AssetBuilder> {
    let id = attrs.get("id")?.clone();
    let duration_ns = attrs
        .get("duration")
        .and_then(|d| parse_fcpxml_time(d))
        .unwrap_or(0);
    Some(AssetBuilder {
        id,
        src: None,
        name: attrs.get("name").cloned().unwrap_or_default(),
        duration_ns,
        has_video: parse_flag(attrs.get("hasVideo"), true),
        has_audio: parse_flag(attrs.get("hasAudio"), true),
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
                has_video: asset.has_video,
                has_audio: asset.has_audio,
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
    Some((
        id,
        FormatSpec {
            width,
            height,
            frame_rate,
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
        });
    }
    if n.contains("1080p24") {
        return Some(FormatSpec {
            width: 1920,
            height: 1080,
            frame_rate: FrameRate::fps_24(),
        });
    }
    if n.contains("2160p24") {
        return Some(FormatSpec {
            width: 3840,
            height: 2160,
            frame_rate: FrameRate::fps_24(),
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
    let mut attrs = e.attributes();
    let mut map = HashMap::with_capacity(attrs.size_hint().0);
    for attr in attrs {
        let attr = attr?;
        let key = std::str::from_utf8(attr.key.as_ref())?.to_string();
        let value = std::str::from_utf8(attr.value.as_ref())?.to_string();
        map.insert(key, value);
    }
    Ok(map)
}

/// Parse an FCPXML time string like "48/24s" or "48048/24000s" into nanoseconds
fn parse_fcpxml_time(s: &str) -> Option<u64> {
    let s = s.trim_end_matches('s');
    if let Some((num, den)) = s.split_once('/') {
        let num: u64 = num.parse().ok()?;
        let den: u64 = den.parse().ok()?;
        if den == 0 {
            return None;
        }
        // time_seconds = num / den; ns = time_seconds * 1_000_000_000
        Some(num * 1_000_000_000 / den)
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
                        us:opacity="0.9" us:speed="2.0"/>
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
        assert!((clip.opacity - 0.9).abs() < 1e-5);
        assert!((clip.speed - 2.0).abs() < 1e-5);
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
          <adjust-transform position="0.2 -0.3" scale="1.25 1.25" rotation="90"/>
          <adjust-compositing opacity="0.6"/>
          <adjust-crop left="11" right="22" top="33" bottom="44"/>
        </asset-clip>
      </spine>
    </sequence>
  </project>
</fcpxml>"#;

        let project = parse_fcpxml(xml).expect("parse should succeed");
        let clip = &project.video_tracks().next().unwrap().clips[0];
        assert!((clip.position_x - 0.2).abs() < 1e-6);
        assert!((clip.position_y + 0.3).abs() < 1e-6);
        assert!((clip.scale - 1.25).abs() < 1e-6);
        assert_eq!(clip.rotate, 90);
        assert!((clip.opacity - 0.6).abs() < 1e-6);
        assert_eq!(clip.crop_left, 11);
        assert_eq!(clip.crop_right, 22);
        assert_eq!(clip.crop_top, 33);
        assert_eq!(clip.crop_bottom, 44);
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
}
