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

#[cfg(test)]
mod tests {
    use super::*;

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
}
