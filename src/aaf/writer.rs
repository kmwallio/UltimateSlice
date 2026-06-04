//! Builds an AAF file from an UltimateSlice [`Project`].
//!
//! Starts from the embedded pyaaf2 skeleton ([`crate::aaf::TEMPLATE_AAF`] —
//! header + full metadictionary + an empty `ContentStorage.Mobs` set) and
//! appends the dynamic object graph with the `cfb` crate:
//!
//! - one **CompositionMob** (the timeline): a `TimelineMobSlot` per audio track
//!   (a `Sequence` of `Filler` gaps + `SourceClip`s), one flattened **video
//!   reference** slot, and a **Timecode** slot;
//! - one **SourceMob** per unique source file — audio gets a `PCMDescriptor`,
//!   the video reference an `ImportDescriptor` — each with a `NetworkLocator`
//!   pointing at the external `file://` URL (linked media; no embedded essence).
//!
//! All byte layouts come from [`crate::aaf::encode`] and are cross-checked
//! against a pyaaf2 reference (see that module's tests).

use std::collections::BTreeMap;
use std::io::{Cursor, Write};
use std::path::Path;

use anyhow::{anyhow, Result};
use uuid::Uuid;

use crate::aaf::encode::*;
use crate::aaf::ids::*;
use crate::model::clip::{Clip, ClipKind};
use crate::model::project::{FrameRate, Project};
use crate::model::track::Track;

/// Fixed AAF `TimeStamp` (2024-01-01 00:00:00) so output is deterministic.
/// Layout: `u16` year LE, then `u8` month, day, hour, minute, second, frac.
const FIXED_TIMESTAMP: [u8; 8] = [0xe8, 0x07, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00];

const SAMPLE_RATE_DEFAULT: u32 = 48000;

type Cf = cfb::CompoundFile<Cursor<Vec<u8>>>;

/// One component of a composition `Sequence`.
enum Comp {
    Filler { len: i64 },
    Clip { len: i64, source_id: Vec<u8>, source_slot: u32, start: i64 },
}

struct Builder {
    cf: Cf,
    content: String, // path to ContentStorage, e.g. "/Header-2/Content-3b03"
    mobs_base: String, // e.g. "Mobs-1901"
    next_unique: u32,
    mob_keys: Vec<Vec<u8>>, // 32-byte MobIDs, in append order
}

impl Builder {
    fn create_storage(&mut self, path: &str, clsid: &str) -> Result<()> {
        self.cf.create_storage(path)?;
        self.cf.set_storage_clsid(path, Uuid::parse_str(clsid)?)?;
        Ok(())
    }

    fn write_stream(&mut self, path: &str, data: &[u8]) -> Result<()> {
        let mut s = self.cf.create_stream(path)?;
        s.write_all(data)?;
        Ok(())
    }

    /// Write an object's `properties` stream from `(pid, stored_form, data)`.
    fn write_props(&mut self, path: &str, entries: &[(u16, u16, Vec<u8>)]) -> Result<()> {
        let data = encode_properties_stream(entries);
        self.write_stream(&format!("{path}/properties"), &data)
    }

    /// Deterministic, file-unique 32-byte MobID.
    fn next_mob_id(&mut self) -> Vec<u8> {
        let mut u = [0u8; 16];
        u[0..4].copy_from_slice(b"USAF");
        u[12..16].copy_from_slice(&self.next_unique.to_le_bytes());
        self.next_unique += 1;
        encode_mob_id(u)
    }

    // ── leaf builders ────────────────────────────────────────────────────

    fn build_source_clip(
        &mut self,
        path: &str,
        datadef: &[u8; 16],
        len: i64,
        source_id: &[u8],
        source_slot: u32,
        start: i64,
    ) -> Result<()> {
        self.create_storage(path, CLSID_SOURCE_CLIP)?;
        self.write_props(
            path,
            &[
                (PID_COMPONENT_DATA_DEFINITION, SF_WEAK_OBJECT_REFERENCE, weak_datadef(datadef)),
                (PID_COMPONENT_LENGTH, SF_DATA, encode_i64(len)),
                (PID_SOURCECLIP_SOURCE_ID, SF_DATA, source_id.to_vec()),
                (PID_SOURCECLIP_SOURCE_MOB_SLOT_ID, SF_DATA, encode_u32(source_slot)),
                (PID_SOURCECLIP_START_TIME, SF_DATA, encode_i64(start)),
            ],
        )
    }

