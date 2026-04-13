// SPDX-License-Identifier: GPL-3.0-or-later
//! Background-thread SAM inference dispatcher.
//!
//! Phase 2b/1. This module wraps the full SAM inference pipeline
//! (frame decode → image encoder → decoder → contour extraction)
//! in a thread-safe helper that can be spawned from the GTK main
//! thread without blocking it during the ~6 s cold-start inference
//! window.
//!
//! ## Why not reuse the MCP handler's inline pipeline?
//!
//! The Phase 2a/3 MCP handler runs the same pipeline synchronously
//! on the GTK main thread because MCP callers are headless
//! automation scripts that don't care about UI responsiveness. But
//! the Phase 2b Inspector "Generate with SAM" button *does* run on
//! the main thread and MUST stay responsive — a 6 s freeze on
//! every click would be unshippable.
//!
//! The fix is to lift the pipeline into a pure function that:
//!
//! * takes only Send-safe inputs (paths, numbers, the prompt struct),
//! * returns only Send-safe outputs (bezier points, a score, a
//!   human-readable error string on failure),
//! * doesn't borrow the project, touch GTK, or hold any non-Send
//!   state.
//!
//! The Inspector button then spawns one of these in a
//! `std::thread::spawn` closure, the background thread runs the
//! pipeline, and the main thread polls for the result via a
//! `mpsc::Receiver` on a `glib::timeout_add_local` tick. When the
//! result lands, the main thread re-borrows the project and
//! applies the mask — which is when we're back on the GTK main
//! thread and can touch `Rc<RefCell<Project>>`, the Inspector
//! widgets, undo history, etc.
//!
//! ## What this commit does NOT do
//!
//! * No UI button — that's Phase 2b/2.
//! * No session caching — each call loads the SAM sessions fresh,
//!   paying the ~2 s load cost per click. A later optimization
//!   (Phase 2d or similar) can cache sessions in an
//!   `Arc<Mutex<Option<SamSessions>>>` across jobs.
//! * No refactor of the MCP handler to also go through this
//!   dispatcher — the MCP handler stays synchronous for now.
//!   Deduplicating the pipeline logic between MCP and Inspector
//!   call sites is a follow-up cleanup, not urgent.

#![cfg(feature = "ai-inference")]

use std::path::PathBuf;
use std::sync::{mpsc, Mutex};

use crate::media::mask_contour;
use crate::media::sam_cache::{self, BoxPrompt, SamSessions};
use crate::model::clip::BezierPoint;

// ── Session cache (Phase 2d) ─────���───────────────────────────────────────
//
// A single global cache for the SAM ONNX sessions. Loading sessions
// costs ~2 s (model files are ~2 GB total); caching eliminates that
// cost on the 2nd+ click. The cache holds at most one set of sessions
// at a time — the worker thread takes them out, uses them, and puts
// them back on completion (success or failure).
//
// Thread safety: `SamSessions` is `Send` (ort::Session is Send). The
// `Mutex` ensures only one thread accesses the sessions at a time,
// which matches the single-job-at-a-time design of `spawn_sam_job`.
static SESSION_CACHE: Mutex<Option<SamSessions>> = Mutex::new(None);

// ── Input / output types ──────────────────────────────────────────────────

/// All the inputs a SAM job needs to run to completion. Every field
/// is `Send + 'static`, so a `SamJobInput` can be moved into a
/// background thread without touching any main-thread state.
#[derive(Debug, Clone)]
pub struct SamJobInput {
    /// Absolute path to the source media file (not a clip id — the
    /// caller resolves the clip on the main thread *before*
    /// dispatching, so the worker thread doesn't need to borrow the
    /// project).
    pub source_path: PathBuf,
    /// Absolute source-media time in ns for the frame to segment.
    pub frame_ns: u64,
    /// Box or emulated-point prompt in source-pixel coordinates.
    /// Ignored if [`SamJobInput::normalized_box`] is `Some` — in that
    /// case the pipeline rebuilds `prompt` after decoding the frame,
    /// once `src_w` / `src_h` are known.
    pub prompt: BoxPrompt,
    /// Optional `(x1, y1, x2, y2)` box in normalized clip-local
    /// coordinates, 0..1. If `Some`, the pipeline multiplies each
    /// coordinate by the decoded frame's `src_w` / `src_h` and
    /// overrides `prompt` with the resulting pixel box. Used by the
    /// Inspector button which doesn't know source dimensions at
    /// click time. Set to `None` when passing a pixel-space prompt.
    pub normalized_box: Option<(f32, f32, f32, f32)>,
    /// Douglas-Peucker tolerance in source pixels. 2.0 is the
    /// default used by the MCP tool; smaller = finer polygon,
    /// larger = coarser.
    pub tolerance_px: f64,
}

