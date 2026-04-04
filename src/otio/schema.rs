//! OTIO JSON schema types for OpenTimelineIO serialization/deserialization.
//!
//! These are intermediate types used only for JSON I/O — not stored in the
//! project model.  They mirror the OTIO 0.17 JSON schema closely enough for
//! interop with DaVinci Resolve, Premiere (via adapter), Nuke, RV, etc.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

// ---------------------------------------------------------------------------
// Primitive value types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtioRationalTime {
    #[serde(rename = "OTIO_SCHEMA")]
    pub schema: String,
    pub value: f64,
    pub rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtioTimeRange {
    #[serde(rename = "OTIO_SCHEMA")]
    pub schema: String,
    pub start_time: OtioRationalTime,
    pub duration: OtioRationalTime,
}

// ---------------------------------------------------------------------------
// Media references
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtioExternalReference {
    #[serde(rename = "OTIO_SCHEMA")]
    pub schema: String,
    pub target_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_range: Option<OtioTimeRange>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtioMissingReference {
    #[serde(rename = "OTIO_SCHEMA")]
    pub schema: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metadata: serde_json::Value,
}

/// Wraps the two media-reference variants OTIO supports.
#[derive(Debug, Clone)]
pub enum OtioMediaReference {
    External(OtioExternalReference),
    Missing(OtioMissingReference),
}

impl Serialize for OtioMediaReference {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            OtioMediaReference::External(r) => r.serialize(s),
            OtioMediaReference::Missing(r) => r.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for OtioMediaReference {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        let schema = v.get("OTIO_SCHEMA").and_then(|s| s.as_str()).unwrap_or("");
        if schema.starts_with("ExternalReference") {
            serde_json::from_value(v)
                .map(OtioMediaReference::External)
                .map_err(serde::de::Error::custom)
        } else {
            serde_json::from_value(v)
                .map(OtioMediaReference::Missing)
                .map_err(serde::de::Error::custom)
        }
    }
}

// ---------------------------------------------------------------------------
// Effects & markers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtioEffect {
    #[serde(rename = "OTIO_SCHEMA")]
    pub schema: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtioMarker {
    #[serde(rename = "OTIO_SCHEMA")]
    pub schema: String,
    pub name: String,
    pub marked_range: OtioTimeRange,
    #[serde(default)]
    pub color: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metadata: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Track children — heterogeneous (Clip | Gap | Transition)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtioClip {
    #[serde(rename = "OTIO_SCHEMA")]
    pub schema: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_range: Option<OtioTimeRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_reference: Option<OtioMediaReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<OtioEffect>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<OtioMarker>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtioGap {
    #[serde(rename = "OTIO_SCHEMA")]
    pub schema: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_range: Option<OtioTimeRange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<OtioEffect>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<OtioMarker>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtioTransition {
    #[serde(rename = "OTIO_SCHEMA")]
    pub schema: String,
    pub name: String,
    pub transition_type: String,
    pub in_offset: OtioRationalTime,
    pub out_offset: OtioRationalTime,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metadata: serde_json::Value,
}

/// Heterogeneous track child — dispatched on `OTIO_SCHEMA`.
#[derive(Debug, Clone)]
pub enum OtioTrackChild {
    Clip(OtioClip),
    Gap(OtioGap),
    Transition(OtioTransition),
}

impl Serialize for OtioTrackChild {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            OtioTrackChild::Clip(c) => c.serialize(s),
            OtioTrackChild::Gap(g) => g.serialize(s),
            OtioTrackChild::Transition(t) => t.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for OtioTrackChild {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        let schema = v.get("OTIO_SCHEMA").and_then(|s| s.as_str()).unwrap_or("");
        if schema.starts_with("Clip") {
            serde_json::from_value(v)
                .map(OtioTrackChild::Clip)
                .map_err(serde::de::Error::custom)
        } else if schema.starts_with("Gap") {
            serde_json::from_value(v)
                .map(OtioTrackChild::Gap)
                .map_err(serde::de::Error::custom)
        } else if schema.starts_with("Transition") {
            serde_json::from_value(v)
                .map(OtioTrackChild::Transition)
                .map_err(serde::de::Error::custom)
        } else {
            Err(serde::de::Error::custom(format!(
                "unknown OTIO_SCHEMA for track child: {schema}"
            )))
        }
    }
}

// ---------------------------------------------------------------------------
// Container types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtioTrack {
    #[serde(rename = "OTIO_SCHEMA")]
    pub schema: String,
    pub name: String,
    pub kind: String,
    pub children: Vec<OtioTrackChild>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<OtioEffect>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<OtioMarker>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtioStack {
    #[serde(rename = "OTIO_SCHEMA")]
    pub schema: String,
    pub name: String,
    pub children: Vec<OtioTrack>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtioTimeline {
    #[serde(rename = "OTIO_SCHEMA")]
    pub schema: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_start_time: Option<OtioRationalTime>,
    pub tracks: OtioStack,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metadata: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Helper constructors
// ---------------------------------------------------------------------------

pub fn rational_time(value: f64, rate: f64) -> OtioRationalTime {
    OtioRationalTime {
        schema: "RationalTime.1".into(),
        value,
        rate,
    }
}

pub fn time_range(start_value: f64, duration_value: f64, rate: f64) -> OtioTimeRange {
    OtioTimeRange {
        schema: "TimeRange.1".into(),
        start_time: rational_time(start_value, rate),
        duration: rational_time(duration_value, rate),
    }
}

/// Convert UltimateSlice nanoseconds to an OTIO RationalTime at the given
/// frame rate.  The result is expressed in frames (not seconds).
pub fn ns_to_rational_time(ns: u64, rate: f64) -> OtioRationalTime {
    let value = (ns as f64 * rate) / 1_000_000_000.0;
    rational_time(value, rate)
}

/// Convert an OTIO RationalTime back to nanoseconds.
pub fn rational_time_to_ns(rt: &OtioRationalTime) -> u64 {
    if rt.rate <= 0.0 {
        return 0;
    }
    ((rt.value / rt.rate) * 1_000_000_000.0).round() as u64
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ns_to_rational_time_24fps() {
        let rt = ns_to_rational_time(1_000_000_000, 24.0);
        assert!((rt.value - 24.0).abs() < 1e-6);
        assert!((rt.rate - 24.0).abs() < 1e-6);
    }

    #[test]
    fn test_rational_time_to_ns_round_trip() {
        let ns: u64 = 2_500_000_000; // 2.5 seconds
        let rt = ns_to_rational_time(ns, 24.0);
        let back = rational_time_to_ns(&rt);
        // Allow 1-frame tolerance at 24 fps (~41.67 ms).
        assert!((back as i64 - ns as i64).unsigned_abs() < 42_000_000);
    }

    #[test]
    fn test_rational_time_to_ns_zero_rate() {
        let rt = rational_time(100.0, 0.0);
        assert_eq!(rational_time_to_ns(&rt), 0);
    }

    #[test]
    fn test_track_child_serde_clip() {
        let child = OtioTrackChild::Clip(OtioClip {
            schema: "Clip.1".into(),
            name: "test".into(),
            source_range: None,
            media_reference: None,
            effects: vec![],
            markers: vec![],
            metadata: serde_json::Value::Null,
        });
        let json = serde_json::to_string(&child).unwrap();
        let back: OtioTrackChild = serde_json::from_str(&json).unwrap();
        match back {
            OtioTrackChild::Clip(c) => assert_eq!(c.name, "test"),
            _ => panic!("expected Clip"),
        }
    }

    #[test]
    fn test_track_child_serde_gap() {
        let child = OtioTrackChild::Gap(OtioGap {
            schema: "Gap.1".into(),
            name: "".into(),
            source_range: Some(time_range(0.0, 24.0, 24.0)),
            effects: vec![],
            markers: vec![],
            metadata: serde_json::Value::Null,
        });
        let json = serde_json::to_string(&child).unwrap();
        let back: OtioTrackChild = serde_json::from_str(&json).unwrap();
        match back {
            OtioTrackChild::Gap(g) => {
                let sr = g.source_range.unwrap();
                assert!((sr.duration.value - 24.0).abs() < 1e-6);
            }
            _ => panic!("expected Gap"),
        }
    }

    #[test]
    fn test_track_child_serde_transition() {
        let child = OtioTrackChild::Transition(OtioTransition {
            schema: "Transition.1".into(),
            name: "Cross Dissolve".into(),
            transition_type: "SMPTE_Dissolve".into(),
            in_offset: rational_time(12.0, 24.0),
            out_offset: rational_time(12.0, 24.0),
            metadata: serde_json::Value::Null,
        });
        let json = serde_json::to_string(&child).unwrap();
        let back: OtioTrackChild = serde_json::from_str(&json).unwrap();
        match back {
            OtioTrackChild::Transition(t) => {
                assert_eq!(t.transition_type, "SMPTE_Dissolve");
            }
            _ => panic!("expected Transition"),
        }
    }

    #[test]
    fn test_media_reference_serde() {
        let ext = OtioMediaReference::External(OtioExternalReference {
            schema: "ExternalReference.1".into(),
            target_url: "file:///path/to/media.mp4".into(),
            available_range: None,
            metadata: serde_json::Value::Null,
        });
        let json = serde_json::to_string(&ext).unwrap();
        let back: OtioMediaReference = serde_json::from_str(&json).unwrap();
        match back {
            OtioMediaReference::External(r) => {
                assert_eq!(r.target_url, "file:///path/to/media.mp4");
            }
            _ => panic!("expected ExternalReference"),
        }

        let miss = OtioMediaReference::Missing(OtioMissingReference {
            schema: "MissingReference.1".into(),
            metadata: serde_json::Value::Null,
        });
        let json = serde_json::to_string(&miss).unwrap();
        let back: OtioMediaReference = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, OtioMediaReference::Missing(_)));
    }
}
