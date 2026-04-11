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

// ── Inference backend ──────────────────────────────────────────────────────
//
// The constants and types below are the low-level inference API that
// Phase 2b's Inspector / Program Monitor UI (and the MCP
// `generate_sam_mask` tool) call into. All tensor shape and input
// name decisions are anchored to the actual ONNX signatures of the
// real SAM 3 ViT-H export at `~/.local/share/ultimateslice/models/
// sam3/` — verified empirically during Phase 2a bring-up. If a
// different SAM 3 export variant (different input size, different
// decoder input names) is ever supported, those differences live
// here and not scattered across the caller sites.

use ndarray::{Array1, Array2, Array3, Array4};

/// Input image resolution expected by the SAM 3 ViT-H image
/// encoder. Matches `config.yaml:input_size` shipped alongside the
/// ONNX files — the encoder's input tensor is literally
/// `image: [3, 1008, 1008] uint8`, no batch dimension. Changing this
/// without a matching model swap produces a shape-mismatch error at
/// session run time.
pub const SAM_INPUT_SIZE: usize = 1008;

/// Language embedding dimensions. The decoder requires
/// `language_mask: [1, 32] bool` and `language_features: [32, 1, 256]
/// float` as inputs regardless of whether text prompts are being
/// used — for box-only workflows we feed zero/false tensors, which
/// tells the decoder to fall back to pure box-prompt operation.
/// Phase 2 intentionally doesn't invoke the language encoder at all,
/// which sidesteps the missing-sidecar problem on the user's current
/// install.
const LANG_TOKENS: usize = 32;
const LANG_EMBED_DIM: usize = 256;

// ── Prompt + result types ──────────────────────────────────────────────────

/// A single box prompt in **source-image pixel coordinates**. The
/// `segment_with_box` entry point rescales into the encoder's 1008×1008
/// space internally so callers don't need to know about the
/// preprocessing transform. Coordinates can be out of order
/// (x2 < x1 is fine — we normalize before use), and are clamped to
/// the source image bounds.
///
/// For the "click = tiny box" emulated-point workflow, callers should
/// construct a BoxPrompt with a small square (e.g. 8×8 px) centered
/// on the click. See `BoxPrompt::point_emulation` for the helper.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxPrompt {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

impl BoxPrompt {
    /// Build a box prompt from two arbitrary corners. The resulting
    /// box always has x1 <= x2 and y1 <= y2.
    pub fn from_corners(ax: f32, ay: f32, bx: f32, by: f32) -> Self {
        Self {
            x1: ax.min(bx),
            y1: ay.min(by),
            x2: ax.max(bx),
            y2: ay.max(by),
        }
    }

    /// Construct a small square box centered on a click point, used
    /// to emulate a point prompt against SAM 3's box-prompt-only
    /// decoder interface. `half_size` is measured in source pixels —
    /// the resulting box is `2 * half_size` on each side.
    ///
    /// 8 px (half_size=4) is a reasonable default: small enough that
    /// SAM treats it as a point-like signal, large enough to survive
    /// the 1008-space rescale without collapsing to a zero-area box.
    pub fn point_emulation(cx: f32, cy: f32, half_size: f32) -> Self {
        Self {
            x1: cx - half_size,
            y1: cy - half_size,
            x2: cx + half_size,
            y2: cy + half_size,
        }
    }

    /// Clamp all four corners into the source-image bounds. Called
    /// internally before the 1008-space rescale to guarantee the
    /// decoder never sees out-of-frame coordinates.
    fn clamp_to_source(self, src_w: usize, src_h: usize) -> Self {
        let max_x = (src_w as f32 - 1.0).max(0.0);
        let max_y = (src_h as f32 - 1.0).max(0.0);
        Self {
            x1: self.x1.clamp(0.0, max_x),
            y1: self.y1.clamp(0.0, max_y),
            x2: self.x2.clamp(0.0, max_x),
            y2: self.y2.clamp(0.0, max_y),
        }
    }
}

/// Result of running SAM on a single frame with one box prompt.
/// The mask is returned at **source image resolution** (i.e. already
/// un-padded and rescaled from the 1008-space output) as a flat
/// `src_w * src_h` row-major buffer of 0/1 bytes, which is the
/// easiest shape for `mask_contour` and `BgRemovalCache`-style
/// downstream consumers to work with.
pub struct SegmentResult {
    /// Binary mask, row-major, 0 = background, 255 = foreground.
    /// Size is always `src_w * src_h` — the caller doesn't need to
    /// know about the 1008-space intermediate.
    pub mask: Vec<u8>,
    pub src_w: usize,
    pub src_h: usize,
    /// SAM's confidence score for this mask. Higher is better.
    /// Phase 2b uses this to dim the Inspector "apply" button when
    /// confidence is low, or to pick the best of multiple output
    /// masks when the decoder returns several.
    pub score: f32,
}

