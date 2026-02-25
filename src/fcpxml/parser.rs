use quick_xml::Reader;
use quick_xml::events::Event;
use anyhow::Result;
use crate::model::project::{Project, FrameRate};
use crate::model::track::Track;
use crate::model::clip::{Clip, ClipKind};
use std::collections::HashMap;

/// Represents a parsed FCPXML asset
struct Asset {
    id: String,
    src: String,
    name: String,
    duration_ns: u64,
}

/// Parse an FCPXML string into a `Project`.
pub fn parse_fcpxml(xml: &str) -> Result<Project> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut assets: HashMap<String, Asset> = HashMap::new();
    let mut project = Project::new("Imported Project");
    // Clear default tracks — we'll add them from the FCPXML
    project.tracks.clear();

    let mut primary_track: Option<Track> = None;
    let mut buf = Vec::new();
    let mut in_spine = false;
    let mut _project_name: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let name_local = e.local_name();
                let name = std::str::from_utf8(name_local.as_ref())?.to_string();
                let attrs = parse_attrs(e)?;

                match name.as_str() {
                    "project" => {
                        if let Some(n) = attrs.get("name") {
                            project.title = n.clone();
                        }
                    }
                    "format" => {
                        if let (Some(w), Some(h)) = (attrs.get("width"), attrs.get("height")) {
                            project.width = w.parse().unwrap_or(1920);
                            project.height = h.parse().unwrap_or(1080);
                        }
                        if let Some(fd) = attrs.get("frameDuration") {
                            project.frame_rate = parse_frame_duration(fd);
                        }
                    }
                    "asset" => {
                        if let (Some(id), Some(src)) = (attrs.get("id"), attrs.get("src")) {
                            let duration_ns = attrs.get("duration")
                                .and_then(|d| parse_fcpxml_time(d))
                                .unwrap_or(0);
                            assets.insert(id.clone(), Asset {
                                id: id.clone(),
                                src: src.replace("file://", ""),
                                name: attrs.get("name").cloned().unwrap_or_default(),
                                duration_ns,
                            });
                        }
                    }
                    "spine" => {
                        in_spine = true;
                        if primary_track.is_none() {
                            primary_track = Some(Track::new_video("Video 1"));
                        }
                    }
                    "asset-clip" if in_spine => {
                        if let Some(track) = primary_track.as_mut() {
                            if let Some(asset_ref) = attrs.get("ref") {
                                if let Some(asset) = assets.get(asset_ref) {
                                    let timeline_start = attrs.get("offset")
                                        .and_then(|t| parse_fcpxml_time(t))
                                        .unwrap_or(0);
                                    let source_in = attrs.get("start")
                                        .and_then(|t| parse_fcpxml_time(t))
                                        .unwrap_or(0);
                                    let duration = attrs.get("duration")
                                        .and_then(|t| parse_fcpxml_time(t))
                                        .unwrap_or(asset.duration_ns);
                                    let label = attrs.get("name")
                                        .cloned()
                                        .unwrap_or_else(|| asset.name.clone());

                                    let mut clip = Clip::new(
                                        &asset.src,
                                        source_in + duration,
                                        timeline_start,
                                        ClipKind::Video,
                                    );
                                    clip.source_in = source_in;
                                    clip.source_out = source_in + duration;
                                    clip.timeline_start = timeline_start;
                                    clip.label = label;
                                    // Restore color/effects from vendor attributes
                                    if let Some(v) = attrs.get("us:brightness") { clip.brightness = v.parse().unwrap_or(0.0); }
                                    if let Some(v) = attrs.get("us:contrast")   { clip.contrast   = v.parse().unwrap_or(1.0); }
                                    if let Some(v) = attrs.get("us:saturation") { clip.saturation = v.parse().unwrap_or(1.0); }
                                    if let Some(v) = attrs.get("us:denoise")    { clip.denoise    = v.parse().unwrap_or(0.0); }
                                    if let Some(v) = attrs.get("us:sharpness")  { clip.sharpness  = v.parse().unwrap_or(0.0); }
                                    if let Some(v) = attrs.get("us:volume")     { clip.volume     = v.parse().unwrap_or(1.0); }
                                    if let Some(v) = attrs.get("us:pan")        { clip.pan        = v.parse().unwrap_or(0.0); }
                                    track.add_clip(clip);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::End(ref e) => {
                let name_local = e.local_name();
                let name = std::str::from_utf8(name_local.as_ref())?;
                if name == "spine" {
                    in_spine = false;
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    if let Some(track) = primary_track {
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

fn parse_attrs(e: &quick_xml::events::BytesStart) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for attr in e.attributes() {
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
        if den == 0 { return None; }
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
        FrameRate { numerator: den, denominator: num }
    } else {
        FrameRate::fps_24()
    }
}
