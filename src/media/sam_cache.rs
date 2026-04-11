// SPDX-License-Identifier: GPL-3.0-or-later
//! Segment Anything Model 3.1 (Meta) install detection and shared paths.
//!
//! Phase 1 scope: **pure infrastructure.** This module answers the
//! question "is the SAM 3.1 model installed on this machine, and if
//! not, where does the user put the files?" so the Preferences
//! Models page can show a meaningful install-status row. It does
//! **not** load ONNX sessions or run inference — that lands in
//! Phase 2 alongside the first user-visible feature ("Generate mask
//! with SAM" in the Inspector), where the session-loading code gets
//! a real consumer and we can test it end-to-end.
//!
//! ## Model layout
//!
//! SAM 3 exports via [SAMExporter] produce three ONNX files, plus an
//! optional tokenizer. We expect them colocated in
//! `$XDG_DATA_HOME/ultimateslice/models/sam3/`:
//!
//! ```text
//!   sam3/
//!     sam3_image_encoder.onnx       (~600 MB — the bulk of the model)
//!     sam3_language_encoder.onnx    (~300 MB — for text / exemplar prompts)
//!     sam3_decoder.onnx             (~10 MB — mask decoder)
//! ```
//!
//! The total install is ~1 GB (fp16) depending on which SAM 3.1
//! checkpoint the user exported. All three files are required
//! because SAM 3 is a unified model: the image encoder produces
//! embeddings consumed by the decoder; the text encoder produces
//! the prompt embedding alternative to point/box prompts. If any
//! one is missing the model can't run.
//!
//! ## Why no session loading here
//!
//! The original Phase 1 plan included `SamSessions` + a low-level
//! `segment_image` function, but shipping that code now without a
//! caller would:
//!
//! 1. generate dead-code warnings we'd paper over with `#[allow]`,
//! 2. require me to commit to specific ONNX input/output tensor
//!    shapes for the SAM 3 decoder before I've verified them
//!    empirically against a real checkpoint, and
//! 3. lock in an inference API design that might not fit Phase 2's
//!    interactive click-to-mask use case.
//!
//! Phase 2 adds both the session-loading plumbing (routed through
//! `ai_providers::configure_session_builder` so GPU acceleration
//! works) and the user-visible consumer in the same change.
//!
//! [SAMExporter]: https://anylabeling.nrl.ai/docs/samexporter

#![cfg(feature = "ai-inference")]

use std::path::{Path, PathBuf};

// ── Model file layout ──────────────────────────────────────────────────────

/// Filename of the image encoder ONNX file inside the SAM model dir.
/// Matches the convention used by `wkentaro/sam3-onnx` — the most
/// recent SAM 3-specific export project — and by the `sam3_*`
/// variant of `vietanhdev/samexporter`. If a user's exporter
/// produces files without the `sam3_` prefix (some older SAM export
/// tools drop it), they need to rename the files to match the
/// constants in this module.
pub const IMAGE_ENCODER_FILENAME: &str = "sam3_image_encoder.onnx";
/// Filename of the language (text) encoder ONNX file inside the SAM
/// model dir. Named `language_encoder` rather than `text_encoder`
/// to match the upstream convention — SAM 3 accepts text *and*
/// exemplar image prompts through the same encoder path.
pub const LANGUAGE_ENCODER_FILENAME: &str = "sam3_language_encoder.onnx";
/// Filename of the mask decoder ONNX file inside the SAM model dir.
pub const DECODER_FILENAME: &str = "sam3_decoder.onnx";

/// All files that must be present for SAM to be usable. Order is
/// fixed so `install_status` reports missing files in a stable,
/// deterministic order for the Preferences UI.
///
/// Note: SAM 3's ONNX export uses ONNX external-data format for the
/// image encoder because its weight tensor exceeds the 2 GB protobuf
/// message size limit. Each `.onnx` file therefore has an associated
/// `.onnx.data` sidecar that ort loads automatically at session
/// creation time when it parses the `.onnx`. Install detection only
/// checks the main `.onnx` files — ort surfaces missing-sidecar
/// errors at session creation time in Phase 2, which is a better
/// place to report them than a hard-coded sidecar probe here (some
/// exporters may choose not to use external data for smaller
/// variants).
pub const REQUIRED_FILES: &[&str] = &[
    IMAGE_ENCODER_FILENAME,
    LANGUAGE_ENCODER_FILENAME,
    DECODER_FILENAME,
];