// ── Preprocessing ──────────────────────────────────────────────────────────

/// Intermediate representation of a source frame after aspect-ratio-
/// preserving resize + pad to 1008×1008. Keeps enough metadata so
/// that segment_with_box can map mask coordinates back to the
/// original source-image space.
struct PreprocessedImage {
    /// CHW uint8 tensor of shape [3, 1008, 1008]. The image encoder
    /// input is `image: [3, 1008, 1008] uint8` — SAM does its own
    /// per-channel mean/std normalization internally, so we feed raw
    /// pixel bytes and never touch float32 on the encoder path.
    tensor: Array3<u8>,
    /// Scale factor that was applied: 1008 / max(src_w, src_h).
    /// Used to convert box prompts from source-pixel coords into the
    /// 1008-space the decoder expects, and to un-scale output masks
    /// back to source coords.
    scale: f32,
    /// Width of the content region (before padding) inside the
    /// 1008×1008 tensor. Everything at `x >= padded_w` is zero pad.
    padded_w: usize,
    /// Height of the content region (before padding) inside the
    /// 1008×1008 tensor. Everything at `y >= padded_h` is zero pad.
    padded_h: usize,
}

/// Resize an RGB (HWC u8) source frame to 1008×1008 with aspect-
/// ratio preservation + zero padding on the short side, and reorder
/// to the CHW layout expected by the SAM 3 image encoder.
///
/// This is the standard SAM preprocessing pipeline (SAM 1 / SAM 2 /
/// SAM 3 all use this exact "longest-side resize + bottom-right pad"
/// convention — the trained model only sees 1008×1008 uint8 CHW
/// tensors during training, so at inference time we have to match).
/// The resize uses nearest-neighbor sampling, which is fast and
/// adequate because SAM's first conv layer does its own downsampling
/// that smooths any nearest-neighbor artifacts away.
///
/// * `rgb` must be `src_w * src_h * 3` bytes long in HWC / interleaved
///   RGB order. RGBA is NOT accepted — callers strip the alpha
///   channel before calling this function.
/// * Returns the preprocessed tensor plus the scale + pad metadata
///   needed to map mask coordinates back to source space.
fn preprocess_image_for_sam(rgb: &[u8], src_w: usize, src_h: usize) -> PreprocessedImage {
    assert!(src_w > 0 && src_h > 0, "source dimensions must be non-zero");
    assert_eq!(
        rgb.len(),
        src_w * src_h * 3,
        "preprocess_image_for_sam: rgb buffer length {} does not match {}x{}x3",
        rgb.len(),
        src_w,
        src_h,
    );

    // Compute the resize scale so the longest source axis lands at
    // exactly SAM_INPUT_SIZE (1008). The short side gets zero-padded
    // at the bottom/right of the output tensor.
    let longest = src_w.max(src_h) as f32;
    let scale = SAM_INPUT_SIZE as f32 / longest;
    let padded_w = ((src_w as f32 * scale).round() as usize).min(SAM_INPUT_SIZE);
    let padded_h = ((src_h as f32 * scale).round() as usize).min(SAM_INPUT_SIZE);

    // [C, H, W] uint8 tensor, zero-initialized so the padding region
    // comes out as black.
    let mut tensor = Array3::<u8>::zeros((3, SAM_INPUT_SIZE, SAM_INPUT_SIZE));

    // Nearest-neighbor resize into the content region.
    let x_ratio = src_w as f32 / padded_w.max(1) as f32;
    let y_ratio = src_h as f32 / padded_h.max(1) as f32;
    for dy in 0..padded_h {
        let sy = ((dy as f32 * y_ratio) as usize).min(src_h - 1);
        for dx in 0..padded_w {
            let sx = ((dx as f32 * x_ratio) as usize).min(src_w - 1);
            let idx = (sy * src_w + sx) * 3;
            tensor[[0, dy, dx]] = rgb[idx];
            tensor[[1, dy, dx]] = rgb[idx + 1];
            tensor[[2, dy, dx]] = rgb[idx + 2];
        }
    }

    PreprocessedImage {
        tensor,
        scale,
        padded_w,
        padded_h,
    }
}

// ── Session loading ────────────────────────────────────────────────────────