    fn build_filler(&mut self, path: &str, datadef: &[u8; 16], len: i64) -> Result<()> {
        self.create_storage(path, CLSID_FILLER)?;
        self.write_props(
            path,
            &[
                (PID_COMPONENT_DATA_DEFINITION, SF_WEAK_OBJECT_REFERENCE, weak_datadef(datadef)),
                (PID_COMPONENT_LENGTH, SF_DATA, encode_i64(len)),
            ],
        )
    }

    /// Build a `Sequence` at `path` from ordered `comps`.
    fn build_sequence(&mut self, path: &str, datadef: &[u8; 16], comps: &[Comp]) -> Result<()> {
        let total: i64 = comps
            .iter()
            .map(|c| match c {
                Comp::Filler { len } => *len,
                Comp::Clip { len, .. } => *len,
            })
            .sum();
        self.create_storage(path, CLSID_SEQUENCE)?;
        let comp_base = "Components-1001";
        self.write_props(
            path,
            &[
                (PID_COMPONENT_DATA_DEFINITION, SF_WEAK_OBJECT_REFERENCE, weak_datadef(datadef)),
                (PID_COMPONENT_LENGTH, SF_DATA, encode_i64(total)),
                (
                    PID_SEQUENCE_COMPONENTS,
                    SF_STRONG_OBJECT_REFERENCE_VECTOR,
                    strong_ref_basename_value(comp_base),
                ),
            ],
        )?;
        self.write_stream(
            &format!("{path}/{comp_base} index"),
            &encode_vector_index(comps.len() as u32),
        )?;
        for (i, c) in comps.iter().enumerate() {
            let cpath = format!("{path}/{comp_base}{{{i}}}");
            match c {
                Comp::Filler { len } => self.build_filler(&cpath, datadef, *len)?,
                Comp::Clip { len, source_id, source_slot, start } => {
                    self.build_source_clip(&cpath, datadef, *len, source_id, *source_slot, *start)?
                }
            }
        }
        Ok(())
    }

    /// Build a `TimelineMobSlot` at `path` whose Segment is built by `seg`.
    #[allow(clippy::too_many_arguments)]
    fn build_timeline_slot(
        &mut self,
        path: &str,
        slot_id: u32,
        edit_rate: (i32, i32),
        phys_track: Option<u32>,
        segment_clsid: &str,
        segment_props: Vec<(u16, u16, Vec<u8>)>,
    ) -> Result<String> {
        self.create_storage(path, CLSID_TIMELINE_MOB_SLOT)?;
        let seg_base = "Segment-4803";
        let mut entries = vec![
            (PID_MOBSLOT_SLOT_ID, SF_DATA, encode_u32(slot_id)),
            (PID_MOBSLOT_SLOT_NAME, SF_DATA, encode_utf16_string("")),
            (
                PID_MOBSLOT_SEGMENT,
                SF_STRONG_OBJECT_REFERENCE,
                strong_ref_basename_value(seg_base),
            ),
        ];
        if let Some(pt) = phys_track {
            entries.push((PID_MOBSLOT_PHYSICAL_TRACK_NUM, SF_DATA, encode_u32(pt)));
        }
        entries.push((
            PID_TIMELINE_EDIT_RATE,
            SF_DATA,
            encode_rational(edit_rate.0, edit_rate.1),
        ));
        entries.push((PID_TIMELINE_ORIGIN, SF_DATA, encode_i64(0)));
        self.write_props(path, &entries)?;
        let seg_path = format!("{path}/{seg_base}");
        self.create_storage(&seg_path, segment_clsid)?;
        self.write_props(&seg_path, &segment_props)?;
        Ok(seg_path)
    }

