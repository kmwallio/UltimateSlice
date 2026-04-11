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
//!     image_encoder.onnx   (~600 MB — the bulk of the model)
//!     text_encoder.onnx    (~300 MB — for text prompts)
//!     decoder.onnx         (~10 MB — mask decoder)
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
pub const IMAGE_ENCODER_FILENAME: &str = "image_encoder.onnx";
/// Filename of the text encoder ONNX file inside the SAM model dir.
pub const TEXT_ENCODER_FILENAME: &str = "text_encoder.onnx";
/// Filename of the mask decoder ONNX file inside the SAM model dir.
pub const DECODER_FILENAME: &str = "decoder.onnx";

/// All files that must be present for SAM to be usable. Order is
/// fixed so `install_status` reports missing files in a stable,
/// deterministic order for the Preferences UI.
pub const REQUIRED_FILES: &[&str] = &[
    IMAGE_ENCODER_FILENAME,
    TEXT_ENCODER_FILENAME,
    DECODER_FILENAME,
];

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
    pub text_encoder: PathBuf,
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
            text_encoder: dir.join(TEXT_ENCODER_FILENAME),
            decoder: dir.join(DECODER_FILENAME),
        }
    }

    /// Iterator over all three paths in canonical order (image
    /// encoder, text encoder, decoder). Mostly useful for tests and
    /// for install-status checks.
    pub fn all_paths(&self) -> [&Path; 3] {
        [
            self.image_encoder.as_path(),
            self.text_encoder.as_path(),
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
/// UI can tell the user "2 of 3 files found" when a download got
/// partway through.
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

/// Walk the candidate directories and build a detailed install
/// status report. Returns `Installed` as soon as it finds a complete
/// set; otherwise reports on the best-populated candidate directory
/// (the one with the *most* files present), so a user who started a
/// download in the XDG location gets a meaningful "2 of 3 files
/// found" message pointing at the right place.
pub fn install_status() -> SamInstallStatus {
    let candidates = candidate_dirs();
    let mut best_partial: Option<(usize, Vec<String>)> = None;
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
            // Prefer the candidate with the most files already in
            // place — that's almost certainly where the user is in
            // the middle of installing.
            let beats_current = best_partial
                .as_ref()
                .map(|(count, _)| present_count > *count)
                .unwrap_or(true);
            if beats_current {
                best_partial = Some((present_count, missing));
            }
        }
    }
    match best_partial {
        Some((_, missing)) => SamInstallStatus::Partial { missing },
        None => SamInstallStatus::NotInstalled,
    }
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
        assert_eq!(paths.text_encoder, dir.join(TEXT_ENCODER_FILENAME));
        assert_eq!(paths.decoder, dir.join(DECODER_FILENAME));
        assert_eq!(paths.all_paths().len(), 3);
    }

    #[test]
    fn sam_required_files_matches_expected_count() {
        // Regression guard: if a future PR adds a fourth required
        // file, tests exercising partial counts need to be updated
        // too.
        assert_eq!(REQUIRED_FILES.len(), 3);
        assert!(REQUIRED_FILES.contains(&IMAGE_ENCODER_FILENAME));
        assert!(REQUIRED_FILES.contains(&TEXT_ENCODER_FILENAME));
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
        // Two of three files written: encoder + decoder, no text encoder.
        touch_required_files(tmp.path(), &[IMAGE_ENCODER_FILENAME, DECODER_FILENAME]);
        assert!(paths.image_encoder.is_file());
        assert!(!paths.text_encoder.is_file());
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
        assert_eq!(missing, vec![TEXT_ENCODER_FILENAME.to_string()]);

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
        }
    }
}
