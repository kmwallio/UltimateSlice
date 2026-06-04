//! Builds an AAF file from an UltimateSlice [`Project`]: starts from the
//! embedded pyaaf2 skeleton ([`crate::aaf::TEMPLATE_AAF`]) and appends the
//! dynamic object graph (CompositionMob, SourceMobs, slots, sequences,
//! source clips, descriptors, locators) for the audio tracks plus a flattened
//! video reference track. Linked media (external `file://` references).
//!
//! (Object-graph construction is implemented incrementally; see task 15.)

#![allow(dead_code)]

use std::io::Cursor;
use std::path::Path;

use anyhow::Result;

use crate::model::project::Project;

/// Build the AAF document for `project` as an in-memory byte buffer. Used by
/// both [`write_aaf`] and the tests.
pub fn build_aaf_bytes(_project: &Project) -> Result<Vec<u8>> {
    // Start from the known-good pyaaf2 skeleton (header + metadictionary +
    // empty ContentStorage.Mobs). The dynamic Mob graph is appended here.
    let comp = cfb::CompoundFile::open(Cursor::new(crate::aaf::TEMPLATE_AAF.to_vec()))?;
    // TODO(task 15): append CompositionMob / SourceMobs into ContentStorage.
    Ok(comp.into_inner().into_inner())
}

/// Export `project` to an AAF file at `path`.
pub fn write_aaf(project: &Project, path: &Path) -> Result<()> {
    let bytes = build_aaf_bytes(project)?;
    std::fs::write(path, bytes)?;
    Ok(())
}