/// Loaded SAM 3 ONNX sessions. Holds the image encoder and decoder;
/// the language encoder is intentionally *not* loaded in Phase 2
/// because (1) it's not needed for box-prompt workflows, and (2)
/// the `.onnx.data` sidecar for the language encoder is missing
/// from the typical samexporter output so loading it would fail.
/// Phase 3 (text prompts) re-enables it after the user runs a
/// second export with `--include_external_data`.
pub struct SamSessions {
    pub image_encoder: ort::session::Session,
    pub decoder: ort::session::Session,
}

impl SamSessions {
    /// Open both sessions, routing each through
    /// [`crate::media::ai_providers::configure_session_builder`] so
    /// CUDA / ROCm / OpenVINO acceleration from Phase 0 applies
    /// automatically. Returns `Err` with a human-readable string on
    /// any failure — the only supported way to recover is to fix the
    /// install and retry.
    pub fn load(paths: &SamModelPaths) -> Result<Self, String> {
        use crate::media::ai_providers;
        use ort::session::{builder::GraphOptimizationLevel, Session};

        let backend = ai_providers::current_backend();

        log::info!(
            "SamCache: loading image encoder from {} (backend={:?})",
            paths.image_encoder.display(),
            backend
        );
        let image_encoder = Session::builder()
            .and_then(|b| Ok(b.with_optimization_level(GraphOptimizationLevel::Level3)?))
            .and_then(|b| ai_providers::configure_session_builder(b, backend))
            .and_then(|mut b| b.commit_from_file(&paths.image_encoder))
            .map_err(|e| format!("Failed to load SAM image encoder: {e}"))?;

        log::info!(
            "SamCache: loading decoder from {}",
            paths.decoder.display()
        );
        let decoder = Session::builder()
            .and_then(|b| Ok(b.with_optimization_level(GraphOptimizationLevel::Level3)?))
            .and_then(|b| ai_providers::configure_session_builder(b, backend))
            .and_then(|mut b| b.commit_from_file(&paths.decoder))
            .map_err(|e| format!("Failed to load SAM decoder: {e}"))?;

        Ok(Self {
            image_encoder,
            decoder,
        })
    }
}

// ── segment_with_box ───────────────────────────────────────────────────────

