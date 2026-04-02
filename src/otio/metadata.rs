use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use crate::model::clip::{SubtitleHighlightMode, SubtitleSegment};

pub(crate) const ULTIMATESLICE_OTIO_METADATA_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct UltimateSliceClipOtioMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) speed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reverse: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) volume: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pan: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) brightness: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) contrast: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) saturation: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) opacity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_font: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_x: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_y: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_outline_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_outline_width: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_shadow: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_shadow_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_shadow_offset_x: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_shadow_offset_y: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_bg_box: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_bg_box_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_bg_box_padding: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_clip_bg_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_secondary_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_segments: Option<Vec<SubtitleSegment>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitles_language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_font: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_outline_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_outline_width: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_bg_box: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_bg_box_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_highlight_mode: Option<SubtitleHighlightMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_highlight_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_word_window_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_position_y: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct UltimateSliceTrackOtioMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) muted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) locked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) soloed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) audio_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) duck: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) duck_amount_db: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct UltimateSliceProjectOtioMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) height: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct UltimateSliceTransitionOtioMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) transition_kind: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct UltimateSliceMarkerOtioMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) color_rgba: Option<String>,
}

fn wrap_section<T: Serialize>(section: &str, payload: &T) -> Value {
    let mut ultimateslice = serde_json::Map::new();
    ultimateslice.insert(
        "version".to_string(),
        Value::from(ULTIMATESLICE_OTIO_METADATA_VERSION),
    );
    let payload_value = match serde_json::to_value(payload) {
        Ok(value) => value,
        Err(err) => {
            log::error!("failed to serialize OTIO ultimateslice {section} metadata: {err}");
            Value::Null
        }
    };
    ultimateslice.insert(section.to_string(), payload_value);

    let mut root = serde_json::Map::new();
    root.insert("ultimateslice".to_string(), Value::Object(ultimateslice));
    Value::Object(root)
}

fn parse_section<T: DeserializeOwned>(metadata: &Value, section: &str) -> Option<T> {
    let us = metadata.get("ultimateslice")?;
    let candidate = us.get(section).cloned().unwrap_or_else(|| us.clone());
    match serde_json::from_value(candidate) {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            log::warn!("failed to parse OTIO ultimateslice {section} metadata: {err}");
            None
        }
    }
}

pub(crate) fn wrap_clip_metadata(metadata: &UltimateSliceClipOtioMetadata) -> Value {
    wrap_section("clip", metadata)
}

pub(crate) fn wrap_track_metadata(metadata: &UltimateSliceTrackOtioMetadata) -> Value {
    wrap_section("track", metadata)
}

pub(crate) fn wrap_project_metadata(metadata: &UltimateSliceProjectOtioMetadata) -> Value {
    wrap_section("project", metadata)
}

pub(crate) fn wrap_transition_metadata(metadata: &UltimateSliceTransitionOtioMetadata) -> Value {
    wrap_section("transition", metadata)
}

pub(crate) fn wrap_marker_metadata(metadata: &UltimateSliceMarkerOtioMetadata) -> Value {
    wrap_section("marker", metadata)
}

pub(crate) fn clip_metadata_from_root(metadata: &Value) -> Option<UltimateSliceClipOtioMetadata> {
    parse_section(metadata, "clip")
}

pub(crate) fn track_metadata_from_root(metadata: &Value) -> Option<UltimateSliceTrackOtioMetadata> {
    parse_section(metadata, "track")
}

pub(crate) fn project_metadata_from_root(
    metadata: &Value,
) -> Option<UltimateSliceProjectOtioMetadata> {
    parse_section(metadata, "project")
}

pub(crate) fn transition_metadata_from_root(
    metadata: &Value,
) -> Option<UltimateSliceTransitionOtioMetadata> {
    parse_section(metadata, "transition")
}

pub(crate) fn marker_metadata_from_root(
    metadata: &Value,
) -> Option<UltimateSliceMarkerOtioMetadata> {
    parse_section(metadata, "marker")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn wrap_clip_metadata_emits_versioned_section() {
        let value = wrap_clip_metadata(&UltimateSliceClipOtioMetadata {
            kind: Some("Title".to_string()),
            speed: Some(1.25),
            ..UltimateSliceClipOtioMetadata::default()
        });

        assert_eq!(
            value["ultimateslice"]["version"].as_u64(),
            Some(ULTIMATESLICE_OTIO_METADATA_VERSION as u64)
        );
        assert_eq!(value["ultimateslice"]["clip"]["kind"], "Title");
        assert_eq!(value["ultimateslice"]["clip"]["speed"], 1.25);
    }

    #[test]
    fn parse_clip_metadata_accepts_legacy_flat_shape() {
        let value = json!({
            "ultimateslice": {
                "kind": "Video",
                "speed": 0.5,
                "reverse": true
            }
        });

        let parsed = clip_metadata_from_root(&value).expect("clip metadata should parse");
        assert_eq!(parsed.kind.as_deref(), Some("Video"));
        assert_eq!(parsed.speed, Some(0.5));
        assert_eq!(parsed.reverse, Some(true));
    }

    #[test]
    fn parse_track_metadata_accepts_nested_structured_shape() {
        let value = json!({
            "ultimateslice": {
                "version": 1,
                "track": {
                    "muted": true,
                    "audio_role": "dialogue"
                }
            }
        });

        let parsed = track_metadata_from_root(&value).expect("track metadata should parse");
        assert_eq!(parsed.muted, Some(true));
        assert_eq!(parsed.audio_role.as_deref(), Some("dialogue"));
    }
}