/// Glob-ish prefix used to find raw PyTorch checkpoints. Matches
/// `sam3.1_multiplex.pt`, `sam3_multiplex.pt`, `sam3.pt`, etc. —
/// anything in the install directory starting with `sam3` and
/// ending in `.pt`. Used by [`install_status`] to detect the
/// intermediate "user downloaded the official Meta distribution
/// but hasn't exported it to ONNX yet" state.
pub const PT_CHECKPOINT_PREFIX: &str = "sam3";
pub const PT_CHECKPOINT_EXTENSION: &str = "pt";

/// User-facing display name for the model. Kept here so the
/// Preferences row and the roadmap / MCP / future log messages share
/// one canonical spelling.
pub const DISPLAY_NAME: &str = "Segment Anything 3.1 (Meta)";

/// Short one-line license + size summary shown in the Preferences
/// hint text.
pub const LICENSE_SUMMARY: &str = "Apache-2.0, ~1 GB install (fp16)";

/// The upstream SAM 3.1 repository. Shown in the Preferences hint so
/// the user can find checkpoint download links and ONNX export
/// instructions.
pub const UPSTREAM_URL: &str = "https://github.com/facebookresearch/sam3";

/// The ONNX export tool we recommend. Documented because the raw
/// PyTorch-to-ONNX step is non-trivial and we don't want users
/// re-deriving it from scratch.
pub const EXPORTER_PIP_NAME: &str = "samexporter";
pub const EXPORTER_UPSTREAM_URL: &str = "https://github.com/vietanhdev/samexporter";

// ── Path resolution ────────────────────────────────────────────────────────

/// Canonical install directory for the SAM model files.
///
/// Follows the same `$XDG_DATA_HOME/ultimateslice/models/<name>/`
/// convention as MusicGen (`music_gen::model_install_dir`) and RIFE
/// (`frame_interp_cache::model_install_dir`). Users who don't have
/// `XDG_DATA_HOME` set fall back to `~/.local/share/`.
pub fn model_install_dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(home).join(".local/share")
        });
    base.join("ultimateslice/models/sam3")
}

/// Resolved absolute paths to all three SAM model ONNX files.
/// Returned by [`find_sam_model_paths`] when the full set is
/// present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SamModelPaths {
    pub image_encoder: PathBuf,
    /// The language (text + exemplar image) encoder. Named
    /// `language_encoder` to match the upstream SAM 3 convention —
    /// SAM 3 accepts both text prompts and exemplar image prompts
    /// through the same encoder path.
    pub language_encoder: PathBuf,
    pub decoder: PathBuf,
}

impl SamModelPaths {
    /// Build a `SamModelPaths` anchored at the given directory,
    /// without checking whether the files exist. Used internally by
    /// [`find_sam_model_paths`] and by tests that need to reason
    /// about expected layouts.
    pub fn from_dir(dir: &Path) -> Self {
        Self {
            image_encoder: dir.join(IMAGE_ENCODER_FILENAME),
            language_encoder: dir.join(LANGUAGE_ENCODER_FILENAME),
            decoder: dir.join(DECODER_FILENAME),
        }
    }

    /// Iterator over all three paths in canonical order (image
    /// encoder, language encoder, decoder). Mostly useful for tests
    /// and for install-status checks.
    pub fn all_paths(&self) -> [&Path; 3] {
        [
            self.image_encoder.as_path(),
            self.language_encoder.as_path(),
            self.decoder.as_path(),
        ]
    }
}