/// Run the full SAM 3 inference pipeline on a single RGB source frame
/// with a single box prompt, returning a binary mask at source
/// resolution. This is the only public SAM inference entry point in
/// Phase 2a — the Inspector UI, MCP tool, and future batched callers
/// all go through here.
///
/// # Arguments
/// * `sessions` — borrowed mutably because `ort::Session::run` takes
///   `&mut self` (it advances internal kernel caches).
/// * `rgb` — HWC RGB u8 source frame, `src_w * src_h * 3` bytes.
/// * `src_w` / `src_h` — source frame dimensions.
/// * `prompt` — box coordinates in source pixel space.
///
/// # Pipeline
///
/// 1. Clamp the prompt into source bounds (SAM's decoder will emit
///    nonsense if the box is outside the image).
/// 2. Preprocess: aspect-preserving resize + pad to 1008×1008 uint8
///    CHW.
/// 3. Run image encoder → 6 feature tensors at three FPN scales.
/// 4. Construct zero-filled language tensors so the decoder's
///    language-prompt path is effectively disabled.
/// 5. Rescale box coordinates from source space into 1008-space.
/// 6. Run decoder → `boxes`, `scores`, `masks`.
/// 7. Pick the highest-scoring mask slot.
/// 8. Un-pad + nearest-neighbor rescale the mask back to source
///    resolution, converting bool → u8 (0 / 255).
///
/// Every failure mode returns a descriptive `Err(String)` — callers
/// can surface these directly in the UI or MCP error response.
pub fn segment_with_box(
    sessions: &mut SamSessions,
    rgb: &[u8],
    src_w: usize,
    src_h: usize,
    prompt: BoxPrompt,
) -> Result<SegmentResult, String> {
    use ort::value::TensorRef;

    if src_w == 0 || src_h == 0 {
        return Err("segment_with_box: source dimensions must be non-zero".to_string());
    }
    if rgb.len() != src_w * src_h * 3 {
        return Err(format!(
            "segment_with_box: rgb length {} does not match {}x{}x3 ({})",
            rgb.len(),
            src_w,
            src_h,
            src_w * src_h * 3
        ));
    }

    let prompt = prompt.clamp_to_source(src_w, src_h);
    if prompt.x2 <= prompt.x1 || prompt.y2 <= prompt.y1 {
        return Err("segment_with_box: prompt box has zero area after clamping".to_string());
    }

    // 1. Preprocess the source frame into the encoder's 1008×1008
    //    uint8 CHW tensor.
    let preprocessed = preprocess_image_for_sam(rgb, src_w, src_h);

    // 2. Run image encoder. The encoder's input is literally
    //    `image: [3, 1008, 1008] uint8` with no batch dim, so we
    //    feed the 3D tensor directly.
    let encoder_input = TensorRef::from_array_view(&preprocessed.tensor)
        .map_err(|e| format!("segment_with_box: failed to wrap encoder input tensor: {e}"))?;
    let encoder_outputs = sessions
        .image_encoder
        .run(ort::inputs!["image" => encoder_input])
        .map_err(|e| format!("segment_with_box: image encoder run failed: {e}"))?;

    // Extract all 6 feature tensors. The decoder needs:
    //   backbone_fpn_0, backbone_fpn_1, backbone_fpn_2, vision_pos_enc_2
    // (backbone_fpn_0 / backbone_fpn_1 / backbone_fpn_2 at three
    // scales, plus only the coarsest vision_pos_enc level). We
    // extract into owned Vec<f32> buffers because the decoder's
    // run() borrows everything we pass in and ort's ValueRef output
    // views don't outlive the outputs collection.
    let (fpn0, fpn0_shape) = extract_float_tensor(&encoder_outputs, "backbone_fpn_0")?;
    let (fpn1, fpn1_shape) = extract_float_tensor(&encoder_outputs, "backbone_fpn_1")?;
    let (fpn2, fpn2_shape) = extract_float_tensor(&encoder_outputs, "backbone_fpn_2")?;
    let (pe2, pe2_shape) = extract_float_tensor(&encoder_outputs, "vision_pos_enc_2")?;

    // Reshape the flat f32 buffers back into ndarray tensors the
    // decoder will accept.
    let fpn0_arr = reshape_to_array4(fpn0, &fpn0_shape, "backbone_fpn_0")?;
    let fpn1_arr = reshape_to_array4(fpn1, &fpn1_shape, "backbone_fpn_1")?;
    let fpn2_arr = reshape_to_array4(fpn2, &fpn2_shape, "backbone_fpn_2")?;
    let pe2_arr = reshape_to_array4(pe2, &pe2_shape, "vision_pos_enc_2")?;

    // 3. Construct zero-filled language tensors. The decoder
    //    requires these as inputs but we're not using text prompts,
    //    so all-false mask + all-zero features effectively disables
    //    the language-attention path.
    let language_mask = Array2::<bool>::from_elem((1, LANG_TOKENS), false);
    let language_features = Array3::<f32>::zeros((LANG_TOKENS, 1, LANG_EMBED_DIM));

    // 4. Rescale the box from source pixel space into the encoder's
    //    1008×1008 space. The prompt was in source coordinates; the
    //    decoder expects them in the same space the image encoder
    //    saw, which is after the `scale` resize.
    let bx1 = prompt.x1 * preprocessed.scale;
    let by1 = prompt.y1 * preprocessed.scale;
    let bx2 = prompt.x2 * preprocessed.scale;
    let by2 = prompt.y2 * preprocessed.scale;
    // box_coords shape: [1, 1, 4]. Single batch, single box, four
    // corners (x1, y1, x2, y2). Different SAM exports sometimes use
    // (cx, cy, w, h) instead; wkentaro/sam3-onnx uses xyxy which is
    // what the exported decoder here was verified against during
    // Phase 2a bring-up.
    let box_coords =
        Array3::<f32>::from_shape_vec((1, 1, 4), vec![bx1, by1, bx2, by2])
            .map_err(|e| format!("segment_with_box: box_coords shape: {e}"))?;
    // box_labels shape: [1, 1] — single positive-foreground label
    // per the SAM 3 decoder convention. Label 1 is the "foreground
    // box prompt" signal; label 0 would be background.
    let box_labels = Array2::<i64>::from_shape_vec((1, 1), vec![1])
        .map_err(|e| format!("segment_with_box: box_labels shape: {e}"))?;
    // box_masks shape: [1, 1] — `true` means "this box slot is
    // active." If false, the decoder ignores the slot entirely,
    // which would give us a no-prompt segmentation (not useful
    // here).
    let box_masks = Array2::<bool>::from_shape_vec((1, 1), vec![true])
        .map_err(|e| format!("segment_with_box: box_masks shape: {e}"))?;

    let orig_h = Array1::<i64>::from_vec(vec![src_h as i64]);
    let orig_w = Array1::<i64>::from_vec(vec![src_w as i64]);
    // Decoder wants these as scalar tensors (rank 0). Collapsing a
    // 1-element Array1 to a 0-dim shape is the easiest way to
    // produce that shape with ndarray.
    let orig_h_scalar = orig_h
        .into_shape_with_order(())
        .map_err(|e| format!("original_height scalar reshape: {e}"))?;
    let orig_w_scalar = orig_w
        .into_shape_with_order(())
        .map_err(|e| format!("original_width scalar reshape: {e}"))?;

    // Wrap each ndarray in a TensorRef for ort::inputs!.
    let in_orig_h = TensorRef::from_array_view(&orig_h_scalar)
        .map_err(|e| format!("original_height tensor: {e}"))?;
    let in_orig_w = TensorRef::from_array_view(&orig_w_scalar)
        .map_err(|e| format!("original_width tensor: {e}"))?;
    let in_pe2 = TensorRef::from_array_view(&pe2_arr)
        .map_err(|e| format!("vision_pos_enc_2 tensor: {e}"))?;
    let in_fpn0 = TensorRef::from_array_view(&fpn0_arr)
        .map_err(|e| format!("backbone_fpn_0 tensor: {e}"))?;
    let in_fpn1 = TensorRef::from_array_view(&fpn1_arr)
        .map_err(|e| format!("backbone_fpn_1 tensor: {e}"))?;
    let in_fpn2 = TensorRef::from_array_view(&fpn2_arr)
        .map_err(|e| format!("backbone_fpn_2 tensor: {e}"))?;
    let in_lang_mask = TensorRef::from_array_view(&language_mask)
        .map_err(|e| format!("language_mask tensor: {e}"))?;
    let in_lang_feat = TensorRef::from_array_view(&language_features)
        .map_err(|e| format!("language_features tensor: {e}"))?;
    let in_box_coords = TensorRef::from_array_view(&box_coords)
        .map_err(|e| format!("box_coords tensor: {e}"))?;
    let in_box_labels = TensorRef::from_array_view(&box_labels)
        .map_err(|e| format!("box_labels tensor: {e}"))?;
    let in_box_masks = TensorRef::from_array_view(&box_masks)
        .map_err(|e| format!("box_masks tensor: {e}"))?;

    // 5. Run decoder with all 11 inputs in one call.
    let decoder_outputs = sessions
        .decoder
        .run(ort::inputs![
            "original_height" => in_orig_h,
            "original_width" => in_orig_w,
            "vision_pos_enc_2" => in_pe2,
            "backbone_fpn_0" => in_fpn0,
            "backbone_fpn_1" => in_fpn1,
            "backbone_fpn_2" => in_fpn2,
            "language_mask" => in_lang_mask,
            "language_features" => in_lang_feat,
            "box_coords" => in_box_coords,
            "box_labels" => in_box_labels,
            "box_masks" => in_box_masks,
        ])
        .map_err(|e| format!("segment_with_box: decoder run failed: {e}"))?;

    // 6. Extract scores and pick the highest one. The decoder can
    //    return multiple candidate instances matching the box prompt;
    //    for a single-object "click to mask" workflow we only care
    //    about the best one. (A future multi-instance UI could show
    //    all of them ranked.)
    let scores = match decoder_outputs.get("scores") {
        Some(val) => val
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("decoder scores extract: {e}"))?
            .1
            .to_vec(),
        None => return Err("decoder output missing 'scores'".to_string()),
    };
    if scores.is_empty() {
        return Err("decoder returned zero-length scores vector".to_string());
    }
    let (best_idx, best_score) = scores
        .iter()
        .enumerate()
        .fold((0usize, f32::NEG_INFINITY), |(bi, bs), (i, &s)| {
            if s > bs {
                (i, s)
            } else {
                (bi, bs)
            }
        });

    // 7. Extract the masks tensor and pull out the best slice. The
    //    decoder output is declared as `masks: [N, H, W, ?] bool` —
    //    but the actual emitted shape varies between exports. We
    //    reshape generically and fall back to the declared rank
    //    elsewhere.
    let (mask_shape_i64, mask_flat) = match decoder_outputs.get("masks") {
        Some(val) => val
            .try_extract_tensor::<bool>()
            .map(|(s, d)| (s.iter().copied().collect::<Vec<_>>(), d.to_vec()))
            .map_err(|e| format!("decoder masks extract: {e}"))?,
        None => return Err("decoder output missing 'masks'".to_string()),
    };
    // Compute [N, mask_h, mask_w] from the emitted shape (dropping
    // any trailing singleton channel dim). This keeps us tolerant to
    // variants that emit [N, H, W] vs [N, H, W, 1] vs [N, 1, H, W].
    let (n_masks, mask_h, mask_w) =
        interpret_mask_shape(&mask_shape_i64).map_err(|e| format!("masks shape: {e}"))?;
    if best_idx >= n_masks {
        return Err(format!(
            "best score index {best_idx} out of range for {n_masks} masks"
        ));
    }
    let per_mask_stride = mask_h * mask_w;
    let mask_slice_start = best_idx * per_mask_stride;
    let mask_slice_end = mask_slice_start + per_mask_stride;
    if mask_slice_end > mask_flat.len() {
        return Err(format!(
            "masks tensor truncated: expected at least {mask_slice_end} elements, got {}",
            mask_flat.len()
        ));
    }
    let mask_bool = &mask_flat[mask_slice_start..mask_slice_end];

    // 8. Rescale the bool mask back to source resolution. The
    //    decoder's mask output is at some intermediate resolution
    //    (often 1008×1008 or the source size directly since we pass
    //    `original_height`/`original_width`). We do a nearest-
    //    neighbor rescale either way so the caller always gets
    //    `src_w × src_h` bytes.
    let mut source_mask = vec![0u8; src_w * src_h];
    let x_ratio = mask_w as f32 / src_w as f32;
    let y_ratio = mask_h as f32 / src_h as f32;
    for sy in 0..src_h {
        let my = ((sy as f32 * y_ratio) as usize).min(mask_h.saturating_sub(1));
        for sx in 0..src_w {
            let mx = ((sx as f32 * x_ratio) as usize).min(mask_w.saturating_sub(1));
            if mask_bool[my * mask_w + mx] {
                source_mask[sy * src_w + sx] = 255;
            }
        }
    }

    Ok(SegmentResult {
        mask: source_mask,
        src_w,
        src_h,
        score: best_score,
    })
}

