//! Low-level AAF structured-storage persistence on top of the `cfb` crate.
//!
//! Builds the per-object `properties` stream and the strong/weak reference
//! index streams that the AAF object model requires. (Filled in incrementally;
//! see task 14.)

#![allow(dead_code)]

use crate::aaf::ids;

/// Encode an AAF `properties` stream from `(pid, stored_form, data)` entries.
///
/// Layout (pyaaf2 `core.py::write_properties`): `u8` byte-order, `u8` version,
/// `u16` entry-count, then `entry_count` × (`u16` pid, `u16` stored-form,
/// `u16` data-length), then the concatenated data blocks.
pub fn encode_properties_stream(entries: &[(u16, u16, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(ids::BYTE_ORDER_LE);
    out.push(ids::PROPERTY_VERSION);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (pid, sf, data) in entries {
        out.extend_from_slice(&pid.to_le_bytes());
        out.extend_from_slice(&sf.to_le_bytes());
        out.extend_from_slice(&(data.len() as u16).to_le_bytes());
    }
    for (_, _, data) in entries {
        out.extend_from_slice(data);
    }
    out
}

/// Encode a UTF-16LE, NUL-terminated AAF string (the stored form for `Name`,
/// `URLString`, etc.).
pub fn encode_utf16_string(s: &str) -> Vec<u8> {
    let mut out: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    out.extend_from_slice(&[0, 0]);
    out
}

// ── Scalar value encoders (SF_DATA payloads) ──────────────────────────────

/// `aafUInt32` little-endian (SlotID, SourceMobSlotID, PhysicalTrackNumber, …).
pub fn encode_u32(v: u32) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}

/// `aafInt64` / `aafLength` / `aafPosition` little-endian (Length, StartTime,
/// Origin, …).
pub fn encode_i64(v: i64) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}

/// `aafRational` = `Int32 numerator` + `Int32 denominator`, both LE
/// (EditRate, SampleRate). e.g. 48000/1 → `80bb0000 01000000`.
pub fn encode_rational(num: i32, den: i32) -> Vec<u8> {
    let mut out = num.to_le_bytes().to_vec();
    out.extend_from_slice(&den.to_le_bytes());
    out
}

/// Fixed 16-byte prefix shared by every AAF `MobID` (SMPTE UMID label +
/// length + instance high bytes), captured from the pyaaf2 reference.
pub const MOB_ID_PREFIX: [u8; 16] = [
    0x06, 0x0a, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x05, 0x01, 0x01, 0x0f, 0x20, 0x13, 0x00, 0x00, 0x00,
];

/// Encode a 32-byte `MobID`: the fixed prefix + 16 unique (material) bytes.
pub fn encode_mob_id(unique: [u8; 16]) -> Vec<u8> {
    let mut out = MOB_ID_PREFIX.to_vec();
    out.extend_from_slice(&unique);
    out
}

/// A zero `MobID` — the "original source" sentinel used by the `SourceClip`
/// at the bottom of a source-mob derivation chain.
pub fn encode_mob_id_zero() -> Vec<u8> {
    vec![0u8; 32]
}

/// Encode a weak object reference (SF_WEAK_OBJECT_REFERENCE) by target unique
/// key: `refIndex:u16` (path into `/referenced properties`), `keyPid:u16`,
/// `keySize:u8`, then the `keySize`-byte key (an AUID). Used for
/// `DataDefinition`. e.g. Sound → `0200 011b 10 <auid16>`.
pub fn encode_weak_ref(ref_index: u16, key_pid: u16, key: &[u8]) -> Vec<u8> {
    let mut out = ref_index.to_le_bytes().to_vec();
    out.extend_from_slice(&key_pid.to_le_bytes());
    out.push(key.len() as u8);
    out.extend_from_slice(key);
    out
}

// ── Strong-reference index streams ────────────────────────────────────────

/// The property *data* for a strong-reference (single/vector/set) is the
/// UTF-16 base name; children live in `<base>{localKey}` storages and the
/// index in a `<base> index` stream. e.g. `Slots-4403`.
pub fn strong_ref_basename_value(base: &str) -> Vec<u8> {
    encode_utf16_string(base)
}

/// Encode a strong-reference **vector** index stream for `count` ordered
/// entries (keys `0..count`):
/// `count:u32, nextFreeKey:u32, lastFreeKey=0xffffffff, then count×localKey:u32`.
pub fn encode_vector_index(count: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes()); // next free key
    out.extend_from_slice(&0xffff_ffffu32.to_le_bytes()); // last free key
    for k in 0..count {
        out.extend_from_slice(&k.to_le_bytes());
    }
    out
}