    /// Build a `NetworkLocator` child + its parent `Locator` vector on a
    /// descriptor at `desc_path`. Returns nothing; appends to props is the
    /// caller's job (caller includes the Locator property entry).
    fn build_network_locator(&mut self, desc_path: &str, url: &str) -> Result<()> {
        let base = "Locator-2f01";
        self.write_stream(&format!("{desc_path}/{base} index"), &encode_vector_index(1))?;
        let lpath = format!("{desc_path}/{base}{{0}}");
        self.create_storage(&lpath, CLSID_NETWORK_LOCATOR)?;
        self.write_props(
            &lpath,
            &[(PID_NETWORK_LOCATOR_URL, SF_DATA, encode_utf16_string(url))],
        )
    }
}

/// `DataDefinition` weak reference to the dictionary datadef with `auid`.
fn weak_datadef(auid: &[u8; 16]) -> Vec<u8> {
    encode_weak_ref(DATADEF_REF_INDEX, PID_DEFINITION_IDENTIFICATION, auid)
}

/// Convert nanoseconds to whole samples at `rate` Hz.
fn ns_to_samples(ns: u64, rate: u32) -> i64 {
    ((ns as u128 * rate as u128) / 1_000_000_000u128) as i64
}

fn ns_to_frames(ns: u64, fps: &FrameRate) -> i64 {
    crate::edl::writer::ns_to_frames(ns, fps) as i64
}

/// A media clip eligible for AAF export carries an external source file.
fn is_media_clip(c: &Clip) -> bool {
    !c.source_path.is_empty()
        && matches!(c.kind, ClipKind::Video | ClipKind::Audio | ClipKind::Image | ClipKind::Audition)
}

/// Build the AAF document for `project` as an in-memory byte buffer.
pub fn build_aaf_bytes(project: &Project) -> Result<Vec<u8>> {
    let cf = cfb::CompoundFile::open(Cursor::new(crate::aaf::TEMPLATE_AAF.to_vec()))?;
    let (content, mobs_base) = locate_content_and_mobs(&cf)?;
    let mut b = Builder { cf, content, mobs_base, next_unique: 1, mob_keys: Vec::new() };

    // Project audio edit rate: first audio clip's sample rate, else 48000.
    let audio_rate = project
        .tracks
        .iter()
        .filter(|t| t.is_audio())
        .flat_map(|t| &t.clips)
        .find_map(|c| c.selected_audio_source_stream().and_then(|s| s.sample_rate_hz))
        .unwrap_or(SAMPLE_RATE_DEFAULT);

    // 1) Build one SourceMob per unique source file (audio + video reference).
    //    audio_srcs/video_srcs: source_path -> source-mob MobID.
    let mut audio_srcs: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut video_srcs: BTreeMap<String, Vec<u8>> = BTreeMap::new();

    for track in &project.tracks {
        if track.is_audio() {
            for clip in track.clips.iter().filter(|c| is_media_clip(c)) {
                if !audio_srcs.contains_key(&clip.source_path) {
                    let id = b.build_audio_source_mob(clip, audio_rate)?;
                    audio_srcs.insert(clip.source_path.clone(), id);
                }
            }
        }
    }
    // Flattened video reference = first video track that has media clips.
    let video_track = project
        .tracks
        .iter()
        .find(|t| t.is_video() && t.clips.iter().any(is_media_clip));
    if let Some(vt) = video_track {
        for clip in vt.clips.iter().filter(|c| is_media_clip(c)) {
            if !video_srcs.contains_key(&clip.source_path) {
                let id = b.build_video_source_mob(clip, &project.frame_rate)?;
                video_srcs.insert(clip.source_path.clone(), id);
            }
        }
    }

    // 2) Build the CompositionMob with audio/video/timecode slots.
    b.build_composition(project, audio_rate, &audio_srcs, video_track, &video_srcs)?;

    // 3) Rewrite the Mobs set index with every appended mob.
    let keys = b.mob_keys.clone();
    let idx = encode_set_index(PID_MOB_ID, 32, &keys);
    let mobs_base = b.mobs_base.clone();
    let content = b.content.clone();
    b.write_stream(&format!("{content}/{mobs_base} index"), &idx)?;

    Ok(b.cf.into_inner().into_inner())
}

