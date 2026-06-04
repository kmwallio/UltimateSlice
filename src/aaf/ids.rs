//! AAF constant tables — class CLSIDs, property IDs (PIDs), stored-form codes,
//! and data-definition AUIDs.
//!
//! Values are ported from pyaaf2 (MIT-licensed,
//! <https://github.com/markreidvfx/pyaaf2>) — `aaf2/model/*`,
//! `aaf2/properties.py` — and cross-checked byte-for-byte against a
//! pyaaf2-written reference AAF (see `src/aaf/encode.rs` tests).

// Constants are consumed by the writer (task 15), added incrementally.
#![allow(dead_code)]

// ── Stored-form codes (pyaaf2 `properties.py`) ────────────────────────────
pub const SF_DATA: u16 = 0x0082;
pub const SF_STRONG_OBJECT_REFERENCE: u16 = 0x0022;
pub const SF_STRONG_OBJECT_REFERENCE_VECTOR: u16 = 0x0032;
pub const SF_STRONG_OBJECT_REFERENCE_SET: u16 = 0x003A;
pub const SF_WEAK_OBJECT_REFERENCE: u16 = 0x0002;
pub const SF_UNIQUE_OBJECT_ID: u16 = 0x0086;

/// Properties-stream header version byte (pyaaf2 `PROPERTY_VERSION`).
pub const PROPERTY_VERSION: u8 = 32;
/// Properties-stream byte-order marker (little-endian 'L').
pub const BYTE_ORDER_LE: u8 = 0x4C;

// ── Class CLSIDs (AAF AUIDs in GUID-string form; parse with `uuid`) ────────
//
// These are written as the CFB storage CLSID for each object.
pub const CLSID_CONTENT_STORAGE: &str = "0d010101-0101-1800-060e-2b3402060101";
pub const CLSID_COMPOSITION_MOB: &str = "0d010101-0101-3500-060e-2b3402060101";
pub const CLSID_SOURCE_MOB: &str = "0d010101-0101-3700-060e-2b3402060101";
pub const CLSID_MASTER_MOB: &str = "0d010101-0101-3600-060e-2b3402060101";
pub const CLSID_TIMELINE_MOB_SLOT: &str = "0d010101-0101-3b00-060e-2b3402060101";
pub const CLSID_SEQUENCE: &str = "0d010101-0101-0f00-060e-2b3402060101";
pub const CLSID_SOURCE_CLIP: &str = "0d010101-0101-1100-060e-2b3402060101";
pub const CLSID_FILLER: &str = "0d010101-0101-0900-060e-2b3402060101";
pub const CLSID_TIMECODE: &str = "0d010101-0101-1400-060e-2b3402060101";
pub const CLSID_WAVE_DESCRIPTOR: &str = "0d010101-0101-2c00-060e-2b3402060101";
pub const CLSID_PCM_DESCRIPTOR: &str = "0d010101-0101-4800-060e-2b3402060101";
pub const CLSID_CDCI_DESCRIPTOR: &str = "0d010101-0101-2800-060e-2b3402060101";
pub const CLSID_IMPORT_DESCRIPTOR: &str = "0d010101-0101-4a00-060e-2b3402060101";
pub const CLSID_NETWORK_LOCATOR: &str = "0d010101-0101-3200-060e-2b3402060101";

// ── Property IDs (PIDs) ───────────────────────────────────────────────────
//
// ContentStorage
pub const PID_CONTENT_MOBS: u16 = 0x1901; // SF_STRONG_OBJECT_REFERENCE_SET, key = MobID

// Mob (common to Composition/Source/Master)
pub const PID_MOB_ID: u16 = 0x4401; // SF_DATA (32-byte MobID) — also the Mobs set key pid
pub const PID_MOB_NAME: u16 = 0x4402; // UTF-16
pub const PID_MOB_SLOTS: u16 = 0x4403; // SF_STRONG_OBJECT_REFERENCE_VECTOR
pub const PID_MOB_LAST_MODIFIED: u16 = 0x4404; // TimeStamp (8 bytes)
pub const PID_MOB_CREATION_TIME: u16 = 0x4405; // TimeStamp (8 bytes)
pub const PID_SOURCEMOB_ESSENCE_DESCRIPTION: u16 = 0x4701; // SF_STRONG_OBJECT_REFERENCE

