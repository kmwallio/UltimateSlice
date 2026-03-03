use crate::model::clip::{Clip, ClipKind};
use crate::model::project::{FrameRate, Project};
use crate::model::track::Track;
use anyhow::Result;
use quick_xml::events::Event;
use quick_xml::Reader;
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
                let name = std::str::from_utf8(name_local.as_ref())?;

                match name {
                    "project" => {
                        let attrs = parse_attrs(e)?;
                        if let Some(n) = attrs.get("name") {
                            project.title = n.clone();
                        }
                    }
                    "format" => {
                        let attrs = parse_attrs(e)?;
                        if let (Some(w), Some(h)) = (attrs.get("width"), attrs.get("height")) {
                            project.width = w.parse().unwrap_or(1920);
                            project.height = h.parse().unwrap_or(1080);
                        }
                        if let Some(fd) = attrs.get("frameDuration") {
                            project.frame_rate = parse_frame_duration(fd);
                        }
                    }
                    "asset" => {
                        let attrs = parse_attrs(e)?;
                        if let (Some(id), Some(src)) = (attrs.get("id"), attrs.get("src")) {
                            let duration_ns = attrs
                                .get("duration")
                                .and_then(|d| parse_fcpxml_time(d))
                                .unwrap_or(0);
                            assets.insert(
                                id.clone(),
                                Asset {
                                    id: id.clone(),
                                    src: src.replace("file://", ""),
                                    name: attrs.get("name").cloned().unwrap_or_default(),
                                    duration_ns,
                                },
                            );
                        }
                    }
                    "spine" => {
                        in_spine = true;
                    }
                    "asset-clip" if in_spine => {
                        let attrs = parse_attrs(e)?;
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

                                // Determine track from us:track-idx / us:track-kind / us:track-name.
                                // Fall back to track 0 (Video 1) for legacy FCPXML without these attrs.
                                let track_idx: usize = attrs
                                    .get("us:track-idx")
                                    .and_then(|s| s.parse().ok())
                                    .unwrap_or(0);
                                let track_kind_str = attrs
                                    .get("us:track-kind")
                                    .map(|s| s.as_str())
                                    .unwrap_or("video");
                                let track_name =
                                    attrs.get("us:track-name").cloned().unwrap_or_else(|| {
                                        if track_kind_str == "audio" {
                                            format!("Audio {}", track_idx + 1)
                                        } else {
                                            format!("Video {}", track_idx + 1)
                                        }
                                    });

                                let clip_kind = if track_kind_str == "audio" {
                                    ClipKind::Audio
                                } else {
                                    ClipKind::Video
                                };

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
                                    clip.title_color =
                                        u32::from_str_radix(v, 16).unwrap_or(0xFFFFFFFF);
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
                                track.push_unsorted(clip);
                            }
                        }
                    }
                    "marker" => {
                        let attrs = parse_attrs(e)?;
                        // Restore timeline markers
                        if let Some(start_str) = attrs.get("start") {
                            if let Some(pos_ns) = parse_fcpxml_time(start_str) {
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

    // Add tracks in index order, sorting clips once per track
    for (_idx, mut track) in track_map {
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
}
