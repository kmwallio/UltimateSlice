use crate::model::project::Project;
use anyhow::Result;
use quick_xml::events::{BytesEnd, BytesStart, Event};
use quick_xml::Writer;
use std::collections::HashMap;
use std::io::{Cursor, Write};

/// Serialize a `Project` to FCPXML format.
pub fn write_fcpxml(project: &Project) -> Result<String> {
    if let Some(original) = &project.source_fcpxml {
        if !project.dirty {
            return Ok(original.clone());
        }
        if let Some(patched) = patch_imported_fcpxml_transform(project, original) {
            return Ok(patched);
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
    fcpxml.push_attribute(("xmlns:us", "urn:ultimateslice"));
    for (k, v) in &project.fcpxml_unknown_root.attrs {
        if !is_writer_managed_fcpxml_attr(k) {
            fcpxml.push_attribute((k.as_str(), v.as_str()));
        }
    }
    writer.write_event(Event::Start(fcpxml))?;

    // <resources>
    write_resources(project, &mut writer)?;

    // <library>
    let mut library = BytesStart::new("library");
    for (k, v) in &project.fcpxml_unknown_library.attrs {
        if !is_writer_managed_library_attr(k) {
            library.push_attribute((k.as_str(), v.as_str()));
        }
    }
    writer.write_event(Event::Start(library))?;

    let mut event = BytesStart::new("event");
    for (k, v) in &project.fcpxml_unknown_event.attrs {
        if !is_writer_managed_event_attr(k) {
            event.push_attribute((k.as_str(), v.as_str()));
        }
    }
    writer.write_event(Event::Start(event))?;

    // <project name="...">
    let mut proj_elem = BytesStart::new("project");
    proj_elem.push_attribute(("name", project.title.as_str()));
    for (k, v) in &project.fcpxml_unknown_project.attrs {
        if !is_writer_managed_project_attr(k) {
            proj_elem.push_attribute((k.as_str(), v.as_str()));
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
    for (k, v) in &project.fcpxml_unknown_sequence.attrs {
        if !is_writer_managed_sequence_attr(k) {
            seq.push_attribute((k.as_str(), v.as_str()));
        }
    }
    writer.write_event(Event::Start(seq))?;

    // <spine>
    let mut spine = BytesStart::new("spine");
    for (k, v) in &project.fcpxml_unknown_spine.attrs {
        if !is_writer_managed_spine_attr(k) {
            spine.push_attribute((k.as_str(), v.as_str()));
        }
    }
    writer.write_event(Event::Start(spine))?;

    // Emit all clips from all tracks, tagging each with us:track-idx and us:track-kind
    // so the parser can reconstruct the full multi-track layout.
    let all_tracks: Vec<_> = project.tracks.iter().enumerate().collect();
    for (track_idx, track) in &all_tracks {
        let track_kind = match track.kind {
            crate::model::track::TrackKind::Video => "video",
            crate::model::track::TrackKind::Audio => "audio",
        };
        for clip in &track.clips {
            let asset_ref = format!("a_{}", sanitize_id(&clip.id));
            let offset = ns_to_fcpxml_time(clip.timeline_start, &project.frame_rate);
            let duration = ns_to_fcpxml_time(clip.duration(), &project.frame_rate);
            let start = ns_to_fcpxml_time(
                clip.source_timecode_start_ns().unwrap_or(clip.source_in),
                &project.frame_rate,
            );

            let mut asset_clip = BytesStart::new("asset-clip");
            asset_clip.push_attribute(("ref", asset_ref.as_str()));
            asset_clip.push_attribute(("offset", offset.as_str()));
            asset_clip.push_attribute(("duration", duration.as_str()));
            asset_clip.push_attribute(("start", start.as_str()));
            asset_clip.push_attribute(("name", clip.label.as_str()));
            // Multi-track routing
            asset_clip.push_attribute(("us:track-idx", track_idx.to_string().as_str()));
            asset_clip.push_attribute(("us:track-kind", track_kind));
            asset_clip.push_attribute(("us:track-name", track.label.as_str()));
            asset_clip.push_attribute(("us:track-muted", track.muted.to_string().as_str()));
            asset_clip.push_attribute(("us:track-locked", track.locked.to_string().as_str()));
            asset_clip.push_attribute(("us:track-soloed", track.soloed.to_string().as_str()));
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
            // Store color/effects as custom vendor attributes (us: prefix).
            // Final Cut Pro ignores unknown attributes, so round-trip is lossless.
            asset_clip.push_attribute(("us:brightness", clip.brightness.to_string().as_str()));
            asset_clip.push_attribute(("us:contrast", clip.contrast.to_string().as_str()));
            asset_clip.push_attribute(("us:saturation", clip.saturation.to_string().as_str()));
            asset_clip.push_attribute(("us:temperature", clip.temperature.to_string().as_str()));
            asset_clip.push_attribute(("us:tint", clip.tint.to_string().as_str()));
            asset_clip.push_attribute(("us:denoise", clip.denoise.to_string().as_str()));
            asset_clip.push_attribute(("us:sharpness", clip.sharpness.to_string().as_str()));
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
            asset_clip.push_attribute(("us:crop-left", clip.crop_left.to_string().as_str()));
            asset_clip.push_attribute(("us:crop-right", clip.crop_right.to_string().as_str()));
            asset_clip.push_attribute(("us:crop-top", clip.crop_top.to_string().as_str()));
            asset_clip.push_attribute(("us:crop-bottom", clip.crop_bottom.to_string().as_str()));
            asset_clip.push_attribute(("us:rotate", clip.rotate.to_string().as_str()));
            asset_clip.push_attribute(("us:flip-h", clip.flip_h.to_string().as_str()));
            asset_clip.push_attribute(("us:flip-v", clip.flip_v.to_string().as_str()));
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
            asset_clip.push_attribute(("us:position-x", clip.position_x.to_string().as_str()));
            let position_x_keyframes_json = if clip.position_x_keyframes.is_empty() {
                None
            } else {
                serde_json::to_string(&clip.position_x_keyframes).ok()
            };
            if let Some(value) = position_x_keyframes_json.as_deref() {
                asset_clip.push_attribute(("us:position-x-keyframes", value));
            }
            asset_clip.push_attribute(("us:position-y", clip.position_y.to_string().as_str()));
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
            asset_clip.push_attribute(("us:speed", clip.speed.to_string().as_str()));
            asset_clip.push_attribute(("us:reverse", clip.reverse.to_string().as_str()));
            if clip.freeze_frame {
                asset_clip.push_attribute(("us:freeze-frame", "true"));
            }
            if let Some(freeze_source_ns) = clip.freeze_frame_source_ns {
                asset_clip
                    .push_attribute(("us:freeze-source-ns", freeze_source_ns.to_string().as_str()));
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
            asset_clip.push_attribute(("us:highlights", clip.highlights.to_string().as_str()));
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
            if let Some(ref lut) = clip.lut_path {
                asset_clip.push_attribute(("us:lut-path", lut.as_str()));
            }
            if !clip.transition_after.is_empty() {
                asset_clip.push_attribute(("us:transition-after", clip.transition_after.as_str()));
                asset_clip.push_attribute((
                    "us:transition-after-ns",
                    clip.transition_after_ns.to_string().as_str(),
                ));
            }
            for (k, v) in &clip.fcpxml_unknown_attrs {
                if !is_writer_managed_asset_clip_attr(k) {
                    asset_clip.push_attribute((k.as_str(), v.as_str()));
                }
            }
            writer.write_event(Event::Start(asset_clip))?;

            let (position_x, position_y) = internal_position_to_fcpxml(
                clip.position_x,
                clip.position_y,
                project.width,
                project.height,
                clip.scale,
            );
            let mut adjust_transform = BytesStart::new("adjust-transform");
            adjust_transform.push_attribute((
                "position",
                format!("{} {}", position_x, position_y).as_str(),
            ));
            adjust_transform
                .push_attribute(("scale", format!("{} {}", clip.scale, clip.scale).as_str()));
            adjust_transform.push_attribute(("rotation", clip.rotate.to_string().as_str()));
            let has_transform_kfs = !clip.position_x_keyframes.is_empty()
                || !clip.position_y_keyframes.is_empty()
                || !clip.scale_keyframes.is_empty();
            if has_transform_kfs {
                writer.write_event(Event::Start(adjust_transform))?;
                write_transform_keyframe_params(&mut writer, clip, project)?;
                writer.write_event(Event::End(BytesEnd::new("adjust-transform")))?;
            } else {
                writer.write_event(Event::Empty(adjust_transform))?;
            }

            let mut adjust_compositing = BytesStart::new("adjust-compositing");
            adjust_compositing.push_attribute(("opacity", clip.opacity.to_string().as_str()));
            if !clip.opacity_keyframes.is_empty() {
                writer.write_event(Event::Start(adjust_compositing))?;
                write_opacity_keyframe_params(&mut writer, clip, &project.frame_rate)?;
                writer.write_event(Event::End(BytesEnd::new("adjust-compositing")))?;
            } else {
                writer.write_event(Event::Empty(adjust_compositing))?;
            }

            // Emit <adjust-volume> with optional keyframes
            {
                let mut adjust_volume = BytesStart::new("adjust-volume");
                adjust_volume.push_attribute(("amount", linear_volume_to_fcpxml_db(clip.volume as f64).as_str()));
                if !clip.volume_keyframes.is_empty() {
                    writer.write_event(Event::Start(adjust_volume))?;
                    write_volume_keyframe_params(&mut writer, clip, &project.frame_rate)?;
                    writer.write_event(Event::End(BytesEnd::new("adjust-volume")))?;
                } else {
                    writer.write_event(Event::Empty(adjust_volume))?;
                }
            }

            let mut adjust_crop = BytesStart::new("adjust-crop");
            adjust_crop.push_attribute(("left", clip.crop_left.to_string().as_str()));
            adjust_crop.push_attribute(("right", clip.crop_right.to_string().as_str()));
            adjust_crop.push_attribute(("top", clip.crop_top.to_string().as_str()));
            adjust_crop.push_attribute(("bottom", clip.crop_bottom.to_string().as_str()));
            writer.write_event(Event::Empty(adjust_crop))?;
            for fragment in &clip.fcpxml_unknown_children {
                writer.get_mut().write_all(fragment.as_bytes())?;
            }

            writer.write_event(Event::End(BytesEnd::new("asset-clip")))?;
        }
    }

    for fragment in &project.fcpxml_unknown_spine.children {
        writer.get_mut().write_all(fragment.as_bytes())?;
    }
    writer.write_event(Event::End(BytesEnd::new("spine")))?;

    // Write markers as <marker> elements inside <sequence>
    for marker in &project.markers {
        let mut m = BytesStart::new("marker");
        m.push_attribute((
            "start",
            ns_to_fcpxml_time(marker.position_ns, &project.frame_rate).as_str(),
        ));
        m.push_attribute(("duration", "1/24s"));
        m.push_attribute(("value", marker.label.as_str()));
        m.push_attribute(("us:color", format!("{:08X}", marker.color).as_str()));
        writer.write_event(Event::Empty(m))?;
    }
    for fragment in &project.fcpxml_unknown_sequence.children {
        writer.get_mut().write_all(fragment.as_bytes())?;
    }

    writer.write_event(Event::End(BytesEnd::new("sequence")))?;
    for fragment in &project.fcpxml_unknown_project.children {
        writer.get_mut().write_all(fragment.as_bytes())?;
    }
    writer.write_event(Event::End(BytesEnd::new("project")))?;
    for fragment in &project.fcpxml_unknown_event.children {
        writer.get_mut().write_all(fragment.as_bytes())?;
    }
    writer.write_event(Event::End(BytesEnd::new("event")))?;
    for fragment in &project.fcpxml_unknown_library.children {
        writer.get_mut().write_all(fragment.as_bytes())?;
    }
    writer.write_event(Event::End(BytesEnd::new("library")))?;
    for fragment in &project.fcpxml_unknown_root.children {
        writer.get_mut().write_all(fragment.as_bytes())?;
    }
    writer.write_event(Event::End(BytesEnd::new("fcpxml")))?;

    let result = writer.into_inner().into_inner();
    Ok(String::from_utf8(result)?)
}

fn patch_imported_fcpxml_transform(project: &Project, original: &str) -> Option<String> {
    let clips: Vec<_> = project.tracks.iter().flat_map(|t| t.clips.iter()).collect();
    if clips.is_empty() {
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

    let keyframe_attrs: [(&str, Option<String>); 5] = [
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

fn write_resources(project: &Project, writer: &mut Writer<Cursor<Vec<u8>>>) -> Result<()> {
    let mut resources = BytesStart::new("resources");
    for (k, v) in &project.fcpxml_unknown_resources.attrs {
        if !is_writer_managed_resources_attr(k) {
            resources.push_attribute((k.as_str(), v.as_str()));
        }
    }
    writer.write_event(Event::Start(resources))?;

    // Format resource
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
            "{}/{}",
            project.frame_rate.denominator, project.frame_rate.numerator
        )
        .as_str(),
    ));
    fmt.push_attribute(("width", project.width.to_string().as_str()));
    fmt.push_attribute(("height", project.height.to_string().as_str()));
    for (k, v) in &project.fcpxml_unknown_format.attrs {
        if !is_writer_managed_format_attr(k) {
            fmt.push_attribute((k.as_str(), v.as_str()));
        }
    }
    if project.fcpxml_unknown_format.children.is_empty() {
        writer.write_event(Event::Empty(fmt))?;
    } else {
        writer.write_event(Event::Start(fmt))?;
        for fragment in &project.fcpxml_unknown_format.children {
            writer.get_mut().write_all(fragment.as_bytes())?;
        }
        writer.write_event(Event::End(BytesEnd::new("format")))?;
    }

    // Asset resources for each unique clip (correct hasVideo/hasAudio per clip kind)
    for track in project.video_tracks().chain(project.audio_tracks()) {
        for clip in &track.clips {
            let asset_id = format!("a_{}", sanitize_id(&clip.id));
            let export_source_path = clip
                .fcpxml_original_source_path
                .as_deref()
                .unwrap_or(&clip.source_path);
            let uri = crate::media::thumbnail::path_to_uri(export_source_path);
            let duration = ns_to_fcpxml_time(clip.source_out, &project.frame_rate);
            let has_video = if clip.kind == crate::model::clip::ClipKind::Audio {
                "0"
            } else {
                "1"
            };
            let has_audio = "1";

            let mut asset = BytesStart::new("asset");
            asset.push_attribute(("id", asset_id.as_str()));
            asset.push_attribute(("name", clip.label.as_str()));
            asset.push_attribute(("duration", duration.as_str()));
            asset.push_attribute(("hasVideo", has_video));
            asset.push_attribute(("hasAudio", has_audio));
            for (k, v) in &clip.fcpxml_unknown_asset_attrs {
                if !is_writer_managed_asset_attr(k) {
                    asset.push_attribute((k.as_str(), v.as_str()));
                }
            }
            writer.write_event(Event::Start(asset))?;

            let mut media_rep = BytesStart::new("media-rep");
            media_rep.push_attribute(("kind", media_rep_kind_for_path(export_source_path)));
            media_rep.push_attribute(("src", uri.as_str()));
            writer.write_event(Event::Empty(media_rep))?;
            for fragment in &clip.fcpxml_unknown_asset_children {
                writer.get_mut().write_all(fragment.as_bytes())?;
            }

            writer.write_event(Event::End(BytesEnd::new("asset")))?;
        }
    }
    for fragment in &project.fcpxml_unknown_resources.children {
        writer.get_mut().write_all(fragment.as_bytes())?;
    }

    writer.write_event(Event::End(BytesEnd::new("resources")))?;
    Ok(())
}

fn known_fcpxml_format_name(project: &Project) -> Option<&'static str> {
    match (
        project.width,
        project.height,
        project.frame_rate.numerator,
        project.frame_rate.denominator,
    ) {
        (1920, 1080, 24, 1) => Some("FFVideoFormat1080p24"),
        (1920, 1080, 30, 1) => Some("FFVideoFormat1080p30"),
        (3840, 2160, 24, 1) => Some("FFVideoFormat2160p24"),
        _ => None,
    }
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

/// Write native `<param>/<keyframeAnimation>/<keyframe>` children for transform properties.
fn write_transform_keyframe_params(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    clip: &crate::model::clip::Clip,
    project: &Project,
) -> Result<()> {
    let fps = &project.frame_rate;

    // Position keyframes
    if !clip.position_x_keyframes.is_empty() || !clip.position_y_keyframes.is_empty() {
        let mut param = BytesStart::new("param");
        param.push_attribute(("name", "position"));
        let (static_x, static_y) = internal_position_to_fcpxml(
            clip.position_x, clip.position_y,
            project.width, project.height, clip.scale,
        );
        param.push_attribute(("value", format!("{} {}", static_x, static_y).as_str()));
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
                &clip.scale_keyframes, t, clip.scale,
            );
            let ix = crate::model::clip::Clip::evaluate_keyframed_value(
                &clip.position_x_keyframes, t, clip.position_x,
            );
            let iy = crate::model::clip::Clip::evaluate_keyframed_value(
                &clip.position_y_keyframes, t, clip.position_y,
            );
            let (fx, fy) = internal_position_to_fcpxml(
                ix, iy, project.width, project.height, scale_at_t,
            );
            let mut kf_elem = BytesStart::new("keyframe");
            kf_elem.push_attribute(("time", ns_to_fcpxml_time(t, fps).as_str()));
            kf_elem.push_attribute(("value", format!("{} {}", fx, fy).as_str()));
            kf_elem.push_attribute(("interp", "linear"));
            writer.write_event(Event::Empty(kf_elem))?;
        }

        writer.write_event(Event::End(BytesEnd::new("keyframeAnimation")))?;
        writer.write_event(Event::End(BytesEnd::new("param")))?;
    }

    // Scale keyframes
    if !clip.scale_keyframes.is_empty() {
        let mut param = BytesStart::new("param");
        param.push_attribute(("name", "Scale"));
        param.push_attribute(("value", format!("{} {}", clip.scale, clip.scale).as_str()));
        writer.write_event(Event::Start(param))?;

        let kfa = BytesStart::new("keyframeAnimation");
        writer.write_event(Event::Start(kfa))?;

        let mut sorted: Vec<&crate::model::clip::NumericKeyframe> =
            clip.scale_keyframes.iter().collect();
        sorted.sort_by_key(|kf| kf.time_ns);

        for kf in &sorted {
            let mut kf_elem = BytesStart::new("keyframe");
            kf_elem.push_attribute(("time", ns_to_fcpxml_time(kf.time_ns, fps).as_str()));
            kf_elem.push_attribute(("value", format!("{} {}", kf.value, kf.value).as_str()));
            kf_elem.push_attribute(("interp", "linear"));
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
        kf_elem.push_attribute(("time", ns_to_fcpxml_time(kf.time_ns, fps).as_str()));
        kf_elem.push_attribute(("value", kf.value.to_string().as_str()));
        kf_elem.push_attribute(("interp", "linear"));
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
) -> Result<()> {
    if clip.volume_keyframes.is_empty() {
        return Ok(());
    }

    let mut param = BytesStart::new("param");
    param.push_attribute(("name", "amount"));
    param.push_attribute(("value", linear_volume_to_fcpxml_db(clip.volume as f64).as_str()));
    writer.write_event(Event::Start(param))?;

    let kfa = BytesStart::new("keyframeAnimation");
    writer.write_event(Event::Start(kfa))?;

    let mut sorted: Vec<&crate::model::clip::NumericKeyframe> =
        clip.volume_keyframes.iter().collect();
    sorted.sort_by_key(|kf| kf.time_ns);

    for kf in &sorted {
        let mut kf_elem = BytesStart::new("keyframe");
        kf_elem.push_attribute(("time", ns_to_fcpxml_time(kf.time_ns, fps).as_str()));
        kf_elem.push_attribute(("value", linear_volume_to_fcpxml_db(kf.value).as_str()));
        kf_elem.push_attribute(("interp", "linear"));
        writer.write_event(Event::Empty(kf_elem))?;
    }

    writer.write_event(Event::End(BytesEnd::new("keyframeAnimation")))?;
    writer.write_event(Event::End(BytesEnd::new("param")))?;

    Ok(())
}

/// Convert nanoseconds to FCPXML rational time string (e.g. "48048/24000s")
fn ns_to_fcpxml_time(ns: u64, fps: &crate::model::project::FrameRate) -> String {
    // FCPXML uses frame-accurate time: frames/timebase s
    let timebase = fps.numerator as u64;
    let denom = fps.denominator as u64;
    // frames = ns * fps_num / (fps_den * 1_000_000_000)
    let frames = ns * timebase / (denom * 1_000_000_000);
    format!("{frames}/{timebase}s")
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
    false
}

fn is_writer_managed_project_attr(key: &str) -> bool {
    matches!(key, "name")
}

fn is_writer_managed_sequence_attr(key: &str) -> bool {
    matches!(key, "duration" | "format")
}

fn is_writer_managed_spine_attr(_key: &str) -> bool {
    false
}

fn is_writer_managed_asset_attr(key: &str) -> bool {
    matches!(
        key,
        "id" | "src" | "name" | "duration" | "hasVideo" | "hasAudio"
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
            | "us:track-height"
            | "us:color-label"
            | "us:brightness"
            | "us:contrast"
            | "us:saturation"
            | "us:denoise"
            | "us:sharpness"
            | "us:volume"
            | "us:volume-keyframes"
            | "us:pan"
            | "us:crop-left"
            | "us:crop-right"
            | "us:crop-top"
            | "us:crop-bottom"
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
            | "us:lut-path"
            | "us:transition-after"
            | "us:transition-after-ns"
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
    fn test_write_fcpxml_dirty_scale_edit_preserves_unknown_fields() {
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
            },
            crate::model::clip::NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 1.75,
                interpolation: crate::model::clip::KeyframeInterpolation::Linear,
            },
        ];
        clip.opacity_keyframes = vec![crate::model::clip::NumericKeyframe {
            time_ns: 250_000_000,
            value: 0.5,
            interpolation: crate::model::clip::KeyframeInterpolation::Linear,
        }];
        clip.position_x_keyframes = vec![crate::model::clip::NumericKeyframe {
            time_ns: 500_000_000,
            value: 0.25,
            interpolation: crate::model::clip::KeyframeInterpolation::Linear,
        }];
        clip.position_y_keyframes = vec![crate::model::clip::NumericKeyframe {
            time_ns: 500_000_000,
            value: -0.5,
            interpolation: crate::model::clip::KeyframeInterpolation::Linear,
        }];
        clip.volume_keyframes = vec![crate::model::clip::NumericKeyframe {
            time_ns: 0,
            value: 0.8,
            interpolation: crate::model::clip::KeyframeInterpolation::Linear,
        }];
        project.dirty = true;

        let written = write_fcpxml(&project).expect("write should succeed");
        assert!(written.contains("customRoot=\"keep-root\""));
        assert!(written.contains("customAsset=\"keep-asset\""));
        assert!(written.contains("customSequence=\"keep-seq\""));
        assert!(written.contains("customClip=\"keep-clip\""));
        assert!(written.contains("<metadata key=\"com.example.unknown\" value=\"keep-meta\""));
        assert!(written.contains("us:scale=\"1.75\""));
        assert!(written.contains("us:position-x=\"0.25\""));
        assert!(written.contains("us:scale-keyframes="));
        assert!(written.contains("us:opacity-keyframes="));
        assert!(written.contains("us:position-x-keyframes="));
        assert!(written.contains("us:position-y-keyframes="));
        assert!(written.contains("us:volume-keyframes="));
        assert!(written.contains("us:position-y=\"-0.5\""));
        assert!(written.contains("adjust-transform position=\"-16.666666"));
        assert!(written.contains(" scale=\"1.75 1.75\" rotation=\"0\""));
        assert!(written.contains(" -18.75\""));
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
    fn test_write_fcpxml_dirty_transform_edit_prefers_in_place_patch_for_multi_clip_import() {
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
        assert!(written.contains("<asset id=\"r1v\""));
        assert!(written.contains("<asset id=\"r2v\""));
        assert!(written.contains("<smart-collection name=\"Projects\""));
        assert!(written.contains("scale=\"0.75 0.75\""));
        assert!(written.contains("rotation=\"37\""));
        assert!(!written.contains("us:track-idx="));
        assert!(!written.contains("<asset id=\"a_"));
        assert!(!written.contains("ref=\"a_"));
    }

    #[test]
    fn test_write_fcpxml_uses_known_format_name_for_1080p24() {
        let project = Project::new("KnownFormatName");
        let xml = write_fcpxml(&project).expect("write should succeed");
        assert!(
            xml.contains(
                "<format id=\"r1\" name=\"FFVideoFormat1080p24\" frameDuration=\"1/24\" width=\"1920\" height=\"1080\"/>"
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
        project.width = 1280;
        project.height = 720;
        project.frame_rate.numerator = 30000;
        project.frame_rate.denominator = 1001;

        let xml = write_fcpxml(&project).expect("write should succeed");
        assert!(
            xml.contains(
                "<format id=\"r1\" frameDuration=\"1001/30000\" width=\"1280\" height=\"720\"/>"
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
        use crate::model::clip::{Clip, ClipKind, NumericKeyframe, KeyframeInterpolation};
        use crate::model::track::Track;

        let mut project = Project::new("NativeKF");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/clip.mp4", 5_000_000_000, 0, ClipKind::Video);
        clip.opacity_keyframes = vec![
            NumericKeyframe { time_ns: 0, value: 1.0, interpolation: KeyframeInterpolation::Linear },
            NumericKeyframe { time_ns: 2_000_000_000, value: 0.0, interpolation: KeyframeInterpolation::Linear },
        ];
        clip.scale_keyframes = vec![
            NumericKeyframe { time_ns: 0, value: 0.5, interpolation: KeyframeInterpolation::Linear },
            NumericKeyframe { time_ns: 5_000_000_000, value: 1.5, interpolation: KeyframeInterpolation::Linear },
        ];
        clip.volume_keyframes = vec![
            NumericKeyframe { time_ns: 0, value: 1.0, interpolation: KeyframeInterpolation::Linear },
            NumericKeyframe { time_ns: 3_000_000_000, value: 0.0, interpolation: KeyframeInterpolation::Linear },
        ];
        track.add_clip(clip);
        project.tracks.push(track);

        let xml = write_fcpxml(&project).expect("write should succeed");

        // Should contain native <adjust-transform> with param/keyframeAnimation/keyframe children
        assert!(xml.contains("<adjust-transform"), "missing adjust-transform");
        assert!(xml.contains("<param name=\"Scale\""), "missing Scale param");
        assert!(xml.contains("<keyframeAnimation"), "missing keyframeAnimation");
        assert!(xml.contains("<keyframe "), "missing keyframe element");
        assert!(xml.contains("interp=\"linear\""), "missing interp attribute");

        // Should contain native <adjust-compositing> with opacity keyframes
        assert!(xml.contains("<adjust-compositing"), "missing adjust-compositing");
        assert!(xml.contains("<param name=\"amount\""), "missing amount param for opacity");

        // Should contain <adjust-volume> with volume keyframes
        assert!(xml.contains("<adjust-volume"), "missing adjust-volume");

        // Should also still have vendor attrs for lossless round-trip
        assert!(xml.contains("us:scale-keyframes="), "missing vendor scale keyframes");
        assert!(xml.contains("us:opacity-keyframes="), "missing vendor opacity keyframes");
        assert!(xml.contains("us:volume-keyframes="), "missing vendor volume keyframes");
    }

    #[test]
    fn test_write_read_native_keyframe_round_trip() {
        use crate::model::clip::{Clip, ClipKind, NumericKeyframe, KeyframeInterpolation};
        use crate::model::track::Track;

        let mut project = Project::new("RoundTrip");
        project.tracks.clear();
        let mut track = Track::new_video("Video 1");
        let mut clip = Clip::new("/tmp/clip.mp4", 5_000_000_000, 0, ClipKind::Video);
        clip.opacity_keyframes = vec![
            NumericKeyframe { time_ns: 0, value: 1.0, interpolation: KeyframeInterpolation::Linear },
            NumericKeyframe { time_ns: 2_000_000_000, value: 0.3, interpolation: KeyframeInterpolation::Linear },
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
    }
}
