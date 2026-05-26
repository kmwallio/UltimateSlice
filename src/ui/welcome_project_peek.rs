//! Lightweight "peek" helper for the welcome screen.
//!
//! Renders a thumbnail card per recent project by extracting just the
//! first asset's media source path from the saved `.uspxml` / `.fcpxml`
//! file — without doing a full project load. A full load walks every
//! track, clip, keyframe, motion-tracking sample, and effect; on a
//! big project that's hundreds of milliseconds, and we'd do it ten
//! times in a row for a Welcome-screen render. Streaming through
//! `quick_xml` and stopping at the first `<media-rep src=...>` (or
//! first `<asset src=...>` for the older inline-src layout) takes
//! ~1 ms even on large files.

use quick_xml::events::Event;
use quick_xml::Reader;
use std::path::Path;

/// Read just enough of a saved project file to extract the OS path of
/// the first asset's media source.
///
/// Returns `None` when:
/// * the file can't be read
/// * the XML is malformed
/// * no `<asset>` element has a recognizable `src` (asset-less project,
///   or a format we don't recognize)
///
/// Path decoding (URI unescape, `file://` strip) is delegated to
/// [`crate::fcpxml::parser::parse_fcpxml_src_path`] so this module
/// stays exactly in sync with how the real loader resolves source
/// paths — promoting the helper to `pub(crate)` keeps the two in
/// lockstep without copying the percent-decode logic.
pub fn peek_first_asset_source(project_path: &Path) -> Option<String> {
    let xml = std::fs::read_to_string(project_path).ok()?;
    peek_first_asset_source_from_str(&xml)
}

/// String-input variant for unit testing without touching the disk.
pub fn peek_first_asset_source_from_str(xml: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut inside_asset = false;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name_bytes = e.name();
                let name = std::str::from_utf8(name_bytes.as_ref()).unwrap_or("");
                if name == "asset" {
                    inside_asset = true;
                    // Older FCPXML layout puts the src directly on the
                    // asset element. Try that first; fall through to
                    // the nested media-rep branch if not present.
                    if let Some(src) = read_src_attribute(&e) {
                        return Some(crate::fcpxml::parser::parse_fcpxml_src_path(&src));
                    }
                } else if name == "media-rep" && inside_asset {
                    if let Some(src) = read_src_attribute(&e) {
                        return Some(crate::fcpxml::parser::parse_fcpxml_src_path(&src));
                    }
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == b"asset" {
                    inside_asset = false;
                }
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

fn read_src_attribute(e: &quick_xml::events::BytesStart) -> Option<String> {
    for attr in e.attributes().flatten() {
        if attr.key.as_ref() == b"src" {
            if let Ok(val) = std::str::from_utf8(&attr.value) {
                return Some(val.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_nested_media_rep_src() {
        let xml = r#"<?xml version="1.0"?>
<fcpxml version="1.14">
  <resources>
    <asset id="a1" name="clip" hasVideo="1">
      <media-rep kind="original-media" src="file:///home/user/video.mp4"/>
    </asset>
  </resources>
</fcpxml>"#;
        assert_eq!(
            peek_first_asset_source_from_str(xml),
            Some("/home/user/video.mp4".to_string())
        );
    }

    #[test]
    fn extracts_inline_src_on_asset_for_older_fcpxml() {
        let xml = r#"<?xml version="1.0"?>
<fcpxml>
  <resources>
    <asset id="a1" src="file:///tmp/old.mov" name="old"/>
  </resources>
</fcpxml>"#;
        assert_eq!(
            peek_first_asset_source_from_str(xml),
            Some("/tmp/old.mov".to_string())
        );
    }

    #[test]
    fn decodes_percent_encoded_path() {
        let xml = r#"<?xml version="1.0"?>
<fcpxml>
  <resources>
    <asset id="a1">
      <media-rep src="file:///home/user/My%20Videos/clip%201.mp4"/>
    </asset>
  </resources>
</fcpxml>"#;
        assert_eq!(
            peek_first_asset_source_from_str(xml),
            Some("/home/user/My Videos/clip 1.mp4".to_string())
        );
    }

    #[test]
    fn returns_first_asset_only_ignoring_later_ones() {
        // Welcome card shows ONE thumbnail per project — the first asset
        // wins. Don't try to be smart and prefer the "best looking" one
        // (e.g. the first hasVideo=1 asset) because that pulls in scoring
        // logic the user can't influence. First-wins is predictable.
        let xml = r#"<?xml version="1.0"?>
<fcpxml>
  <resources>
    <asset id="a1"><media-rep src="file:///first.mp4"/></asset>
    <asset id="a2"><media-rep src="file:///second.mp4"/></asset>
  </resources>
</fcpxml>"#;
        assert_eq!(
            peek_first_asset_source_from_str(xml),
            Some("/first.mp4".to_string())
        );
    }

    #[test]
    fn handles_media_rep_outside_asset_gracefully() {
        // A stray media-rep tag at the root level (or inside a non-asset
        // parent) must not be picked up — we'd return a path that has
        // nothing to do with a project clip.
        let xml = r#"<?xml version="1.0"?>
<fcpxml>
  <media-rep src="file:///should-not-be-picked.mp4"/>
  <resources>
    <asset id="a1"><media-rep src="file:///real.mp4"/></asset>
  </resources>
</fcpxml>"#;
        assert_eq!(
            peek_first_asset_source_from_str(xml),
            Some("/real.mp4".to_string())
        );
    }

    #[test]
    fn returns_none_for_asset_less_project() {
        let xml = r#"<?xml version="1.0"?>
<fcpxml>
  <resources>
    <format id="r1" name="FFVideoFormat1080p24"/>
  </resources>
</fcpxml>"#;
        assert!(peek_first_asset_source_from_str(xml).is_none());
    }

    #[test]
    fn returns_none_for_malformed_xml() {
        assert!(peek_first_asset_source_from_str("not xml at all").is_none());
        assert!(peek_first_asset_source_from_str("<unclosed").is_none());
    }

    #[test]
    fn returns_none_for_empty_string() {
        assert!(peek_first_asset_source_from_str("").is_none());
    }

    #[test]
    fn handles_self_closing_asset_with_inline_src() {
        // Empty-element form: <asset .../> — the parser sees this as
        // Event::Empty, not Event::Start. Without handling both, this
        // case silently misses.
        let xml = r#"<?xml version="1.0"?>
<fcpxml><resources>
  <asset id="a1" src="file:///solo.mp4"/>
</resources></fcpxml>"#;
        assert_eq!(
            peek_first_asset_source_from_str(xml),
            Some("/solo.mp4".to_string())
        );
    }

    #[test]
    fn handles_real_world_uspxml_fixture() {
        // Captured shape from a real ultimateslice .uspxml at the time
        // of this commit. If the export format changes meaningfully,
        // this fixture should be updated alongside.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE fcpxml>
<fcpxml version="1.14" xmlns:us="urn:ultimateslice">
    <resources>
        <format id="r1" name="FFVideoFormat1080p24" frameDuration="1/24s"/>
        <asset id="a_118a3efe_8584_4ced_8642_2e85549fcd83" name="GX010430" hasVideo="1" hasAudio="1">
            <media-rep kind="original-media" src="file:///home/kmwallio/Videos/AudioPlay.Library/GX010430.MP4"/>
        </asset>
    </resources>
</fcpxml>"#;
        assert_eq!(
            peek_first_asset_source_from_str(xml),
            Some("/home/kmwallio/Videos/AudioPlay.Library/GX010430.MP4".to_string())
        );
    }
}
