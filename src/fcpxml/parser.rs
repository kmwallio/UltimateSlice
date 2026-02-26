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

    // track_idx → Track (built during parsing, indexed by us:track-idx)
    let mut track_map: std::collections::BTreeMap<usize, Track> = std::collections::BTreeMap::new();
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
                    }
                    "asset-clip" if in_spine => {
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

                                // Determine track from us:track-idx / us:track-kind / us:track-name.
                                // Fall back to track 0 (Video 1) for legacy FCPXML without these attrs.
                                let track_idx: usize = attrs.get("us:track-idx")
                                    .and_then(|s| s.parse().ok())
                                    .unwrap_or(0);
                                let track_kind_str = attrs.get("us:track-kind")
                                    .map(|s| s.as_str())
                                    .unwrap_or("video");
                                let track_name = attrs.get("us:track-name")
                                    .cloned()
                                    .unwrap_or_else(|| {
                                        if track_kind_str == "audio" {
                                            format!("Audio {}", track_idx + 1)
                                        } else {
                                            format!("Video {}", track_idx + 1)
                                        }
                                    });

                                let clip_kind = if track_kind_str == "audio" { ClipKind::Audio } else { ClipKind::Video };

                                // Get or create the target track
                                let track = track_map.entry(track_idx).or_insert_with(|| {
                                    if clip_kind == ClipKind::Audio {
                                        Track::new_audio(&track_name)
                                    } else {
                                        Track::new_video(&track_name)
                                    }
                                });

                                let mut clip = Clip::new(
                                    &asset.src,
                                    source_in + duration,
                                    timeline_start,
                                    clip_kind,
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
                                if let Some(v) = attrs.get("us:crop-left")  { clip.crop_left  = v.parse().unwrap_or(0); }
                                if let Some(v) = attrs.get("us:crop-right") { clip.crop_right = v.parse().unwrap_or(0); }
                                if let Some(v) = attrs.get("us:crop-top")   { clip.crop_top   = v.parse().unwrap_or(0); }
                                if let Some(v) = attrs.get("us:crop-bottom"){ clip.crop_bottom= v.parse().unwrap_or(0); }
                                if let Some(v) = attrs.get("us:rotate")     { clip.rotate     = v.parse().unwrap_or(0); }
                                if let Some(v) = attrs.get("us:flip-h")     { clip.flip_h     = v.parse().unwrap_or(false); }
                                if let Some(v) = attrs.get("us:flip-v")     { clip.flip_v     = v.parse().unwrap_or(false); }
                                if let Some(v) = attrs.get("us:title-text") { clip.title_text = v.clone(); }
                                if let Some(v) = attrs.get("us:title-font") { clip.title_font = v.clone(); }
                                if let Some(v) = attrs.get("us:title-color"){ clip.title_color = u32::from_str_radix(v, 16).unwrap_or(0xFFFFFFFF); }
                                if let Some(v) = attrs.get("us:title-x")    { clip.title_x    = v.parse().unwrap_or(0.5); }
                                if let Some(v) = attrs.get("us:title-y")    { clip.title_y    = v.parse().unwrap_or(0.9); }
                                if let Some(v) = attrs.get("us:speed")      { clip.speed      = v.parse().unwrap_or(1.0); }
                                if let Some(v) = attrs.get("us:lut-path")  { clip.lut_path   = Some(v.clone()); }
                                track.add_clip(clip);
                            }
                        }
                    }
                    "marker" => {
                        // Restore timeline markers
                        if let Some(start_str) = attrs.get("start") {
                            if let Some(pos_ns) = parse_fcpxml_time(start_str) {
                                let label = attrs.get("value").cloned().unwrap_or_default();
                                let color = attrs.get("us:color")
                                    .and_then(|s| u32::from_str_radix(s, 16).ok())
                                    .unwrap_or(0xFF8C00FF);
                                use crate::model::project::Marker;
                                let mut m = Marker::new(pos_ns, label);
                                m.color = color;
                                project.markers.push(m);
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

    // Add tracks in index order
    for (_idx, track) in track_map {
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
