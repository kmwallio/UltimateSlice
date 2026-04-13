# SAM integration — work-in-progress notes

Status tracking for the Segment Anything 3 integration on branch `sam`.
Everything below assumes familiarity with the existing AI inference
infrastructure (`src/media/bg_removal_cache.rs`, etc.) and the Phase 0
GPU-provider abstraction (`src/media/ai_providers.rs`).

## Where we are

Branch `sam` is pushed through commit `92583af`. Phase 2a is
validated end-to-end against a real 1920×1080 vlog clip on an Intel
Arc laptop via the `ai-webgpu` feature flag; SAM correctly traces a
small sticker subject in ~6 s per inference with `score ≈ 0.63` and
sub-1% center error vs the known tracker position.

Phase 2b/2 is **implemented but not yet committed** — the Inspector
"Generate with SAM" button is wired to `sam_job::spawn_sam_job` with
a hardcoded centre-region box prompt and a 100 ms polling tick that
replaces `masks[0]` on success. Builds clean on both `ai-webgpu` and
no-feature, all `sam_job` tests still pass. See "Phase 2b/2 — DONE"
below for the change list.

Commit history on the branch (bottom = oldest):

| Commit    | Phase | Summary                                                                      |
|-----------|-------|------------------------------------------------------------------------------|
| `bfd7c5e` | 0     | GPU execution provider detection + selection                                 |
| `712639c` | 1     | SAM install detection + Preferences row                                      |
| `a248cae` | 1.1   | `.pt` checkpoint detection + correct ONNX filenames + RIFE hint              |
| `6eaf879` | 2a/1  | `sam_cache` inference backend: sessions, preprocessing, `segment_with_box`   |
| `e57c1c9` | 2a/2  | `mask_contour`: binary mask → bezier polygon                                 |
| `548ff45` | 2a/3  | `generate_sam_mask` MCP tool — first end-to-end consumer                     |
| `cb8061e` | (other) | WebGPU provider + three `segment_with_box` bug fixes + per-vendor GPU docs |
| `f7f12fe` | 2a/4  | Non-square mask rescale fix for 1920×1080 sources                            |
| `92583af` | 2b/1  | `sam_job` background dispatcher (headless, no UI yet)                        |
| (staged)  | 2b/2  | Inspector "Generate with SAM" button + poll tick + normalized_box plumbing   |
| (staged)  | 2b/3  | Drag-to-box prompt via Program Monitor TransformOverlay                      |

Tests: 1056 passing, 2 ignored (as of 2b/2 staging — `segment_with_box_smoke`
still needs model + GPU or 128 GB RAM).

## Phase 2b/2 — DONE

Landed locally but not yet committed. Changes:

- **`src/media/sam_job.rs`** — added
  `normalized_box: Option<(f32, f32, f32, f32)>` to `SamJobInput`.
  When `Some`, the pipeline rebuilds the pixel prompt from
  `(x1, y1, x2, y2) × (src_w, src_h)` after `decode_single_frame`.
  Used by the Inspector button, which doesn't know source
  dimensions at click time. Existing pixel-space callers (tests,
  future MCP refactor) pass `None`.