// MobSlot / TimelineMobSlot
pub const PID_MOBSLOT_SLOT_ID: u16 = 0x4801; // u32
pub const PID_MOBSLOT_SLOT_NAME: u16 = 0x4802; // UTF-16
pub const PID_MOBSLOT_SEGMENT: u16 = 0x4803; // SF_STRONG_OBJECT_REFERENCE
pub const PID_MOBSLOT_PHYSICAL_TRACK_NUM: u16 = 0x4804; // u32 (optional)
pub const PID_TIMELINE_EDIT_RATE: u16 = 0x4b01; // Rational
pub const PID_TIMELINE_ORIGIN: u16 = 0x4b02; // i64

// Component (Segment base — Sequence, SourceClip, Filler, Timecode)
pub const PID_COMPONENT_DATA_DEFINITION: u16 = 0x0201; // SF_WEAK_OBJECT_REFERENCE
pub const PID_COMPONENT_LENGTH: u16 = 0x0202; // i64

// Sequence
pub const PID_SEQUENCE_COMPONENTS: u16 = 0x1001; // SF_STRONG_OBJECT_REFERENCE_VECTOR

// SourceClip
pub const PID_SOURCECLIP_SOURCE_ID: u16 = 0x1101; // 32-byte MobID (zero = original)
pub const PID_SOURCECLIP_SOURCE_MOB_SLOT_ID: u16 = 0x1102; // u32
pub const PID_SOURCECLIP_START_TIME: u16 = 0x1201; // i64

// Timecode
pub const PID_TIMECODE_START: u16 = 0x1501; // i64 (frames)
pub const PID_TIMECODE_FPS: u16 = 0x1502; // u16
pub const PID_TIMECODE_DROP: u16 = 0x1503; // bool (1 byte)

// FileDescriptor / WAVEDescriptor / PCMDescriptor
pub const PID_FILEDESC_LOCATOR: u16 = 0x2f01; // SF_STRONG_OBJECT_REFERENCE_VECTOR
pub const PID_FILEDESC_SAMPLE_RATE: u16 = 0x3001; // Rational
pub const PID_FILEDESC_LENGTH: u16 = 0x3002; // i64
pub const PID_WAVE_SUMMARY: u16 = 0x3801; // SF_DATA (RIFF fmt chunk)

// NetworkLocator
pub const PID_NETWORK_LOCATOR_URL: u16 = 0x4001; // UTF-16 file:// URL

// ── Weak-reference resolution ─────────────────────────────────────────────
//
// `DefinitionObject.Identification` is the AUID key the DataDefinition weak
// reference matches on; refIndex 2 is the `/referenced properties` path to
// `Dictionary.DataDefinitions` (present in the embedded template).
pub const PID_DEFINITION_IDENTIFICATION: u16 = 0x1b01;
pub const DATADEF_REF_INDEX: u16 = 0x0002;

// ── DataDefinition AUIDs (byte form used in property data / weak-ref keys) ─
pub const DATADEF_SOUND: [u8; 16] = [
    0x02, 0x02, 0x03, 0x01, 0x00, 0x02, 0x00, 0x00, 0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x01,
];
pub const DATADEF_PICTURE: [u8; 16] = [
    0x02, 0x02, 0x03, 0x01, 0x00, 0x01, 0x00, 0x00, 0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x01,
];
pub const DATADEF_TIMECODE: [u8; 16] = [
    0x01, 0x02, 0x03, 0x01, 0x00, 0x01, 0x00, 0x00, 0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x01,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_clsids_parse_as_uuids() {
        for s in [
            CLSID_CONTENT_STORAGE,
            CLSID_COMPOSITION_MOB,
            CLSID_SOURCE_MOB,
            CLSID_TIMELINE_MOB_SLOT,
            CLSID_SEQUENCE,
            CLSID_SOURCE_CLIP,
            CLSID_WAVE_DESCRIPTOR,
            CLSID_NETWORK_LOCATOR,
        ] {
            assert!(uuid::Uuid::parse_str(s).is_ok(), "bad CLSID {s}");
        }
    }

    #[test]
    fn datadef_sound_matches_reference_weak_ref_key() {
        // Must equal the AUID seen in the reference SourceClip.DataDefinition.
        let hex: String = DATADEF_SOUND.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "0202030100020000060e2b3404010101");
    }
}