/// Probe the standard install locations for the SAM model files and
/// return `Some(paths)` if and only if the full set is present at
/// one of them. Returns `None` if any file is missing anywhere.
///
/// Search order matches the other AI caches so devs hacking on the
/// app see the same resolution order for every model:
///
/// 1. `<exe_dir>/data/models/sam3/` — next to a built binary
/// 2. `data/models/sam3/` — development working directory
/// 3. `/app/share/ultimateslice/models/sam3/` — Flatpak layout
/// 4. `$XDG_DATA_HOME/ultimateslice/models/sam3/` — user install
///
/// Phase 1 doesn't consume this function directly — the Preferences
/// row uses [`install_status`] for its richer "partial install"
/// reporting. It exists in Phase 1 so its signature is stable
/// before the Phase 2 session-loading code (which uses it to get a
/// concrete `SamModelPaths` to feed into three `Session::builder`
/// calls) lands.
#[allow(dead_code)]
pub fn find_sam_model_paths() -> Option<SamModelPaths> {
    for dir in candidate_dirs() {
        let paths = SamModelPaths::from_dir(&dir);
        if paths.all_paths().iter().all(|p| p.is_file()) {
            log::info!("SamCache: found SAM 3 model at {}", dir.display());
            return Some(paths);
        }
    }
    None
}

fn candidate_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    // Next to executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.join("data/models/sam3"));
        }
    }
    // Development CWD.
    dirs.push(PathBuf::from("data/models/sam3"));
    // Flatpak.
    dirs.push(PathBuf::from("/app/share/ultimateslice/models/sam3"));
    // XDG data home (the canonical user install).
    dirs.push(model_install_dir());
    dirs
}

// ── Install status ─────────────────────────────────────────────────────────

/// What the Preferences Models page shows in the SAM row's status
/// column. More granular than a plain `Option<SamModelPaths>` so the
/// UI can tell the user "2 of 3 files found" or "PyTorch checkpoint
/// detected but not yet exported to ONNX" when it's true.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SamInstallStatus {
    /// All three ONNX files present at one of the search locations.
    /// The model is ready to load.
    Installed,
    /// The install directory exists and contains at least one — but
    /// not all — of the required files. The `missing` list names the
    /// files that still need to be downloaded, in canonical order,
    /// so the UI can surface exactly what's still needed.
    Partial { missing: Vec<String> },
    /// A raw PyTorch checkpoint (`sam3*.pt`) is present at one of
    /// the candidate install directories, but none of the required
    /// ONNX files are there yet. This is the state a user lands in
    /// immediately after downloading from `facebookresearch/sam3`
    /// or the HuggingFace Meta org — the checkpoint is authoritative
    /// but `ort` can't load it directly, so the Preferences UI
    /// surfaces explicit `samexporter` run instructions pointing at
    /// the detected `pt_path`.
    PtCheckpointOnly { pt_path: PathBuf },
    /// No SAM install directory found at any candidate location.
    NotInstalled,
}

impl SamInstallStatus {
    /// Short one-line status for the Preferences row status label.
    pub fn short_label(&self) -> String {
        match self {
            SamInstallStatus::Installed => "✓ Installed".to_string(),
            SamInstallStatus::Partial { missing } => {
                let have = REQUIRED_FILES.len() - missing.len();
                format!("⚠ Partial ({}/{} files)", have, REQUIRED_FILES.len())
            }
            SamInstallStatus::PtCheckpointOnly { .. } => {
                "⚠ PyTorch checkpoint — needs ONNX export".to_string()
            }
            SamInstallStatus::NotInstalled => "Not installed".to_string(),
        }
    }

    /// True if the model is fully installed and ready to load.
    /// Consumed by Phase 2's "Generate with SAM" button to decide
    /// whether to enable the action; unused in Phase 1 but
    /// deliberately kept public so its API is stable before that
    /// consumer lands.
    #[allow(dead_code)]
    pub fn is_ready(&self) -> bool {
        matches!(self, SamInstallStatus::Installed)
    }
}

/// Scan a single directory for any `sam3*.pt` PyTorch checkpoint
/// files. Returns the first matching path (by filesystem order — no
/// guarantee of stability, but stable enough for a "one checkpoint
/// per directory" convention). Returns `None` if the directory
/// doesn't exist or contains no matching files.
fn find_pt_checkpoint_in(dir: &Path) -> Option<PathBuf> {
    let read_dir = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return None,
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with(PT_CHECKPOINT_PREFIX)
            && name.ends_with(&format!(".{PT_CHECKPOINT_EXTENSION}"))
        {
            return Some(path);
        }
    }
    None
}