/// Encode a strong-reference **set** index stream. Same 12-byte header as a
/// vector, then `keyPid:u16, keySize:u8`, then per entry
/// `localKey:u32, referenceCount:u32, keyBytes[keySize]`. `keys` are the
/// per-entry unique-key byte blobs (e.g. 32-byte MobIDs), in order; local keys
/// are assigned `0..len`.
pub fn encode_set_index(key_pid: u16, key_size: u8, keys: &[Vec<u8>]) -> Vec<u8> {
    let count = keys.len() as u32;
    let mut out = Vec::new();
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes()); // next free key
    out.extend_from_slice(&0xffff_ffffu32.to_le_bytes()); // last free key
    out.extend_from_slice(&key_pid.to_le_bytes());
    out.push(key_size);
    for (local_key, key) in keys.iter().enumerate() {
        debug_assert_eq!(key.len(), key_size as usize);
        out.extend_from_slice(&(local_key as u32).to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes()); // reference count
        out.extend_from_slice(key);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hx(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
    fn unhex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn properties_stream_header_and_entry_layout() {
        // One SF_DATA property, pid=0x0001, value = [0xAA, 0xBB].
        let bytes = encode_properties_stream(&[(0x0001, ids::SF_DATA, vec![0xAA, 0xBB])]);
        assert_eq!(bytes[0], ids::BYTE_ORDER_LE); // 'L'
        assert_eq!(bytes[1], ids::PROPERTY_VERSION); // 32
        assert_eq!(&bytes[2..4], &1u16.to_le_bytes()); // entry count
        assert_eq!(&bytes[4..6], &0x0001u16.to_le_bytes()); // pid
        assert_eq!(&bytes[6..8], &ids::SF_DATA.to_le_bytes()); // stored form
        assert_eq!(&bytes[8..10], &2u16.to_le_bytes()); // data len
        assert_eq!(&bytes[10..12], &[0xAA, 0xBB]); // data
    }

    #[test]
    fn utf16_string_is_nul_terminated_le() {
        // "AB" -> 41 00 42 00 00 00
        assert_eq!(encode_utf16_string("AB"), vec![0x41, 0, 0x42, 0, 0, 0]);
    }

    // The following assert against exact bytes captured from a pyaaf2-written
    // reference AAF, so the encoders stay byte-compatible with real readers.

    #[test]
    fn rational_matches_reference() {
        // EditRate / SampleRate 48000/1.
        assert_eq!(hx(&encode_rational(48000, 1)), "80bb000001000000");
    }

    #[test]
    fn int64_length_matches_reference() {
        // Length 4800 samples.
        assert_eq!(hx(&encode_i64(4800)), "c012000000000000");
    }

    #[test]
    fn mob_id_prefix_matches_reference() {
        let id = encode_mob_id([
            0xc4, 0x88, 0x17, 0x1b, 0x3f, 0x6a, 0xbe, 0x4a, 0xb1, 0x72, 0xf9, 0xb0, 0x06, 0xc6,
            0xec, 0x45,
        ]);
        assert_eq!(
            hx(&id),
            "060a2b340101010501010f2013000000c488171b3f6abe4ab172f9b006c6ec45"
        );
    }

    #[test]
    fn weak_ref_sound_datadef_matches_reference() {
        // SourceClip.DataDefinition → Sound, refIndex=2, keyPid=0x1b01.
        let sound = unhex("0202030100020000060e2b3404010101");
        assert_eq!(
            hx(&encode_weak_ref(2, 0x1b01, &sound)),
            "0200011b100202030100020000060e2b3404010101"
        );
    }

    #[test]
    fn vector_index_matches_reference() {
        // 1-entry Components/Slots/Locator index.
        assert_eq!(hx(&encode_vector_index(1)), "0100000001000000ffffffff00000000");
        // 2-entry Slots index.
        assert_eq!(
            hx(&encode_vector_index(2)),
            "0200000002000000ffffffff0000000001000000"
        );
    }

    #[test]
    fn set_index_mobs_matches_reference() {
        // 4-MobID Mobs set, keyPid=0x4401 (MobID), keySize=32.
        let mob =
            |u: &str| encode_mob_id(unhex(u).try_into().unwrap());
        let keys = vec![
            mob("c488171b3f6abe4ab172f9b006c6ec45"),
            mob("333610abc9e89f4c81bcdc06f174a79a"),
            mob("5097cda0c9138642aa2af71b27e9beaf"),
            mob("78a25ba8d6b48d439c26df82c151d670"),
        ];
        let expected = unhex(
            "0400000004000000ffffffff014420\
             0000000001000000 060a2b340101010501010f2013000000 c488171b3f6abe4ab172f9b006c6ec45\
             0100000001000000 060a2b340101010501010f2013000000 333610abc9e89f4c81bcdc06f174a79a\
             0200000001000000 060a2b340101010501010f2013000000 5097cda0c9138642aa2af71b27e9beaf\
             0300000001000000 060a2b340101010501010f2013000000 78a25ba8d6b48d439c26df82c151d670",
        );
        assert_eq!(encode_set_index(0x4401, 32, &keys), expected);
    }
}