impl Builder {
    /// Append an audio file SourceMob (PCMDescriptor + NetworkLocator). Returns
    /// its 32-byte MobID. Slot 1 holds a SourceClip over the original essence.
    fn build_audio_source_mob(&mut self, clip: &Clip, rate: u32) -> Result<Vec<u8>> {
        let mob_id = self.next_mob_id();
        let idx = self.mob_keys.len();
        self.mob_keys.push(mob_id.clone());
        let mpath = format!("{}/{}{{{idx}}}", self.content, self.mobs_base);
        let channels = clip.selected_audio_source_stream().map(|s| s.channels).unwrap_or(2).max(1);
        let media_ns = clip.media_duration_ns.unwrap_or(clip.source_out);
        let len = ns_to_samples(media_ns, rate).max(1);

        self.create_storage(&mpath, CLSID_SOURCE_MOB)?;
        self.write_mob_props(&mpath, &mob_id, &clip_name(clip), true)?;

        // Slot 1: SourceClip over the original essence (SourceID = 0).
        let slot0 = format!("{mpath}/Slots-4403{{0}}");
        self.write_stream(&format!("{mpath}/Slots-4403 index"), &encode_vector_index(1))?;
        let seg_props = vec![
            (PID_COMPONENT_DATA_DEFINITION, SF_WEAK_OBJECT_REFERENCE, weak_datadef(&DATADEF_SOUND)),
            (PID_COMPONENT_LENGTH, SF_DATA, encode_i64(len)),
            (PID_SOURCECLIP_SOURCE_ID, SF_DATA, encode_mob_id_zero()),
            (PID_SOURCECLIP_SOURCE_MOB_SLOT_ID, SF_DATA, encode_u32(0)),
            (PID_SOURCECLIP_START_TIME, SF_DATA, encode_i64(0)),
        ];
        self.build_timeline_slot(&slot0, 1, (rate as i32, 1), Some(1), CLSID_SOURCE_CLIP, seg_props)?;

        // PCMDescriptor + NetworkLocator.
        let dpath = format!("{mpath}/EssenceDescription-4701");
        self.create_storage(&dpath, CLSID_PCM_DESCRIPTOR)?;
        self.write_props(
            &dpath,
            &[
                (0x3d01, SF_DATA, encode_u32(16)),                       // QuantizationBits
                (0x3d03, SF_DATA, encode_rational(rate as i32, 1)),      // AudioSamplingRate
                (0x3d07, SF_DATA, encode_u32(channels)),                 // Channels
                (0x3d09, SF_DATA, encode_u32(rate * channels * 2)),      // AverageBPS
                (0x3d0a, SF_DATA, (channels as u16 * 2).to_le_bytes().to_vec()), // BlockAlign u16
                (PID_FILEDESC_SAMPLE_RATE, SF_DATA, encode_rational(rate as i32, 1)),
                (PID_FILEDESC_LENGTH, SF_DATA, encode_i64(len)),
                (
                    PID_FILEDESC_LOCATOR,
                    SF_STRONG_OBJECT_REFERENCE_VECTOR,
                    strong_ref_basename_value("Locator-2f01"),
                ),
            ],
        )?;
        self.build_network_locator(&dpath, &file_uri(&clip.source_path))?;
        Ok(mob_id)
    }

