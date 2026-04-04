use crate::model::project::Project;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectFileFormat {
    Fcpxml,
    Otio,
}

fn detect_project_file_format(path: &Path) -> ProjectFileFormat {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("otio") => ProjectFileFormat::Otio,
        _ => ProjectFileFormat::Fcpxml,
    }
}

pub(crate) fn load_project_from_path(path: &Path) -> Result<Project, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("Failed to read file: {e}"))?;
    match detect_project_file_format(path) {
        ProjectFileFormat::Otio => crate::otio::parser::parse_otio_with_path(&content, Some(path))
            .map_err(|e| format!("OTIO parse error: {e}")),
        ProjectFileFormat::Fcpxml => {
            crate::fcpxml::parser::parse_fcpxml_with_path(&content, Some(path))
                .map_err(|e| format!("FCPXML parse error: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_otio() -> &'static str {
        r#"{
            "OTIO_SCHEMA": "Timeline.1",
            "name": "OTIO Test",
            "global_start_time": { "OTIO_SCHEMA": "RationalTime.1", "value": 0.0, "rate": 24.0 },
            "tracks": {
                "OTIO_SCHEMA": "Stack.1",
                "name": "tracks",
                "children": []
            }
        }"#
    }

    fn minimal_fcpxml() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.10" xmlns:us="urn:ultimateslice">
  <resources>
    <format id="r1" frameDuration="1/24s" width="1920" height="1080"/>
  </resources>
  <library>
    <event>
      <project name="FCPXML Test">
        <sequence duration="0/24s" format="r1">
          <spine/>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#
    }

    #[test]
    fn detect_project_file_format_is_case_insensitive_for_otio() {
        assert_eq!(
            detect_project_file_format(Path::new("/tmp/project.OTIO")),
            ProjectFileFormat::Otio
        );
        assert_eq!(
            detect_project_file_format(Path::new("/tmp/project.fcpxml")),
            ProjectFileFormat::Fcpxml
        );
    }

    #[test]
    fn load_project_from_path_parses_otio() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sample.OTIO");
        std::fs::write(&path, minimal_otio()).expect("write otio fixture");

        let project = load_project_from_path(&path).expect("otio load should succeed");
        assert_eq!(project.title, "OTIO Test");
    }

    #[test]
    fn load_project_from_path_resolves_relative_otio_media_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let otio_dir = dir.path().join("interchange");
        std::fs::create_dir_all(&otio_dir).expect("create otio dir");
        let path = otio_dir.join("sample.otio");
        std::fs::write(
            &path,
            r#"{
                "OTIO_SCHEMA": "Timeline.1",
                "name": "Relative OTIO",
                "global_start_time": { "OTIO_SCHEMA": "RationalTime.1", "value": 0.0, "rate": 24.0 },
                "tracks": {
                    "OTIO_SCHEMA": "Stack.1",
                    "name": "tracks",
                    "children": [{
                        "OTIO_SCHEMA": "Track.1",
                        "name": "V1",
                        "kind": "Video",
                        "children": [{
                            "OTIO_SCHEMA": "Clip.1",
                            "name": "Shot",
                            "source_range": {
                                "OTIO_SCHEMA": "TimeRange.1",
                                "start_time": { "OTIO_SCHEMA": "RationalTime.1", "value": 0.0, "rate": 24.0 },
                                "duration": { "OTIO_SCHEMA": "RationalTime.1", "value": 24.0, "rate": 24.0 }
                            },
                            "media_reference": {
                                "OTIO_SCHEMA": "ExternalReference.1",
                                "target_url": "../media/clip.mp4"
                            }
                        }]
                    }]
                }
            }"#,
        )
        .expect("write relative otio fixture");

        let project = load_project_from_path(&path).expect("relative otio load should succeed");
        assert_eq!(
            project.tracks[0].clips[0].source_path,
            dir.path().join("media/clip.mp4").to_string_lossy()
        );
    }

    #[test]
    fn load_project_from_path_parses_fcpxml() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sample.fcpxml");
        std::fs::write(&path, minimal_fcpxml()).expect("write fcpxml fixture");

        let project = load_project_from_path(&path).expect("fcpxml load should succeed");
        assert_eq!(project.title, "FCPXML Test");
    }
}
