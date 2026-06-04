//! AAF constant tables — class / property / type AUIDs, property IDs (PIDs),
//! and stored-form codes.
//!
//! Values are ported from pyaaf2 (MIT-licensed,
//! <https://github.com/markreidvfx/pyaaf2>) — `aaf2/model/*` and
//! `aaf2/properties.py` — and from the AAF object specification. Each constant
//! is sourced from the corresponding pyaaf2 definition.

/// A 16-byte AAF AUID (stored in the AAF "swapped" GUID byte order used in
/// property data and CFB CLSIDs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Auid(pub [u8; 16]);

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

// NOTE: class/property/type AUID and PID tables are filled in alongside the
// encoder (task 13) once the exact per-object byte layouts are captured from
// the pyaaf2 reference file.