    /// Append a video reference SourceMob (ImportDescriptor + NetworkLocator).
    fn build_video_source_mob(&mut self, clip: &Clip, fps: &FrameRate) -> Result<Vec<u8>> {
        let mob_id = self.next_mob_id();
        let idx = self.mob_keys.len();
        self.mob_keys.push(mob_id.clone());
        let mpath = format!("{}/{}{{{idx}}}", self.content, self.mobs_base);
        let media_ns = clip.media_duration_ns.unwrap_or(clip.source_out);
        let len = ns_to_frames(media_ns, fps).max(1);

        self.create_storage(&mpath, CLSID_SOURCE_MOB)?;
        self.write_mob_props(&mpath, &mob_id, &clip_name(clip), true)?;

        let slot0 = format!("{mpath}/Slots-4403{{0}}");
        self.write_stream(&format!("{mpath}/Slots-4403 index"), &encode_vector_index(1))?;
        let seg_props = vec![
            (PID_COMPONENT_DATA_DEFINITION, SF_WEAK_OBJECT_REFERENCE, weak_datadef(&DATADEF_PICTURE)),
            (PID_COMPONENT_LENGTH, SF_DATA, encode_i64(len)),
            (PID_SOURCECLIP_SOURCE_ID, SF_DATA, encode_mob_id_zero()),
            (PID_SOURCECLIP_SOURCE_MOB_SLOT_ID, SF_DATA, encode_u32(0)),
            (PID_SOURCECLIP_START_TIME, SF_DATA, encode_i64(0)),
        ];
        self.build_timeline_slot(
            &slot0,
            1,
            (fps.numerator as i32, fps.denominator as i32),
            Some(1),
            CLSID_SOURCE_CLIP,
            seg_props,
        )?;

        // ImportDescriptor (no required props) + NetworkLocator.
        let dpath = format!("{mpath}/EssenceDescription-4701");
        self.create_storage(&dpath, CLSID_IMPORT_DESCRIPTOR)?;
        self.write_props(
            &dpath,
            &[(
                PID_FILEDESC_LOCATOR,
                SF_STRONG_OBJECT_REFERENCE_VECTOR,
                strong_ref_basename_value("Locator-2f01"),
            )],
        )?;
        self.build_network_locator(&dpath, &file_uri(&clip.source_path))?;
        Ok(mob_id)
    }

    /// Write the common Mob property entries (MobID, Name, Slots vector,
    /// timestamps; SourceMobs also get EssenceDescription).
    fn write_mob_props(
        &mut self,
        path: &str,
        mob_id: &[u8],
        name: &str,
        is_source: bool,
    ) -> Result<()> {
        let mut entries = vec![
            (PID_MOB_ID, SF_DATA, mob_id.to_vec()),
            (PID_MOB_NAME, SF_DATA, encode_utf16_string(name)),
            (
                PID_MOB_SLOTS,
                SF_STRONG_OBJECT_REFERENCE_VECTOR,
                strong_ref_basename_value("Slots-4403"),
            ),
            (PID_MOB_LAST_MODIFIED, SF_DATA, FIXED_TIMESTAMP.to_vec()),
            (PID_MOB_CREATION_TIME, SF_DATA, FIXED_TIMESTAMP.to_vec()),
        ];
        if is_source {
            entries.push((
                PID_SOURCEMOB_ESSENCE_DESCRIPTION,
                SF_STRONG_OBJECT_REFERENCE,
                strong_ref_basename_value("EssenceDescription-4701"),
            ));
        }
        self.write_props(path, &entries)
    }