/// Outcome of a SAM job. `Success` carries the full bezier polygon
/// and SAM's confidence score; `Error` carries a human-readable
/// message suitable for direct UI display.
///
/// Both variants are `Send + 'static` so they travel cleanly from
/// the worker thread back to the main thread through an mpsc
/// channel.
#[derive(Debug, Clone)]
pub enum SamJobResult {
    Success {
        /// Bezier polygon points in normalized 0..1 clip-local
        /// coordinates, ready to plug into
        /// `ClipMask::new_path(points)`.
        mask_points: Vec<BezierPoint>,
        /// SAM's confidence score for the returned mask. Higher is
        /// better; typical range for valid matches is ~0.3–0.9.
        score: f32,
    },
    Error(String),
}

impl SamJobResult {
    /// Convenience accessor for "did this job succeed?" — useful
    /// for test assertions and UI state updates.
    pub fn is_success(&self) -> bool {
        matches!(self, SamJobResult::Success { .. })
    }
}

// ── Pipeline function (pure, thread-safe) ─────────────────────────────────

/// Run the full SAM inference pipeline on the calling thread.
/// Blocking — takes seconds to run.
///
/// This is the function the Phase 2b Inspector button wraps in
/// `std::thread::spawn`. It's also safe to call synchronously
/// from the MCP handler if we want to eventually dedupe the
/// pipeline code — for now the two call sites live separately.
///
/// # Pipeline steps
///
/// 1. Locate the installed SAM model via `find_sam_model_paths`.
///    Returns `Error("SAM 3 model not installed...")` on miss.
/// 2. Decode the requested frame via `decode_single_frame`
///    (ffmpeg subprocess). Returns `Error("Frame decode failed:
///    <reason>")` on miss.
/// 3. Load SAM sessions via `SamSessions::load`. Returns
///    `Error("SAM session load failed: <reason>")` on miss.
/// 4. Run `segment_with_box` to produce a binary mask at source
///    resolution. Returns `Error("SAM inference failed: <reason>")`
///    on miss.
/// 5. Run `mask_to_bezier_path` to extract a closed bezier
///    polygon. Returns `Error("SAM produced an empty or
///    degenerate mask")` if the mask has no valid contour.
/// 6. Return `Success { mask_points, score }`.
///
/// Every error variant returns a descriptive string the caller can
/// surface directly in the UI or the MCP response.
pub fn run_sam_pipeline(input: SamJobInput) -> SamJobResult {
    // Step 1 — find the model install.
    let sam_paths = match sam_cache::find_sam_model_paths() {
        Some(p) => p,
        None => {
            return SamJobResult::Error(
                "SAM 3 model not installed. See Preferences → Models → \
                 Segment Anything 3.1 for install instructions."
                    .to_string(),
            );
        }
    };

    // Step 2 — decode the frame via ffmpeg.
    let (rgb, src_w, src_h) =
        match sam_cache::decode_single_frame(&input.source_path, input.frame_ns) {
            Ok(v) => v,
            Err(e) => return SamJobResult::Error(format!("Frame decode failed: {e}")),
        };

    // Step 2b — if the caller passed a normalized override, convert
    // it to pixel coordinates now that we know the source dimensions
    // and replace `input.prompt`. Used by the Inspector button which
    // doesn't know `src_w` / `src_h` at click time.
    let prompt = if let Some((nx1, ny1, nx2, ny2)) = input.normalized_box {
        let sx1 = nx1 * src_w as f32;
        let sy1 = ny1 * src_h as f32;
        let sx2 = nx2 * src_w as f32;
        let sy2 = ny2 * src_h as f32;
        BoxPrompt::from_corners(sx1, sy1, sx2, sy2)
    } else {
        input.prompt
    };

    // Step 3 — acquire SAM sessions from the cache, or load fresh
    // if this is the first call (or the cache was poisoned by a
    // previous panic — unlikely but defended against).
    let mut sessions = {
        let cached = SESSION_CACHE.lock().ok().and_then(|mut guard| guard.take());
        match cached {
            Some(s) => {
                log::debug!("SAM: reusing cached sessions");
                s
            }
            None => match sam_cache::SamSessions::load(&sam_paths) {
                Ok(s) => s,
                Err(e) => return SamJobResult::Error(format!("SAM session load failed: {e}")),
            },
        }
    };

    // Step 4 — run inference.
    let result = match sam_cache::segment_with_box(&mut sessions, &rgb, src_w, src_h, prompt) {
        Ok(r) => r,
        Err(e) => {
            // Return sessions to the cache even on inference
            // failure — the sessions themselves are still valid.
            if let Ok(mut guard) = SESSION_CACHE.lock() {
                *guard = Some(sessions);
            }
            return SamJobResult::Error(format!("SAM inference failed: {e}"));
        }
    };

    // Return sessions to the cache for the next job.
    if let Ok(mut guard) = SESSION_CACHE.lock() {
        *guard = Some(sessions);
    }

    // Step 5 — extract contour.
    let mask_points = match mask_contour::mask_to_bezier_path(
        &result.mask,
        result.src_w,
        result.src_h,
        input.tolerance_px,
    ) {
        Some(p) if p.len() >= 3 => p,
        _ => {
            return SamJobResult::Error(
                "SAM produced an empty or degenerate mask (no closed contour found)".to_string(),
            );
        }
    };

    // Diagnostic: log the prompt vs mask centroid so coordinate
    // mapping issues are visible in the log.
    if !mask_points.is_empty() {
        let (sum_x, sum_y) = mask_points
            .iter()
            .fold((0.0_f64, 0.0_f64), |(sx, sy), p| (sx + p.x, sy + p.y));
        let n = mask_points.len() as f64;
        let prompt_cx = if let Some((nx1, _, nx2, _)) = input.normalized_box {
            (nx1 + nx2) as f64 / 2.0
        } else {
            (input.prompt.x1 + input.prompt.x2) as f64 / 2.0 / src_w as f64
        };
        let prompt_cy = if let Some((_, ny1, _, ny2)) = input.normalized_box {
            (ny1 + ny2) as f64 / 2.0
        } else {
            (input.prompt.y1 + input.prompt.y2) as f64 / 2.0 / src_h as f64
        };
        log::info!(
            "SAM: prompt center=({:.4},{:.4}), mask centroid=({:.4},{:.4}), \
             {} bezier points, score={:.3}, source={}x{}",
            prompt_cx,
            prompt_cy,
            sum_x / n,
            sum_y / n,
            mask_points.len(),
            result.score,
            src_w,
            src_h,
        );
    }

    SamJobResult::Success {
        mask_points,
        score: result.score,
    }
}