// ── Private helpers ────────────────────────────────────────────────────────

fn extract_float_tensor(
    outputs: &ort::session::SessionOutputs,
    name: &str,
) -> Result<(Vec<f32>, Vec<i64>), String> {
    let val = outputs
        .get(name)
        .ok_or_else(|| format!("encoder output missing '{name}'"))?;
    let (shape, data) = val
        .try_extract_tensor::<f32>()
        .map_err(|e| format!("failed to extract '{name}' as f32 tensor: {e}"))?;
    Ok((data.to_vec(), shape.iter().copied().collect()))
}

fn reshape_to_array4(
    data: Vec<f32>,
    shape: &[i64],
    name: &str,
) -> Result<Array4<f32>, String> {
    if shape.len() != 4 {
        return Err(format!(
            "'{name}' expected rank-4 tensor, got rank {}",
            shape.len()
        ));
    }
    let dims = (
        shape[0] as usize,
        shape[1] as usize,
        shape[2] as usize,
        shape[3] as usize,
    );
    Array4::<f32>::from_shape_vec(dims, data)
        .map_err(|e| format!("reshape '{name}' to {dims:?}: {e}"))
}

/// Interpret the masks output shape as `(n_masks, mask_h, mask_w)`.
/// Handles these SAM decoder variants:
///   - `[N, H, W]`         — rank-3, straightforward
///   - `[N, H, W, 1]`      — rank-4 with trailing singleton channel
///   - `[N, 1, H, W]`      — rank-4 with leading singleton channel
///   - `[1, N, H, W]`      — rank-4 with leading batch dim
fn interpret_mask_shape(shape: &[i64]) -> Result<(usize, usize, usize), String> {
    let dims: Vec<usize> = shape.iter().map(|d| *d as usize).collect();
    match dims.as_slice() {
        [n, h, w] => Ok((*n, *h, *w)),
        [n, h, w, 1] => Ok((*n, *h, *w)),
        [n, 1, h, w] => Ok((*n, *h, *w)),
        [1, n, h, w] => Ok((*n, *h, *w)),
        _ => Err(format!("unsupported masks rank/shape: {shape:?}")),
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

    // ── Inference backend tests ──────────────────────────────────

    #[test]
    fn box_prompt_from_corners_normalizes_orientation() {
        // Callers can drag the box either direction — the struct
        // always stores (x1,y1) as the top-left corner.
        let b = BoxPrompt::from_corners(50.0, 80.0, 10.0, 20.0);
        assert_eq!(b.x1, 10.0);
        assert_eq!(b.y1, 20.0);
        assert_eq!(b.x2, 50.0);
        assert_eq!(b.y2, 80.0);
    }

    #[test]
    fn box_prompt_point_emulation_builds_centered_square() {
        let b = BoxPrompt::point_emulation(100.0, 200.0, 4.0);
        assert_eq!(b.x1, 96.0);
        assert_eq!(b.y1, 196.0);
        assert_eq!(b.x2, 104.0);
        assert_eq!(b.y2, 204.0);
        // Width and height are 2 * half_size on each side.
        assert_eq!(b.x2 - b.x1, 8.0);
        assert_eq!(b.y2 - b.y1, 8.0);
    }

    #[test]
    fn box_prompt_clamp_to_source_clips_oob_corners() {
        // Prompt partially outside the source rectangle gets
        // clipped, not rejected.
        let b = BoxPrompt::from_corners(-10.0, -5.0, 200.0, 300.0)
            .clamp_to_source(100, 80);
        assert_eq!(b.x1, 0.0);
        assert_eq!(b.y1, 0.0);
        assert_eq!(b.x2, 99.0);
        assert_eq!(b.y2, 79.0);
    }

    #[test]
    fn preprocess_image_landscape_padded_on_bottom() {
        // 200×100 source: longest is 200 → scale 1008/200 = 5.04
        // → padded content is 1008 × 504. Bottom half is zero pad.
        let src_w = 200usize;
        let src_h = 100usize;
        let rgb: Vec<u8> = vec![0xff; src_w * src_h * 3]; // solid white
        let pp = preprocess_image_for_sam(&rgb, src_w, src_h);
        assert_eq!(pp.tensor.dim(), (3, SAM_INPUT_SIZE, SAM_INPUT_SIZE));
        assert_eq!(pp.padded_w, SAM_INPUT_SIZE);
        // 100 * 5.04 = 504, give or take rounding.
        assert!(
            (pp.padded_h as i32 - 504).abs() <= 1,
            "padded_h should round to ~504, got {}",
            pp.padded_h
        );
        assert!((pp.scale - 5.04).abs() < 0.01);

        // Content region is white, pad region is black.
        assert_eq!(pp.tensor[[0, 0, 0]], 0xff);
        assert_eq!(pp.tensor[[0, pp.padded_h - 1, pp.padded_w - 1]], 0xff);
        // Just past the content region: zero pad.
        assert_eq!(pp.tensor[[0, pp.padded_h, 0]], 0);
        assert_eq!(pp.tensor[[1, SAM_INPUT_SIZE - 1, SAM_INPUT_SIZE - 1]], 0);
    }

    #[test]
    fn preprocess_image_portrait_padded_on_right() {
        // 100×200 source: longest is 200 (height) → content is
        // 504 × 1008, right half is zero pad.
        let src_w = 100usize;
        let src_h = 200usize;
        let rgb: Vec<u8> = vec![0x80; src_w * src_h * 3];
        let pp = preprocess_image_for_sam(&rgb, src_w, src_h);
        assert_eq!(pp.padded_h, SAM_INPUT_SIZE);
        assert!(
            (pp.padded_w as i32 - 504).abs() <= 1,
            "padded_w should round to ~504, got {}",
            pp.padded_w
        );
        // Content region starts gray.
        assert_eq!(pp.tensor[[0, 0, 0]], 0x80);
        // Pad region is zero.
        assert_eq!(pp.tensor[[0, 0, pp.padded_w]], 0);
    }

    #[test]
    fn preprocess_image_square_source_fills_entire_1008() {
        // A square source should fill the whole 1008×1008 tensor
        // with no padding.
        let src = 500usize;
        let rgb: Vec<u8> = vec![123; src * src * 3];
        let pp = preprocess_image_for_sam(&rgb, src, src);
        assert_eq!(pp.padded_w, SAM_INPUT_SIZE);
        assert_eq!(pp.padded_h, SAM_INPUT_SIZE);
        assert_eq!(pp.tensor[[0, 0, 0]], 123);
        assert_eq!(
            pp.tensor[[2, SAM_INPUT_SIZE - 1, SAM_INPUT_SIZE - 1]],
            123
        );
    }

    #[test]
    fn preprocess_image_preserves_channel_order() {
        // 1×1 source with a unique (R, G, B) triple should land in
        // channels 0, 1, 2 respectively in the CHW tensor.
        let rgb: Vec<u8> = vec![10, 20, 30];
        let pp = preprocess_image_for_sam(&rgb, 1, 1);
        // After scaling, the single pixel covers the entire 1008×1008
        // content region (which fills the whole tensor for a 1×1
        // source). Sample the origin.
        assert_eq!(pp.tensor[[0, 0, 0]], 10);
        assert_eq!(pp.tensor[[1, 0, 0]], 20);
        assert_eq!(pp.tensor[[2, 0, 0]], 30);
    }

    #[test]
    fn interpret_mask_shape_handles_common_variants() {
        assert_eq!(interpret_mask_shape(&[3, 128, 128]).unwrap(), (3, 128, 128));
        assert_eq!(
            interpret_mask_shape(&[3, 128, 128, 1]).unwrap(),
            (3, 128, 128)
        );
        assert_eq!(
            interpret_mask_shape(&[3, 1, 128, 128]).unwrap(),
            (3, 128, 128)
        );
        assert_eq!(
            interpret_mask_shape(&[1, 3, 128, 128]).unwrap(),
            (3, 128, 128)
        );
        assert!(interpret_mask_shape(&[128]).is_err());
        assert!(interpret_mask_shape(&[5, 5, 5, 5, 5]).is_err());
    }

    /// End-to-end integration test: loads the real SAM 3 ONNX
    /// sessions, runs `segment_with_box` on a synthetic image with a
    /// bright square on a dark background, and asserts the resulting
    /// mask roughly overlaps the known square position.
    ///
    /// This test is `#[ignore]`-gated because it requires the SAM
    /// model to be installed locally (~2 GB download) and takes
    /// tens of seconds to minutes on CPU. Run it manually on a dev
    /// machine with:
    ///
    /// ```
    /// cargo test -- --ignored --nocapture sam_cache::tests::segment_with_box_smoke
    /// ```
    ///
    /// The test self-skips if the model isn't installed, so it's
    /// safe to run unconditionally on any machine.
    #[test]
    #[ignore]
    fn segment_with_box_smoke() {
        let paths = match find_sam_model_paths() {
            Some(p) => p,
            None => {
                eprintln!(
                    "segment_with_box_smoke: SAM model not installed, \
                     skipping. Install with samexporter and retry."
                );
                return;
            }
        };

        eprintln!("Loading SAM sessions from {}...", paths.image_encoder.display());
        let mut sessions = SamSessions::load(&paths).expect("SamSessions::load");

        // Synthesize a 512×512 RGB image with a bright white 100×100
        // square at (150, 150). SAM should find a mask that covers
        // most of the square when prompted with a box around it.
        let src_w = 512usize;
        let src_h = 512usize;
        let mut rgb = vec![20u8; src_w * src_h * 3];
        for y in 150..250 {
            for x in 150..250 {
                let idx = (y * src_w + x) * 3;
                rgb[idx] = 240;
                rgb[idx + 1] = 240;
                rgb[idx + 2] = 240;
            }
        }

        // Prompt: a box slightly larger than the white square.
        let prompt = BoxPrompt::from_corners(145.0, 145.0, 255.0, 255.0);

        eprintln!("Running segment_with_box (this may take 10-60s on CPU)...");
        let result = segment_with_box(&mut sessions, &rgb, src_w, src_h, prompt)
            .expect("segment_with_box");

        eprintln!("SAM returned mask with score={}", result.score);
        assert_eq!(result.src_w, src_w);
        assert_eq!(result.src_h, src_h);
        assert_eq!(result.mask.len(), src_w * src_h);

        // Count foreground pixels and verify the mask isn't
        // degenerate (entirely empty or entirely full).
        let fg: usize = result.mask.iter().filter(|&&v| v > 0).count();
        eprintln!(
            "Foreground pixels: {} / {} ({:.2}%)",
            fg,
            src_w * src_h,
            100.0 * fg as f64 / (src_w * src_h) as f64
        );
        assert!(fg > 100, "mask should contain at least some foreground");
        assert!(
            fg < src_w * src_h / 2,
            "mask should not fill more than half the frame"
        );

        // Check that a majority of the actual white square region
        // lands inside the mask — this is the real "did SAM find the
        // object" signal.
        let mut square_mask_hits = 0usize;
        for y in 150..250 {
            for x in 150..250 {
                if result.mask[y * src_w + x] > 0 {
                    square_mask_hits += 1;
                }
            }
        }
        let square_area = 100 * 100;
        let hit_ratio = square_mask_hits as f64 / square_area as f64;
        eprintln!(
            "Square hit ratio: {}/{} ({:.2}%)",
            square_mask_hits,
            square_area,
            hit_ratio * 100.0
        );
        assert!(
            hit_ratio > 0.5,
            "SAM mask should cover >50% of the white square; got {:.2}%",
            hit_ratio * 100.0
        );
    }
}