    /// Build the CompositionMob with one slot per audio track + a flattened
    /// video reference slot + a timecode slot.
    fn build_composition(
        &mut self,
        project: &Project,
        audio_rate: u32,
        audio_srcs: &BTreeMap<String, Vec<u8>>,
        video_track: Option<&Track>,
        video_srcs: &BTreeMap<String, Vec<u8>>,
    ) -> Result<()> {
        let mob_id = self.next_mob_id();
        let idx = self.mob_keys.len();
        self.mob_keys.push(mob_id.clone());
        let mpath = format!("{}/{}{{{idx}}}", self.content, self.mobs_base);
        self.create_storage(&mpath, CLSID_COMPOSITION_MOB)?;
        self.write_mob_props(&mpath, &mob_id, &project_name(project), false)?;

        // Collect the slots to build: (edit_rate, phys_track, datadef, comps).
        let mut slots: Vec<(i32, i32, Option<u32>, [u8; 16], Vec<Comp>)> = Vec::new();
        let mut phys = 1u32;
        for track in project.tracks.iter().filter(|t| t.is_audio()) {
            let comps =
                sequence_components(track, audio_srcs, |ns| ns_to_samples(ns, audio_rate));
            slots.push((audio_rate as i32, 1, Some(phys), DATADEF_SOUND, comps));
            phys += 1;
        }
        if let Some(vt) = video_track {
            let fps = project.frame_rate.clone();
            let comps = sequence_components(vt, video_srcs, |ns| ns_to_frames(ns, &fps));
            slots.push((
                project.frame_rate.numerator as i32,
                project.frame_rate.denominator as i32,
                None,
                DATADEF_PICTURE,
                comps,
            ));
        }

        // Build Slots vector: each audio/video slot, then a timecode slot.
        let slot_count = slots.len() + 1;
        self.write_stream(
            &format!("{mpath}/Slots-4403 index"),
            &encode_vector_index(slot_count as u32),
        )?;
        let mut slot_no = 0usize;
        let mut slot_id = 1u32;
        for (rn, rd, pt, datadef, comps) in slots {
            let spath = format!("{mpath}/Slots-4403{{{slot_no}}}");
            // Build the slot, then its Sequence segment.
            self.create_storage(&spath, CLSID_TIMELINE_MOB_SLOT)?;
            let mut entries = vec![
                (PID_MOBSLOT_SLOT_ID, SF_DATA, encode_u32(slot_id)),
                (PID_MOBSLOT_SLOT_NAME, SF_DATA, encode_utf16_string("")),
                (
                    PID_MOBSLOT_SEGMENT,
                    SF_STRONG_OBJECT_REFERENCE,
                    strong_ref_basename_value("Segment-4803"),
                ),
            ];
            if let Some(p) = pt {
                entries.push((PID_MOBSLOT_PHYSICAL_TRACK_NUM, SF_DATA, encode_u32(p)));
            }
            entries.push((PID_TIMELINE_EDIT_RATE, SF_DATA, encode_rational(rn, rd)));
            entries.push((PID_TIMELINE_ORIGIN, SF_DATA, encode_i64(0)));
            self.write_props(&spath, &entries)?;
            self.build_sequence(&format!("{spath}/Segment-4803"), &datadef, &comps)?;
            slot_no += 1;
            slot_id += 1;
        }

        // Timecode slot (edit rate = project fps).
        let fps = &project.frame_rate;
        let tc_props = vec![
            (PID_TIMECODE_START, SF_DATA, encode_i64(0)),
            (PID_TIMECODE_FPS, SF_DATA, (fps.as_f64().round() as u16).to_le_bytes().to_vec()),
            (PID_TIMECODE_DROP, SF_DATA, vec![if is_drop_frame(fps) { 1 } else { 0 }]),
            (PID_COMPONENT_DATA_DEFINITION, SF_WEAK_OBJECT_REFERENCE, weak_datadef(&DATADEF_TIMECODE)),
        ];
        let tcpath = format!("{mpath}/Slots-4403{{{slot_no}}}");
        self.build_timeline_slot(
            &tcpath,
            slot_id,
            (fps.numerator as i32, fps.denominator as i32),
            None,
            CLSID_TIMECODE,
            tc_props,
        )?;
        Ok(())
    }
}

/// True for 29.97 / 59.94 (1001-denominator) rates — drop-frame timecode.
fn is_drop_frame(fps: &FrameRate) -> bool {
    crate::edl::writer::is_drop_frame(fps)
}

/// Lay a track's media clips out as Filler-gap + SourceClip components,
/// converting nanosecond positions to edit units with `to_edit`.
fn sequence_components<F: Fn(u64) -> i64>(
    track: &Track,
    srcs: &BTreeMap<String, Vec<u8>>,
    to_edit: F,
) -> Vec<Comp> {
    let mut comps = Vec::new();
    let mut clips: Vec<&Clip> = track.clips.iter().filter(|c| is_media_clip(c)).collect();
    clips.sort_by_key(|c| c.timeline_start);
    let mut cursor = 0u64;
    for clip in clips {
        let Some(source_id) = srcs.get(&clip.source_path) else { continue };
        if clip.timeline_start > cursor {
            let gap = to_edit(clip.timeline_start) - to_edit(cursor);
            if gap > 0 {
                comps.push(Comp::Filler { len: gap });
            }
        }
        let len = (to_edit(clip.timeline_end()) - to_edit(clip.timeline_start)).max(1);
        comps.push(Comp::Clip {
            len,
            source_id: source_id.clone(),
            source_slot: 1,
            start: to_edit(clip.source_in),
        });
        cursor = clip.timeline_end();
    }
    comps
}

fn clip_name(clip: &Clip) -> String {
    if clip.label.is_empty() {
        std::path::Path::new(&clip.source_path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "clip".to_string())
    } else {
        clip.label.clone()
    }
}