- **`src/ui/inspector.rs`** —
  - New `SamJobInFlight { handle, clip_id }` helper so the poll
    tick applies the result to the clip the user originally
    clicked on, even if selection has since changed.
  - New `InspectorView` fields: `sam_generate_btn: Button`
    (unconditional) + `sam_job_handle: Rc<RefCell<Option<SamJobInFlight>>>`
    (feature-gated).
  - "Generate with SAM" button at the bottom of the Shape Mask
    panel, hidden when `ai-inference` is off.
  - Click handler: non-default-mask confirmation dialog via
    `gtk::AlertDialog::choose`, then spawns a SAM job with
    hardcoded normalized box `(0.35, 0.35)–(0.65, 0.65)` (tight
    enough to stay on the safe side of SAM 3's decoder constraint).
    Button flips to "Generating… (~6s)" and goes insensitive.
  - 100 ms `glib::timeout_add_local` polling tick installed once
    during `build_inspector` that drains the handle, replaces
    `masks[0]` with `ClipMask::new_path(...)` on success (marks
    dirty + calls `on_frei0r_changed`), or pops an `AlertDialog`
    with the error text on failure. Button restored either way.
  - Populate path sets button sensitivity based on
    `find_sam_model_paths().is_some() && !job_busy && (is_video || is_image)`.

**Not yet done in 2b/2:** live manual validation against a real
clip on a GUI session. This is the Phase 2b/2 validation procedure
step from the original plan. The code compiles, tests pass, but
"click the button, watch a mask appear" still needs a human driver.

## Remaining Phase 2b plan

Phase 2b is the first user-visible SAM feature: an Inspector button
that invokes SAM on the currently-selected clip with a user-drawn
box prompt, replacing `masks[0]` with the resulting bezier polygon.
The "replace `masks[0]`" strategy is Option A from the earlier
design discussion; multi-mask UI is deferred to a later phase.

### Phase 2b/2 — Inspector button wired to hardcoded box prompt

**Goal:** visible button in the Inspector Masks panel that, when
clicked, runs the full SAM pipeline against the selected clip and
replaces `masks[0]` on success. Uses a hardcoded center-region box
prompt for now; real drag-to-box UI comes in 2b/3.

**Files to touch:**

- `src/ui/inspector.rs` (~10k lines — careful recon first). Look for
  the existing Masks panel section. Insertion point is near the
  existing mask shape/enable controls that touch `clip.masks[0]`.
- Possibly `src/ui/window.rs` to wire the SAM job poll timer into
  the main-thread event loop alongside the other cache pollers.

**New code:**

1. **Button widget** in the Masks panel, label "Generate with SAM",
   enabled only when:
   - `#[cfg(feature = "ai-inference")]`
   - `sam_cache::find_sam_model_paths().is_some()`
   - A clip is currently selected
2. **Confirmation dialog** shown when `masks[0]` has non-default
   content (test: `mask.path.is_some()` OR any shape/position field
   differs from `ClipMask::new(MaskShape::Rectangle)` defaults).
   Dialog: "This will replace your existing mask. Continue?" with
   Cancel / Replace buttons.
3. **Click handler** that:
   - Resolves the selected clip's `id`, `source_path`, `source_in`,
     `source_out` via a brief `project.borrow()`.
   - Constructs a hardcoded center-region `BoxPrompt` in source
     pixel coordinates. For Phase 2b/2 use a tight center box
     equivalent to normalized `(0.35, 0.35)–(0.65, 0.65)` — small
     enough that SAM 3's "tight exemplar" decoder constraint
     doesn't trip the zero-length-scores failure mode.
   - Calls `sam_job::spawn_sam_job(SamJobInput { ... })`.
   - Stashes the returned `SamJobHandle` in Inspector state
     (probably a new field on `InspectorView` or a
     `Rc<RefCell<Option<SamJobHandle>>>` alongside it).
   - Changes the button label to "Generating… (~6s)" and
     `set_sensitive(false)`.
4. **Polling tick** via `glib::timeout_add_local` at 100 ms (or
   fold into an existing polling timer if there is one nearby).
   On each tick, drain `handle.try_recv()`:
   - `Some(SamJobResult::Success { mask_points, score })`: replace
     `clip.masks[0]` with `ClipMask::new_path(mask_points)`, mark
     project dirty, call `on_project_changed()`, restore button
     label, clear the handle.
   - `Some(SamJobResult::Error(msg))`: show a
     `gtk::MessageDialog::builder().message_type(Error)...`
     with the error text, restore button, clear the handle.
   - `None`: job still running, do nothing.

**Scope estimate:** 300–500 LOC in `inspector.rs` plus ~50 LOC in
`window.rs` for the poll-tick wiring. One commit. Reviewable.

**Validation procedure for this commit:**

1. Build: `cargo build --release --features ai-webgpu`
2. Launch, open a project with a video clip, select a clip.
3. Inspector → Masks panel. "Generate with SAM" button appears,
   enabled, below the existing mask controls.
4. Click it. Confirmation dialog appears if there's an existing
   non-default mask. Accept.
5. Button label flips to "Generating… (~6s)", button disabled.
6. After ~6 s, button label restores and a new bezier-path mask
   appears on the clip in the Program Monitor, covering roughly
   the center of the frame (since the hardcoded prompt is a
   center-region box).
7. Error cases: remove the SAM model files, restart, click the
   button — expect the button to be disabled. Re-install, restart,
   click with a clip that has a zero-area source range — expect
   the error dialog with "Frame decode failed" or similar.

### Phase 2b/3 — Real drag-to-box prompt via Program Monitor

**Goal:** replace the hardcoded box from 2b/2 with a rectangle the
user draws on the Program Monitor. Entering "SAM prompt mode" is
triggered by the Inspector button; the user drags a box and
releases; the box becomes the prompt.

**Files to touch:**

- `src/ui/transform_overlay.rs` (~1.5k lines). This is the module
  that currently handles scale / position / crop / rotation handle
  interactions on the Program Monitor's selected clip. Add a new
  "SAM prompt mode" state that takes over click+drag input, draws
  a live rectangle, and commits a `BoxPrompt` on release.
- `src/ui/inspector.rs` — the click handler from 2b/2 no longer
  builds a hardcoded prompt; instead it flips the transform
  overlay into SAM prompt mode and registers a one-shot callback.

**Key design questions to answer before coding:**

1. **How does the overlay communicate the captured box back to
   the Inspector?** Options: a shared `Rc<RefCell<Option<BoxPrompt>>>`
   in window scope; a channel via `glib::Sender`; a callback
   closure stored in the transform overlay. The existing overlay
   has similar patterns for transform edits — copy whichever is
   already used.
2. **What happens if the user presses Escape?** Exit prompt mode,
   restore the button, clear any pending state. Needs to hook a
   keyboard handler that the overlay doesn't currently have in
   prompt mode.
3. **Click-without-drag behavior.** A single click (no drag)
   should be interpreted as a point prompt — built via
   `BoxPrompt::point_emulation(cx, cy, 4.0)` — since users will
   click on subjects expecting point-to-mask behavior. Threshold:
   if `release_pos - press_pos < 4 px`, treat as point prompt;
   otherwise box prompt.
4. **Visual feedback during drag.** Draw a semi-transparent blue
   rectangle as the user drags, matching the style of the
   existing transform handles.

**Risk:** the transform overlay's existing state machine handles
many interaction modes (move / scale / crop-edge / rotation / no
selection). Adding a new mode without breaking the existing
interactions needs careful state isolation. Do the recon first
before writing code.

**Scope estimate:** 200–400 LOC in `transform_overlay.rs`, 50 LOC
in `inspector.rs` to hand off. One commit.

**Validation:** click the Inspector button, cursor changes to
crosshair, status bar shows hint, drag a rectangle on the sticker
subject, release → mask appears on the sticker. Press Escape
during drag to cancel → no mask change. Click without drag on the
sticker → small point-prompt mask appears.

### Phase 2b/4 — Undo integration

**Goal:** Ctrl+Z after SAM mask generation reverts `masks[0]` to
its previous state.

**Files to touch:**

- `src/undo.rs`. Add a `ReplaceClipMaskCommand` (or similar name,
  match the existing naming convention in the file) that captures
  the old `masks[0]` state (via something like
  `ClipMaskSnapshot::from_clip`, which already exists per the
  `cargo check` warnings) and the new one.
- `src/ui/inspector.rs` — the click handler's success path now
  dispatches the undo command instead of directly mutating
  `clip.masks[0]`.

**Scope estimate:** 100–150 LOC.

**Validation:** generate a SAM mask, Ctrl+Z reverts to prior
state, Ctrl+Shift+Z redoes. Combine with other edits (move the
clip, trim, etc.) to confirm the SAM mask-replace is a single
undo step in the history list.

## Deferred / follow-up work

These are tracked here so nothing gets lost, but are NOT part of
Phase 2b.

- **Phase 2c — CPU memory tuning** — RETIRED. WebGPU sidesteps the
  CPU-path OOM problem entirely on the user's hardware. If a user
  reports CPU-path OOM in the future we can revisit the
  `with_memory_pattern(false)` + `GraphOptimizationLevel::Level1`
  approach, but it's off the critical path.
- **Phase 2d — Session caching across jobs.** The current dispatcher
  loads SAM sessions (~2 s cold start) on every job. A single
  `Arc<Mutex<Option<SamSessions>>>` shared across jobs would
  eliminate the cold-start cost on clicks 2+. Estimated ~50 LOC in
  `sam_job.rs` plus a one-line wire-up in `window.rs`. Low urgency;
  users tolerate the 6 s first click and subsequent clicks rarely
  happen back-to-back in practice.
- **Multi-mask selector UI.** The Inspector currently only displays
  `clip.masks[0]` — appending masks makes them invisible in the UI
  (observed during Phase 2a validation). Option A (replace
  `masks[0]`) is what Phase 2b ships; multi-mask support is a
  larger refactor touching ~10 sites in `inspector.rs` that use
  `masks.first()` / `masks[0]`. Probably Phase 2e or later.
- **Text prompts.** SAM 3's decoder accepts text prompts via the
  language encoder, currently fed a hardcoded `"visual"` placeholder
  token sequence in box-only mode. Adding real text prompts
  requires a CLIP tokenizer dependency plus a text input field in
  the Inspector. Post-Phase 2.
- **MCP handler refactor.** `window.rs`'s `McpCommand::GenerateSamMask`
  arm duplicates the pipeline logic that's now in
  `sam_job::run_sam_pipeline`. Clean-up opportunity: have the MCP
  handler call `run_sam_pipeline` directly (still synchronous on
  the main thread — MCP is automation traffic). ~30-line dedup.
  Not urgent.
- **Dropping the `info!` diagnostic in `segment_with_box`.** Added
  during Phase 2a/4 debugging. Currently logs one line per SAM
  call. Keep for a while to catch future aspect-ratio or shape
  issues; demote to `debug!` when the feature is stable.

## Constraints and gotchas discovered in Phase 2a

These bit me during validation and should be remembered:

1. **SAM 3 rejects loose box prompts.** A box with generous margin
   around the subject (e.g. 12 % × 18 % of frame) returns
   `"decoder returned zero-length scores vector"`. A tight box
   exactly matching the subject extent (e.g. 8 % × 13 %) works.
   Point prompts work because the 8-px emulated box is tight by
   definition. The wkentaro/sam3-onnx reference example uses a
   6.4 %×1.8 % box. **Phase 2b UX must guide users to draw tight
   boxes around single instances**, not loose capture areas. A
   status-bar hint like "Draw a tight box around the subject" or
   a tooltip on the button would help.

2. **The decoder output has aspect-ratio padding.** For a 1920×1080
   source, SAM's decoder outputs at 1920×1080 but content is only
   in the top ~608 rows (scaled from the encoder's 1008×567
   content region within a 1008×1008 tensor). The `segment_with_box`
   rescale uses `padded_w`/`padded_h` from `PreprocessedImage` to
   compute the correct content sub-rectangle. Fixed in `f7f12fe`
   but worth knowing if new SAM exports change the output layout.

3. **The Inspector Masks panel is hardcoded to `masks[0]`.** Any
   code that appends to `clip.masks` produces masks that are
   invisible in the UI. Phase 2b's Option A sidesteps this by
   always replacing `masks[0]` rather than appending. A proper
   multi-mask selector is deferred.

4. **MCP argument name mismatches are silent.** The `add_clip` MCP
   tool's schema requires `timeline_start_ns` / `source_in_ns` /
   `source_out_ns` / `track_index`, but passing `start_ns` /
   `track_id` (my mistake during validation) doesn't produce an
   error — it silently defaults missing fields to 0, creating a
   zero-duration invisible clip. Worth fixing someday in the MCP
   arg parser, but not a Phase 2b blocker.

5. **The ffmpeg single-frame decode pattern uses `-ss` before
   `-i`** for fast keyframe seek. Accurate enough for SAM
   (mask precision is dominated by the model, not by sub-keyframe
   positioning), but worth remembering if we ever need frame-exact
   single-frame extraction elsewhere (e.g. for export thumbnails).

6. **WebGPU is the deployment recipe for cross-vendor GPU.** On
   Intel Arc, AMD, and NVIDIA alike, `cargo build --features
   ai-webgpu` produces a binary that accelerates SAM via a single
   Vulkan path with a ~300 MB prebuilt ort download from pyke CDN.
   No source-build dance required. The native vendor features
   (`ai-cuda` / `ai-rocm` / `ai-openvino`) remain faster on their
   home hardware but harder to build.

7. **The SAM integration test is `#[ignore]`-gated** and self-skips
   when the model isn't installed. Run manually with
   `cargo test --release --features ai-webgpu -- --ignored
   segment_with_box_smoke` on a dev box with ≥128 GB RAM (or GPU)
   to validate after touching anything in `sam_cache.rs` or
   `sam_job.rs`.

8. **SAM session load is not cached.** Every MCP call and every
   future Inspector button click reloads the 2 GB model. Cold
   start is ~2 s on WebGPU plus ~4 s inference = ~6 s total per
   call. Session caching is Phase 2d.

## How to pick this up

If you're a future agent or the user resuming this work:

1. `git fetch && git checkout sam && git pull`
2. Read this file, then read the most recent commit message
   (`git show HEAD`) for detailed context on where the last
   commit left off.
3. Start with **Phase 2b/2** (Inspector button wired to the
   dispatcher). Don't skip ahead to 2b/3 — the hardcoded prompt
   in 2b/2 is the isolation mechanism that lets you test the
   button + polling + mask-replace logic without the added
   complexity of the Program Monitor drag overlay.
4. Before coding, `grep -n "mask_enable\|clip\.masks\.first\|masks\[0\]" src/ui/inspector.rs`
   to find the existing Masks panel access points. Insert the new
   button in the same widget tree; reuse the same `selected_clip`
   accessor the existing mask controls use.
5. When Phase 2b/2 is ready, manually test per the validation
   procedure in this file. Commit. Then start 2b/3.