/// Walk the candidate directories and build a detailed install
/// status report. Priority order:
///
///   1. **Installed** — any candidate dir has all three ONNX files.
///      Returned immediately on the first hit.
///   2. **Partial** — the candidate dir with the most ONNX files
///      (but fewer than three). Reported so a user mid-download
///      sees exactly what's still missing. A sibling `.pt` file at
///      the same dir is treated as already-accounted-for — we show
///      the ONNX progress, not the checkpoint.
///   3. **PtCheckpointOnly** — no ONNX files anywhere, but a raw
///      `sam3*.pt` checkpoint is present at one of the candidate
///      dirs. The UI uses this to surface the ONNX export step
///      with a concrete pointer at the detected checkpoint path.
///   4. **NotInstalled** — nothing at all.
pub fn install_status() -> SamInstallStatus {
    let candidates = candidate_dirs();

    let mut best_partial: Option<(usize, Vec<String>)> = None;
    let mut pt_checkpoint: Option<PathBuf> = None;

    for dir in &candidates {
        let paths = SamModelPaths::from_dir(dir);
        let present_count = paths
            .all_paths()
            .iter()
            .filter(|p| p.is_file())
            .count();

        if present_count == REQUIRED_FILES.len() {
            return SamInstallStatus::Installed;
        }

        if present_count > 0 {
            // Collect the *missing* filenames in canonical order.
            let missing: Vec<String> = REQUIRED_FILES
                .iter()
                .filter(|f| !dir.join(f).is_file())
                .map(|f| (*f).to_string())
                .collect();
            // Prefer the candidate with the most ONNX files already
            // in place — that's almost certainly where the user is
            // in the middle of installing.
            let beats_current = best_partial
                .as_ref()
                .map(|(count, _)| present_count > *count)
                .unwrap_or(true);
            if beats_current {
                best_partial = Some((present_count, missing));
            }
        }

        // Track the first `.pt` checkpoint we find across candidate
        // dirs. We only surface this if no ONNX files exist
        // anywhere (Partial takes precedence — a user who has
        // started exporting clearly already knows about the .pt
        // file).
        if pt_checkpoint.is_none() {
            pt_checkpoint = find_pt_checkpoint_in(dir);
        }
    }

    if let Some((_, missing)) = best_partial {
        return SamInstallStatus::Partial { missing };
    }
    if let Some(pt_path) = pt_checkpoint {
        return SamInstallStatus::PtCheckpointOnly { pt_path };
    }
    SamInstallStatus::NotInstalled
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Helper: create zero-byte placeholder files at the SAM layout
    /// inside `dir`. Returns the paths that were created.
    fn touch_required_files(dir: &Path, which: &[&str]) {
        for name in which {
            let path = dir.join(name);
            fs::write(&path, b"").unwrap();
        }
    }

    #[test]
    fn sam_model_paths_from_dir_builds_expected_layout() {
        let dir = Path::new("/tmp/sam3-test");
        let paths = SamModelPaths::from_dir(dir);
        assert_eq!(paths.image_encoder, dir.join(IMAGE_ENCODER_FILENAME));
        assert_eq!(paths.language_encoder, dir.join(LANGUAGE_ENCODER_FILENAME));
        assert_eq!(paths.decoder, dir.join(DECODER_FILENAME));
        assert_eq!(paths.all_paths().len(), 3);
    }

    #[test]
    fn sam_filenames_use_sam3_prefix() {
        // Guard the filename convention. These names must match
        // what the upstream export tools (wkentaro/sam3-onnx,
        // vietanhdev/samexporter) actually produce; renaming them
        // is a breaking change from the user's perspective because
        // they have to rename files on disk to match.
        assert!(IMAGE_ENCODER_FILENAME.starts_with("sam3_"));
        assert!(LANGUAGE_ENCODER_FILENAME.starts_with("sam3_"));
        assert!(DECODER_FILENAME.starts_with("sam3_"));
        assert!(IMAGE_ENCODER_FILENAME.ends_with(".onnx"));
        assert!(LANGUAGE_ENCODER_FILENAME.ends_with(".onnx"));
        assert!(DECODER_FILENAME.ends_with(".onnx"));
    }

    #[test]
    fn sam_required_files_matches_expected_count() {
        // Regression guard: if a future PR adds a fourth required
        // file, tests exercising partial counts need to be updated
        // too.
        assert_eq!(REQUIRED_FILES.len(), 3);
        assert!(REQUIRED_FILES.contains(&IMAGE_ENCODER_FILENAME));
        assert!(REQUIRED_FILES.contains(&LANGUAGE_ENCODER_FILENAME));
        assert!(REQUIRED_FILES.contains(&DECODER_FILENAME));
    }

    #[test]
    fn install_status_short_labels_round_trip() {
        assert_eq!(SamInstallStatus::Installed.short_label(), "✓ Installed");
        assert_eq!(
            SamInstallStatus::NotInstalled.short_label(),
            "Not installed"
        );
        let partial = SamInstallStatus::Partial {
            missing: vec![DECODER_FILENAME.to_string()],
        };
        // 3 - 1 missing = 2 present.
        assert_eq!(partial.short_label(), "⚠ Partial (2/3 files)");
        assert!(SamInstallStatus::Installed.is_ready());
        assert!(!partial.is_ready());
        assert!(!SamInstallStatus::NotInstalled.is_ready());
    }

    #[test]
    fn model_install_dir_ends_in_sam3() {
        let dir = model_install_dir();
        assert!(
            dir.ends_with("ultimateslice/models/sam3"),
            "expected install dir to end in ultimateslice/models/sam3, got {}",
            dir.display()
        );
    }

    // The remaining tests synthesize a fake SAM install under a
    // tempdir and exercise the file-existence logic directly on
    // `SamModelPaths` and by building a bespoke `install_status`
    // that probes that tempdir. We can't easily hijack the real
    // candidate-dirs search without env manipulation (XDG_DATA_HOME
    // is process-global and other tests run in parallel), so the
    // tests below validate the pure logic rather than
    // `install_status()` itself.

    #[test]
    fn sam_model_paths_all_files_present_detected_via_is_file() {
        let tmp = TempDir::new().unwrap();
        let paths = SamModelPaths::from_dir(tmp.path());
        // Before any files exist, none of the paths are files.
        assert!(paths.all_paths().iter().all(|p| !p.is_file()));
        // Write all three.
        touch_required_files(tmp.path(), REQUIRED_FILES);
        assert!(paths.all_paths().iter().all(|p| p.is_file()));
    }

    #[test]
    fn sam_model_paths_partial_install_detected() {
        let tmp = TempDir::new().unwrap();
        let paths = SamModelPaths::from_dir(tmp.path());
        // Two of three files written: image encoder + decoder, no
        // language encoder.
        touch_required_files(tmp.path(), &[IMAGE_ENCODER_FILENAME, DECODER_FILENAME]);
        assert!(paths.image_encoder.is_file());
        assert!(!paths.language_encoder.is_file());
        assert!(paths.decoder.is_file());

        // Simulate the install_status reducer against this single
        // candidate: count present files, list missing ones.
        let present_count = paths.all_paths().iter().filter(|p| p.is_file()).count();
        assert_eq!(present_count, 2);
        let missing: Vec<String> = REQUIRED_FILES
            .iter()
            .filter(|f| !tmp.path().join(f).is_file())
            .map(|f| (*f).to_string())
            .collect();
        assert_eq!(missing, vec![LANGUAGE_ENCODER_FILENAME.to_string()]);

        let status = SamInstallStatus::Partial {
            missing: missing.clone(),
        };
        assert_eq!(status.short_label(), "⚠ Partial (2/3 files)");
        assert!(!status.is_ready());
    }

    #[test]
    fn sam_model_paths_empty_dir_is_not_installed() {
        let tmp = TempDir::new().unwrap();
        let paths = SamModelPaths::from_dir(tmp.path());
        let present_count = paths.all_paths().iter().filter(|p| p.is_file()).count();
        assert_eq!(present_count, 0);
    }

    #[test]
    fn sam_install_status_not_installed_when_no_candidate_exists() {
        // This *does* call the real `install_status()` — but we
        // expect it to return NotInstalled on a CI box with no SAM
        // files installed. If a developer has SAM installed
        // locally, this test will (correctly) detect that and
        // assert the installed status is one of the two "has
        // something" variants.
        let status = install_status();
        match status {
            SamInstallStatus::NotInstalled => {
                // Expected on CI and most dev machines.
            }
            SamInstallStatus::Partial { missing } => {
                // Developer has a partial install. Validate the
                // missing list is non-empty and only contains known
                // required filenames.
                assert!(!missing.is_empty());
                for name in &missing {
                    assert!(
                        REQUIRED_FILES.contains(&name.as_str()),
                        "missing file {name} is not in REQUIRED_FILES"
                    );
                }
            }
            SamInstallStatus::Installed => {
                // Developer already installed SAM — nothing to
                // check; the happy path works.
            }
            SamInstallStatus::PtCheckpointOnly { pt_path } => {
                // Developer has the raw checkpoint sitting in the
                // install dir but hasn't exported it yet. Validate
                // that the detected path actually exists and
                // matches the sam3*.pt convention.
                assert!(pt_path.is_file(), "detected pt path must exist");
                let name = pt_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                assert!(
                    name.starts_with(PT_CHECKPOINT_PREFIX),
                    "pt path {} does not start with expected prefix",
                    name
                );
                assert!(
                    name.ends_with(&format!(".{PT_CHECKPOINT_EXTENSION}")),
                    "pt path {name} does not have expected extension"
                );
            }
        }
    }

    #[test]
    fn pt_checkpoint_only_status_rendered_by_short_label() {
        let status = SamInstallStatus::PtCheckpointOnly {
            pt_path: PathBuf::from("/fake/sam3.1_multiplex.pt"),
        };
        assert_eq!(
            status.short_label(),
            "⚠ PyTorch checkpoint — needs ONNX export"
        );
        // Not ready to load — still needs the export step.
        assert!(!status.is_ready());
    }

    #[test]
    fn find_pt_checkpoint_in_detects_sam3_variants() {
        let tmp = TempDir::new().unwrap();
        // Empty dir → no checkpoint.
        assert!(find_pt_checkpoint_in(tmp.path()).is_none());

        // Non-matching file → still no checkpoint.
        fs::write(tmp.path().join("unrelated.bin"), b"").unwrap();
        assert!(find_pt_checkpoint_in(tmp.path()).is_none());

        // Canonical multiplex filename → detected.
        let expected = tmp.path().join("sam3.1_multiplex.pt");
        fs::write(&expected, b"").unwrap();
        let found = find_pt_checkpoint_in(tmp.path()).expect("should detect sam3.1_multiplex.pt");
        assert_eq!(found, expected);
    }

    #[test]
    fn find_pt_checkpoint_in_matches_alternative_sam3_filenames() {
        // The app should match any `sam3*.pt` file so different
        // checkpoint variants (sam3.pt, sam3_large.pt, etc.) work
        // without users having to rename their downloads.
        for filename in &[
            "sam3.pt",
            "sam3_large.pt",
            "sam3_multiplex.pt",
            "sam3.1.pt",
        ] {
            let tmp = TempDir::new().unwrap();
            let path = tmp.path().join(filename);
            fs::write(&path, b"").unwrap();
            let found = find_pt_checkpoint_in(tmp.path())
                .unwrap_or_else(|| panic!("should detect {filename}"));
            assert_eq!(found, path);
        }
    }

    #[test]
    fn find_pt_checkpoint_in_ignores_non_sam3_pickles() {
        // A stray PyTorch pickle that isn't SAM should not be
        // detected as a SAM checkpoint.
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("rife.pkl"), b"").unwrap();
        fs::write(tmp.path().join("flownet.pt"), b"").unwrap();
        assert!(find_pt_checkpoint_in(tmp.path()).is_none());
    }
}