// ── Background thread dispatch ────────────────────────────────────────────

/// Handle returned by [`spawn_sam_job`]. Holds the mpsc receiver
/// that the background thread will send its single result on. The
/// main thread polls this via [`SamJobHandle::try_recv`] on a
/// `glib::timeout_add_local` tick and picks up the result when
/// ready.
///
/// The background thread terminates after sending its single
/// result, so there's no `Drop` cleanup needed — if the main
/// thread drops the handle without draining it (e.g. the user
/// navigated away mid-inference), the worker thread's send fails
/// silently and everything goes away.
pub struct SamJobHandle {
    result_rx: mpsc::Receiver<SamJobResult>,
}

impl SamJobHandle {
    /// Non-blocking poll. Returns `Some(result)` exactly once when
    /// the background thread has finished and sent its result; all
    /// subsequent calls return `None`. Returns `None` immediately
    /// if the job is still running.
    pub fn try_recv(&self) -> Option<SamJobResult> {
        self.result_rx.try_recv().ok()
    }
}

/// Spawn a background thread that runs [`run_sam_pipeline`] against
/// `input` and delivers the result to a fresh [`SamJobHandle`].
/// Returns immediately — the main thread never blocks.
///
/// The worker thread terminates after sending its one result. If
/// the main thread drops the returned `SamJobHandle` without
/// polling (e.g. the user closed the Inspector panel before SAM
/// finished), the worker's `send` hits a disconnected channel on
/// the next call, which we let fail silently — no thread-join
/// deadlock, no resource leak beyond the worker's own stack and
/// the SAM session it had loaded.
pub fn spawn_sam_job(input: SamJobInput) -> SamJobHandle {
    // `sync_channel(1)` gives us a bounded channel with exactly one
    // slot — the worker thread only sends one result and then
    // terminates, so a bounded channel matches the single-shot
    // semantics perfectly. If we used an unbounded `channel`, the
    // semantics would be identical but the bounded form documents
    // the intent.
    let (tx, rx) = mpsc::sync_channel::<SamJobResult>(1);

    std::thread::Builder::new()
        .name("sam-inference".to_string())
        .spawn(move || {
            let result = run_sam_pipeline(input);
            // If the receiver was dropped (handle gone, UI
            // navigated away), this send fails and we just return.
            // No panic, no log spam — dropped handles are an
            // expected part of the "user cancelled" lifecycle.
            let _ = tx.send(result);
        })
        .expect("failed to spawn sam-inference thread");

    SamJobHandle { result_rx: rx }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Helper: poll a `SamJobHandle` until it delivers a result or
    /// a timeout elapses. Tests fail fast on stuck jobs instead of
    /// hanging the whole test binary.
    fn wait_for_result(handle: &SamJobHandle, timeout: Duration) -> Option<SamJobResult> {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if let Some(r) = handle.try_recv() {
                return Some(r);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }

    #[test]
    fn sam_job_result_is_success_flag() {
        let ok = SamJobResult::Success {
            mask_points: vec![],
            score: 0.5,
        };
        assert!(ok.is_success());
        let err = SamJobResult::Error("nope".to_string());
        assert!(!err.is_success());
    }

    #[test]
    fn spawn_sam_job_bogus_source_returns_decode_error() {
        // Use a path that definitely doesn't exist. The frame
        // decoder will fail at step 2 of run_sam_pipeline and the
        // job should deliver a descriptive error, not hang.
        //
        // IMPORTANT: this test only runs through to step 2 if SAM
        // is *installed*. If the test machine has no SAM model
        // (most CI runners) it'll stop at step 1 with "SAM 3 model
        // not installed" — which is also a valid error response,
        // just from a different phase of the pipeline. Both cases
        // are acceptable for this test's purpose: "bogus input
        // produces a descriptive Err, not a hang or panic."
        let input = SamJobInput {
            source_path: PathBuf::from("/nonexistent/path/to/bogus.mp4"),
            frame_ns: 0,
            prompt: BoxPrompt::from_corners(100.0, 100.0, 200.0, 200.0),
            normalized_box: None,
            tolerance_px: 2.0,
        };
        let handle = spawn_sam_job(input);
        // Generous timeout: SAM session load on a cold box can
        // take several seconds, and "not installed" / "frame
        // decode failed" should both complete fast but we don't
        // want flakes.
        let result = wait_for_result(&handle, Duration::from_secs(30))
            .expect("job should complete within 30 s");
        match result {
            SamJobResult::Success { .. } => {
                panic!("bogus source path should never succeed")
            }
            SamJobResult::Error(msg) => {
                // Accept either the "model not installed" or the
                // "frame decode failed" variant depending on
                // whether the test machine has SAM installed.
                let ok = msg.contains("not installed")
                    || msg.contains("Frame decode failed")
                    || msg.contains("decode");
                assert!(ok, "unexpected error variant: {msg}");
            }
        }
    }

    #[test]
    fn sam_job_handle_try_recv_returns_none_before_completion() {
        // Pending job: spawn one with a bogus source so it fails
        // fast, but try_recv should still return None in the first
        // millisecond before the worker thread has sent its
        // result. This is a loose check — if the worker is too
        // fast we might miss the "still pending" window, in which
        // case the test passes silently on the early-return path.
        let input = SamJobInput {
            source_path: PathBuf::from("/nonexistent/path.mp4"),
            frame_ns: 0,
            prompt: BoxPrompt::from_corners(0.0, 0.0, 10.0, 10.0),
            normalized_box: None,
            tolerance_px: 2.0,
        };
        let handle = spawn_sam_job(input);
        // Most of the time this sees None; sometimes the worker
        // already sent before we polled. Both are OK — the
        // assertion we actually care about is "try_recv doesn't
        // block."
        let _initial = handle.try_recv();
        // Then drain to completion so the worker thread exits
        // cleanly before the test finishes (otherwise it races
        // with test-harness teardown and may trip leak detectors
        // in some builds).
        let _final = wait_for_result(&handle, Duration::from_secs(30));
    }

    #[test]
    fn sam_job_handle_drop_does_not_panic() {
        // Handle is dropped immediately after spawn. The worker
        // thread's `tx.send` will hit a disconnected receiver when
        // it finishes and silently fail. This test passes if the
        // process doesn't panic.
        let input = SamJobInput {
            source_path: PathBuf::from("/nonexistent/path.mp4"),
            frame_ns: 0,
            prompt: BoxPrompt::from_corners(0.0, 0.0, 10.0, 10.0),
            normalized_box: None,
            tolerance_px: 2.0,
        };
        let _handle = spawn_sam_job(input);
        // Drop immediately.
        drop(_handle);
        // Give the worker thread a moment to complete so the test
        // runner doesn't race with it during shutdown.
        std::thread::sleep(Duration::from_millis(100));
    }
}