fn project_name(project: &Project) -> String {
    if project.title.is_empty() {
        "UltimateSlice Sequence".to_string()
    } else {
        project.title.clone()
    }
}

/// Build a `file://` URL for `path` (percent-encoding non-unreserved bytes,
/// preserving `/`). Mirrors `pathlib.Path(path).as_uri()`.
fn file_uri(path: &str) -> String {
    let mut out = String::from("file://");
    for &b in path.as_bytes() {
        match b {
            b'/' | b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Locate the ContentStorage path and the `Mobs` set base name in the template.
fn locate_content_and_mobs(cf: &Cf) -> Result<(String, String)> {
    let header = cf
        .read_storage("/")?
        .find(|e| e.is_storage() && e.name().starts_with("Header"))
        .ok_or_else(|| anyhow!("AAF template: no Header storage"))?
        .name()
        .to_string();
    let header_path = format!("/{header}");
    let content = cf
        .read_storage(&header_path)?
        .find(|e| e.is_storage() && e.name().starts_with("Content"))
        .ok_or_else(|| anyhow!("AAF template: no ContentStorage"))?
        .name()
        .to_string();
    let content_path = format!("{header_path}/{content}");
    let mobs_idx = cf
        .read_storage(&content_path)?
        .find(|e| e.is_stream() && e.name().starts_with("Mobs") && e.name().ends_with(" index"))
        .ok_or_else(|| anyhow!("AAF template: no Mobs index"))?
        .name()
        .to_string();
    let mobs_base = mobs_idx.trim_end_matches(" index").to_string();
    Ok((content_path, mobs_base))
}

/// Export `project` to an AAF file at `path`.
pub fn write_aaf(project: &Project, path: &Path) -> Result<()> {
    let bytes = build_aaf_bytes(project)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::track::Track;

    fn sample_project() -> Project {
        let mut p = Project::new("AAF Test");
        p.tracks.clear();
        let mut a = Track::new_audio("A1");
        // Two clips with a gap between them.
        a.add_clip(Clip::new("/tmp/ref_audio.wav", 5_000_000_000, 0, ClipKind::Audio));
        a.add_clip(Clip::new(
            "/tmp/ref_audio.wav",
            3_000_000_000,
            6_000_000_000,
            ClipKind::Audio,
        ));
        p.tracks.push(a);
        let mut v = Track::new_video("V1");
        v.add_clip(Clip::new("/tmp/ref_video.mov", 4_000_000_000, 0, ClipKind::Video));
        p.tracks.push(v);
        p
    }

    fn mobs_count(bytes: &[u8]) -> u32 {
        let mut cf = cfb::CompoundFile::open(Cursor::new(bytes.to_vec())).unwrap();
        let (content, base) = locate_content_and_mobs(&cf).unwrap();
        let mut s = cf.open_stream(format!("{content}/{base} index")).unwrap();
        let mut buf = [0u8; 4];
        std::io::Read::read_exact(&mut s, &mut buf).unwrap();
        u32::from_le_bytes(buf)
    }

    #[test]
    fn builds_reopenable_aaf_with_expected_mobs() {
        let bytes = build_aaf_bytes(&sample_project()).unwrap();
        // 1 audio source mob (deduped) + 1 video source mob + 1 composition.
        assert_eq!(mobs_count(&bytes), 3);
        // Reopens cleanly and the composition storages exist.
        let cf = cfb::CompoundFile::open(Cursor::new(bytes.clone())).unwrap();
        let (content, base) = locate_content_and_mobs(&cf).unwrap();
        // The composition is the last appended mob (index 2).
        assert!(cf.exists(format!("{content}/{base}{{2}}/properties")));
        // Write to a temp path for external pyaaf2 validation (see
        // tools/validate_aaf.py).
        let _ = std::fs::write(std::env::temp_dir().join("us_aaf_test.aaf"), &bytes);
    }

    #[test]
    fn empty_project_still_builds_one_composition() {
        let mut p = Project::new("Empty");
        p.tracks.clear();
        let bytes = build_aaf_bytes(&p).unwrap();
        // Just the (empty) composition mob.
        assert_eq!(mobs_count(&bytes), 1);
    }
}
