use quick_xml::events::{BytesEnd, BytesStart, Event};
use quick_xml::Writer;
use std::io::Cursor;
use anyhow::Result;
use crate::model::project::Project;

/// Serialize a `Project` to FCPXML 1.10 format.
pub fn write_fcpxml(project: &Project) -> Result<String> {
    let mut writer = Writer::new_with_indent(Cursor::new(Vec::new()), b' ', 4);

    // XML declaration
    writer.write_event(Event::Decl(quick_xml::events::BytesDecl::new("1.0", Some("UTF-8"), None)))?;

    // <fcpxml version="1.10">
    let mut fcpxml = BytesStart::new("fcpxml");
    fcpxml.push_attribute(("version", "1.10"));
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
    let _fps = format!("{}/{}", project.frame_rate.numerator, project.frame_rate.denominator);
    let duration_str = ns_to_fcpxml_time(project.duration(), &project.frame_rate);
    let format_ref = "r1";

    let mut seq = BytesStart::new("sequence");
    seq.push_attribute(("duration", duration_str.as_str()));
    seq.push_attribute(("format", format_ref));
    writer.write_event(Event::Start(seq))?;

    // <spine>
    writer.write_event(Event::Start(BytesStart::new("spine")))?;

    // Emit clips from the primary video track
    let video_tracks: Vec<_> = project.video_tracks().collect();
    if let Some(track) = video_tracks.first() {
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
            // Store color/effects as custom vendor attributes (us: prefix).
            // Final Cut Pro ignores unknown attributes, so round-trip is lossless.
            asset_clip.push_attribute(("us:brightness", clip.brightness.to_string().as_str()));
            asset_clip.push_attribute(("us:contrast",   clip.contrast.to_string().as_str()));
            asset_clip.push_attribute(("us:saturation", clip.saturation.to_string().as_str()));
            asset_clip.push_attribute(("us:denoise",    clip.denoise.to_string().as_str()));
            asset_clip.push_attribute(("us:sharpness",  clip.sharpness.to_string().as_str()));
            writer.write_event(Event::Empty(asset_clip))?;
        }
    }

    writer.write_event(Event::End(BytesEnd::new("spine")))?;
    writer.write_event(Event::End(BytesEnd::new("sequence")))?;
    writer.write_event(Event::End(BytesEnd::new("project")))?;
    writer.write_event(Event::End(BytesEnd::new("event")))?;
    writer.write_event(Event::End(BytesEnd::new("library")))?;
    writer.write_event(Event::End(BytesEnd::new("fcpxml")))?;

    let result = writer.into_inner().into_inner();
    Ok(String::from_utf8(result)?)
}

fn write_resources(project: &Project, writer: &mut Writer<Cursor<Vec<u8>>>) -> Result<()> {
    writer.write_event(Event::Start(BytesStart::new("resources")))?;

    // Format resource
    let _fps = format!("{}/{}", project.frame_rate.numerator, project.frame_rate.denominator);
    let mut fmt = BytesStart::new("format");
    fmt.push_attribute(("id", "r1"));
    fmt.push_attribute(("name", "FFVideoFormat1080p24"));
    fmt.push_attribute(("frameDuration", format!("{}/{}", project.frame_rate.denominator, project.frame_rate.numerator).as_str()));
    fmt.push_attribute(("width", project.width.to_string().as_str()));
    fmt.push_attribute(("height", project.height.to_string().as_str()));
    writer.write_event(Event::Empty(fmt))?;

    // Asset resources for each unique clip
    for track in project.video_tracks().chain(project.audio_tracks()) {
        for clip in &track.clips {
            let asset_id = format!("a_{}", sanitize_id(&clip.id));
            let uri = crate::media::thumbnail::path_to_uri(&clip.source_path);
            let duration = ns_to_fcpxml_time(clip.source_out, &project.frame_rate);

            let mut asset = BytesStart::new("asset");
            asset.push_attribute(("id", asset_id.as_str()));
            asset.push_attribute(("name", clip.label.as_str()));
            asset.push_attribute(("src", uri.as_str()));
            asset.push_attribute(("duration", duration.as_str()));
            asset.push_attribute(("hasVideo", "1"));
            asset.push_attribute(("hasAudio", "1"));
            writer.write_event(Event::Empty(asset))?;
        }
    }

    writer.write_event(Event::End(BytesEnd::new("resources")))?;
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
