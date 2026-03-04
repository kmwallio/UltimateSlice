use crate::model::project::Project;
use anyhow::Result;
use quick_xml::events::{BytesEnd, BytesStart, Event};
use quick_xml::Writer;
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

    // <fcpxml version="1.14">
    let mut fcpxml = BytesStart::new("fcpxml");
    fcpxml.push_attribute(("version", crate::fcpxml::FCPXML_EXPORT_VERSION));
    fcpxml.push_attribute(("xmlns:us", "urn:ultimateslice"));
    writer.write_event(Event::Start(fcpxml))?;

    // <resources>
    write_resources(project, &mut writer)?;

    // <library>
    writer.write_event(Event::Start(BytesStart::new("library")))?;
    writer.write_event(Event::Start(BytesStart::new("event")))?;

    // <project name="...">
    let mut proj_elem = BytesStart::new("project");
    proj_elem.push_attribute(("name", project.title.as_str()));
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
    writer.write_event(Event::Start(seq))?;

    // <spine>
    writer.write_event(Event::Start(BytesStart::new("spine")))?;

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
            let start = ns_to_fcpxml_time(clip.source_in, &project.frame_rate);

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
            // Store color/effects as custom vendor attributes (us: prefix).
            // Final Cut Pro ignores unknown attributes, so round-trip is lossless.
            asset_clip.push_attribute(("us:brightness", clip.brightness.to_string().as_str()));
            asset_clip.push_attribute(("us:contrast", clip.contrast.to_string().as_str()));
            asset_clip.push_attribute(("us:saturation", clip.saturation.to_string().as_str()));
            asset_clip.push_attribute(("us:denoise", clip.denoise.to_string().as_str()));
            asset_clip.push_attribute(("us:sharpness", clip.sharpness.to_string().as_str()));
            asset_clip.push_attribute(("us:volume", clip.volume.to_string().as_str()));
            asset_clip.push_attribute(("us:pan", clip.pan.to_string().as_str()));
            asset_clip.push_attribute(("us:crop-left", clip.crop_left.to_string().as_str()));
            asset_clip.push_attribute(("us:crop-right", clip.crop_right.to_string().as_str()));
            asset_clip.push_attribute(("us:crop-top", clip.crop_top.to_string().as_str()));
            asset_clip.push_attribute(("us:crop-bottom", clip.crop_bottom.to_string().as_str()));
            asset_clip.push_attribute(("us:rotate", clip.rotate.to_string().as_str()));
            asset_clip.push_attribute(("us:flip-h", clip.flip_h.to_string().as_str()));
            asset_clip.push_attribute(("us:flip-v", clip.flip_v.to_string().as_str()));
            asset_clip.push_attribute(("us:scale", clip.scale.to_string().as_str()));
            asset_clip.push_attribute(("us:opacity", clip.opacity.to_string().as_str()));
            asset_clip.push_attribute(("us:position-x", clip.position_x.to_string().as_str()));
            asset_clip.push_attribute(("us:position-y", clip.position_y.to_string().as_str()));
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
            asset_clip.push_attribute(("us:shadows", clip.shadows.to_string().as_str()));
            asset_clip.push_attribute(("us:midtones", clip.midtones.to_string().as_str()));
            asset_clip.push_attribute(("us:highlights", clip.highlights.to_string().as_str()));
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
            adjust_transform.push_attribute((
                "scale",
                format!("{} {}", clip.scale, clip.scale).as_str(),
            ));
            adjust_transform.push_attribute(("rotation", clip.rotate.to_string().as_str()));
            writer.write_event(Event::Empty(adjust_transform))?;

            let mut adjust_compositing = BytesStart::new("adjust-compositing");
            adjust_compositing.push_attribute(("opacity", clip.opacity.to_string().as_str()));
            writer.write_event(Event::Empty(adjust_compositing))?;

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

    writer.write_event(Event::End(BytesEnd::new("sequence")))?;
    writer.write_event(Event::End(BytesEnd::new("project")))?;
    writer.write_event(Event::End(BytesEnd::new("event")))?;
    writer.write_event(Event::End(BytesEnd::new("library")))?;
    writer.write_event(Event::End(BytesEnd::new("fcpxml")))?;

    let result = writer.into_inner().into_inner();
    Ok(String::from_utf8(result)?)
}

fn patch_imported_fcpxml_transform(project: &Project, original: &str) -> Option<String> {
    let mut clips = project.tracks.iter().flat_map(|t| t.clips.iter());
    let clip = clips.next()?;
    if clips.next().is_some() {
        return None;
    }

    let mut xml =
        replace_attr_in_first_tag(original, "asset-clip", "us:scale", &clip.scale.to_string())?;
    xml = replace_attr_in_first_tag(&xml, "asset-clip", "us:position-x", &clip.position_x.to_string())?;
    xml = replace_attr_in_first_tag(&xml, "asset-clip", "us:position-y", &clip.position_y.to_string())?;
    let transform_scale = format!("{} {}", clip.scale, clip.scale);
    if let Some(updated) = replace_attr_in_first_tag(&xml, "adjust-transform", "scale", &transform_scale) {
        xml = updated;
    }
    let (position_x, position_y) =
        internal_position_to_fcpxml(
            clip.position_x,
            clip.position_y,
            project.width,
            project.height,
            clip.scale,
        );
    let transform_position = format!("{} {}", position_x, position_y);
    if let Some(updated) =
        replace_attr_in_first_tag(&xml, "adjust-transform", "position", &transform_position)
    {
        xml = updated;
    }
    Some(xml)
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
    let (shift_x_px, shift_y_px) = if range_x.abs() < f64::EPSILON || range_y.abs() < f64::EPSILON
    {
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

fn replace_attr_in_first_tag(xml: &str, tag_name: &str, attr_name: &str, new_value: &str) -> Option<String> {
    let tag_open = format!("<{tag_name}");
    let tag_start = xml.find(&tag_open)?;
    let after_start = &xml[tag_start..];
    let tag_end_rel = after_start.find('>')?;
    let tag_end = tag_start + tag_end_rel;
    let tag_text = &xml[tag_start..=tag_end];
    let updated_tag = replace_or_insert_attr(tag_text, attr_name, new_value)?;
    let mut out = String::with_capacity(xml.len() + updated_tag.len().saturating_sub(tag_text.len()));
    out.push_str(&xml[..tag_start]);
    out.push_str(&updated_tag);
    out.push_str(&xml[tag_end + 1..]);
    Some(out)
}

fn replace_or_insert_attr(tag_text: &str, attr_name: &str, new_value: &str) -> Option<String> {
    let attr_prefix = format!(r#"{attr_name}=""#);
    if let Some(attr_start) = tag_text.find(&attr_prefix) {
        let value_start = attr_start + attr_prefix.len();
        let value_end_rel = tag_text[value_start..].find('"')?;
        let value_end = value_start + value_end_rel;
        let mut updated = String::with_capacity(
            tag_text.len() + new_value.len().saturating_sub(value_end.saturating_sub(value_start)),
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

fn write_resources(project: &Project, writer: &mut Writer<Cursor<Vec<u8>>>) -> Result<()> {
    writer.write_event(Event::Start(BytesStart::new("resources")))?;

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
    writer.write_event(Event::Empty(fmt))?;

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
            writer.write_event(Event::Start(asset))?;

            let mut media_rep = BytesStart::new("media-rep");
            media_rep.push_attribute(("kind", media_rep_kind_for_path(export_source_path)));
            media_rep.push_attribute(("src", uri.as_str()));
            writer.write_event(Event::Empty(media_rep))?;

            writer.write_event(Event::End(BytesEnd::new("asset")))?;
        }
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
            | "us:brightness"
            | "us:contrast"
            | "us:saturation"
            | "us:denoise"
            | "us:sharpness"
            | "us:volume"
            | "us:pan"
            | "us:crop-left"
            | "us:crop-right"
            | "us:crop-top"
            | "us:crop-bottom"
            | "us:rotate"
            | "us:flip-h"
            | "us:flip-v"
            | "us:scale"
            | "us:opacity"
            | "us:position-x"
            | "us:position-y"
            | "us:title-text"
            | "us:title-font"
            | "us:title-color"
            | "us:title-x"
            | "us:title-y"
            | "us:speed"
            | "us:reverse"
            | "us:shadows"
            | "us:midtones"
            | "us:highlights"
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
        project.dirty = true;

        let written = write_fcpxml(&project).expect("write should succeed");
        assert!(written.contains("customRoot=\"keep-root\""));
        assert!(written.contains("customAsset=\"keep-asset\""));
        assert!(written.contains("customSequence=\"keep-seq\""));
        assert!(written.contains("customClip=\"keep-clip\""));
        assert!(written.contains("<metadata key=\"com.example.unknown\" value=\"keep-meta\""));
        assert!(written.contains("us:scale=\"1.75\""));
        assert!(written.contains("us:position-x=\"0.25\""));
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
        assert!(written.contains("<customTag mode=\"hold\"><childTag value=\"keep-child\"/></customTag>"));
        assert!(written.contains("us:scale=\"2.25\""));
        assert!(written.contains("adjust-transform"));
        assert!(written.contains("scale=\"2.25 2.25\""));
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
    fn test_write_fcpxml_omits_unknown_format_name_for_nonstandard_preset() {
        let mut project = Project::new("UnknownFormatName");
        project.width = 1280;
        project.height = 720;
        project.frame_rate.numerator = 30000;
        project.frame_rate.denominator = 1001;

        let xml = write_fcpxml(&project).expect("write should succeed");
        assert!(
            xml.contains("<format id=\"r1\" frameDuration=\"1001/30000\" width=\"1280\" height=\"720\"/>"),
            "expected export to keep numeric format data for non-standard preset:\n{xml}"
        );
        assert!(
            !xml.contains("name=\"FFVideoFormat"),
            "expected export to omit unsupported hardcoded format names:\n{xml}"
        );
    }
}
