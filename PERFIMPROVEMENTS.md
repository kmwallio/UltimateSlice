# Multi-Track Scalability: Architectural Improvement Plan

This document proposes a phased architectural roadmap to improve multi-track playback
performance beyond the three-track baseline established in `PERFSUMMARY.md`, targeting
real-time playback of **6+ simultaneous video tracks**.

Cross-references to ROADMAP items are noted inline.

---

## Architectural Decisions

### GES (GStreamer Editing Services): Skip

**Recommendation: Do not adopt `gstreamer-editing-services`.**

The custom compositor pipeline in `program_player.rs` already solves the core scheduling
problem GES would address, with more direct control. GES would require a full rewrite of
`ProgramPlayer` (~2,300 lines), replacing `compositor`/`audiomixer` with
`GESTimeline`/`GESLayer`/`gnlcompositor`. The Rust GES bindings are less mature than the
core `gstreamer` crate, and `gnlcompositor` is functionally equivalent to the existing
`compositor` element.

More critically, GES abstracts away the compositor pad properties (`zorder`, `alpha`,
`position_x/y`, `width/height`) that the custom pipeline uses directly for compositing.
Recovering equivalent per-clip control through GES requires `GESVideoSource` + custom
`GESEffect` layers — more code than the status quo with no capability gain.

The only GES benefit (automatic clip scheduling at boundaries) is addressed in Phase 2
by the incremental slot manager. **Revisit after Phase 3 only if timeline complexity
grows substantially.**

---

### Threading Architecture: Keep 33ms Timer, Defer Async Thread to Phase 3

The 33ms `glib::timeout_add_local` heartbeat (`window.rs:679`) is lightweight when no
rebuild occurs. The problem is `rebuild_pipeline_at()` (~130ms) executing synchronously
inside `poll()`, freezing the GTK main loop at every clip boundary.

**Phases 1–2:** Keep the timer as-is. Boundary rebuilds happen synchronously but are
reduced dramatically in cost by Phase 2A (incremental slot management).

**Phase 3:** Move boundary rebuilds to `glib::idle_add_local_once()` to defer them past
the current timer tick without a separate thread. For fully off-thread pipeline management,
use a dedicated `std::thread` + `glib::MainContext::channel()` for events back to GTK.
`gst::Pipeline` and `gst::Element` are `Send+Sync`, so pipeline mutations can run
off-thread. The constraint: `gtk4paintablesink` must be created on the GTK main thread;
`video_sink_bin` state transitions must be dispatched via GTK idle.

**Verdict on glib channels:** `glib::MainContext::channel()` is the right tool for
Phase 3, not for Phases 1–2. The bottleneck is CPU/GPU decode throughput and rebuild
granularity, not channel overhead.

---

## Phase 1 — Quick Wins

All changes are independent and can be applied in any order.

### 1A. Switch `uridecodebin` → `uridecodebin3`

**File:** `src/media/program_player.rs`, `rebuild_pipeline_at()` ~line 1877

One-line factory name swap:

```rust
// Before:
let decoder = gst::ElementFactory::make("uridecodebin")
// After:
let decoder = gst::ElementFactory::make("uridecodebin3")
```

`uridecodebin3` uses `decodebin3` internally, which has improved concurrent stream
scheduling and is required for reliable hardware decoder autoplugging via rank promotion.
The existing `pad-added` and `deep-element-added` callbacks are fully compatible — no
other changes needed.

**Expected impact:** Better concurrent stream scheduling; prerequisite for 1B.

---

### 1B. Hardware Decode Rank Promotion

**ROADMAP:** `[ ] Hardware-accelerated decoding/encoding (VA-API, NVENC)`

**Files:** `src/media/program_player.rs`, `src/media/player.rs`, `src/ui/preferences.rs`

Add `fn set_hw_decoder_ranks(enabled: bool)` that promotes hardware video decoder
elements above the software fallback (`avdec_h265` rank = PRIMARY = 256):

```rust
fn set_hw_decoder_ranks(enabled: bool) {
    use gstreamer::prelude::PluginFeatureExtManual;
    let registry = gst::Registry::get();
    let rank = if enabled { gst::Rank::PRIMARY + 100 } else { gst::Rank::NONE };
    for name in &[
        "vulkanh265dec", "vulkah265dec", "vah265dec", "vaapih265dec",  // H.265
        "vulkah264dec",  "vah264dec",    "vaapih264dec",                // H.264
    ] {
        if let Some(feat) = registry.find_feature(name, gst::ElementFactory::static_type()) {
            feat.set_rank(rank);
            log::info!("set {} rank → {:?}", name, rank);
        }
    }
}
```

- Call from `ProgramPlayer::new()` respecting the existing `hardware_acceleration_enabled`
  preference.
- Expose as `pub fn set_hardware_acceleration_enabled(&mut self, enabled: bool)`.
- Wire the existing hardware acceleration toggle in `src/ui/preferences.rs` to this
  method (currently it only controls `glsinkbin` in `player.rs`).
- Also call from `src/media/player.rs` on construction (source monitor uses `playbin`
  and benefits equally).

**Key detail:** `vulkanh265dec` outputs `video/x-raw(memory:VulkanImage)`. `decodebin3`
automatically inserts `vulkandownload` to convert back to system-memory NV12 before the
effects bin. No changes to `build_effects_bin()` are needed.

**Detecting fallback:** In the `deep-element-added` callback (~line 1975), if `avdec_h265`
appears while hardware decode is enabled, log a warning — the driver is falling back to
software.

**Expected impact:** 5.3K H.265 decode offloaded to AMD RX 6800 XT GPU; frees ~10–12
CPU threads per track. 3-track region: ~6 fps → ~18–24 fps. 6-track: ~12–18 fps.

---

### 1C. Audio Pan via `audiopanorama`

> **Note on ROADMAP discrepancy:** The ROADMAP marks `[x] Volume / pan controls per
> clip in the inspector (sliders, GStreamer volume + audiopanorama, persisted in FCPXML)`.
> However, `update_current_audio()` (line 964) has `_pan: f64` (underscore = unused)
> and `audiopanorama` does not appear anywhere in `program_player.rs`. This still needs
> to be implemented.

**File:** `src/media/program_player.rs`

`ProgramClip.pan` is stored but ignored. The `audiomixer` sink pad has no pan property —
pan requires a separate `audiopanorama` element in the per-slot audio chain.

**Changes:**

1. Add `audio_panorama: Option<gst::Element>` to `VideoSlot` struct (~line 123).
2. In `rebuild_pipeline_at()` (~line 1927), extend the audio path:
   ```
   uridecodebin (audio pad) → audioconvert → audiopanorama → audiomixer sink pad
   ```
   Set `panorama` property from `clip.pan` at construction.
3. Add teardown of `audio_panorama` in `teardown_slots()` (~line 1492): flush, set
   state Null, remove from pipeline.
4. In `update_current_audio()` (~line 964), apply pan live:
   ```rust
   if let Some(ref ap) = slot.audio_panorama {
       ap.set_property("panorama", pan.clamp(-1.0, 1.0));
   }
   ```

`audiopanorama.panorama` takes `-1.0` (full left) to `1.0` (full right), matching
`ProgramClip.pan` semantics. The existing `audioconvert` already produces stereo, so
caps negotiation is clean.

**Expected impact:** Completes audio pan (~40 lines). No performance effect.

---

### 1D. `videorate drop-only` per Slot

**File:** `src/media/program_player.rs`, `build_effects_bin()` ~line 1762

Append a `videorate` element at the end of the effects chain (after `videobox_zoom`):

```rust
let rate_cap = gst::ElementFactory::make("videorate")
    .property("drop-only", true)
    .property("max-rate", project_frame_rate as i32)
    .build()
    .ok();
if let Some(ref e) = rate_cap {
    chain.push(e.clone());
}
```

Add `project_frame_rate: u32` to `ProgramPlayer` alongside `project_width/height`.

Without `drop-only=true`, `videorate` can duplicate frames — we only want dropping.
With it, a decoder that can only sustain 15 fps still sends real frames at 15 fps
rather than stalling the compositor's `GstAggregator` (which waits indefinitely for a
buffer from every connected sink pad, effectively dropping output to zero fps).

**Expected impact:** In heavy regions, output becomes the minimum-sustainable FPS of all
slots instead of dropping to zero. ~15 lines.

---

## Phase 2 — Scalability

### 2A. Incremental Slot Management ← highest UX impact

**File:** `src/media/program_player.rs`

Currently `poll()` calls `rebuild_pipeline_at()` for any clip-set change, tearing down
and rebuilding all slots regardless of how many clips actually changed. Cost scales
linearly with clip count.

**New functions:**

```rust
fn compute_slot_diff(desired: &[usize], current: &[usize]) -> (Vec<usize>, Vec<usize>)
// Returns (to_remove, to_add) as Vec of clip_idx, using HashSet symmetric diff

fn add_slot(&mut self, clip_idx: usize, zorder: u32, timeline_pos: u64) -> Result<VideoSlot>
// Build one uridecodebin3 + effects_bin, add to pipeline, sync_state_with_parent, seek to pos

fn remove_slot(&mut self, slot_idx: usize)
// Send EOS on compositor pad → flush → set state Null → remove elements → release pads
// Does NOT touch remaining slots or the pipeline state

fn update_slots_incrementally(&mut self, timeline_pos: u64)
// 1. Compute diff (desired vs current slots)
// 2. If empty diff: return immediately (hot path — most ticks take this branch)
// 3. pipeline.set_state(Paused)  — ~5ms
// 4. remove_slot() for each departing clip
// 5. add_slot()    for each entering clip
// 6. Update zorder on unchanged slots if stacking order shifted (live pad property set)
// 7. pipeline.set_state(Playing)
```

**Modify `poll()`** (~line 914): replace the `rebuild_pipeline_at()` call with
`update_slots_incrementally()`.

Keep `rebuild_pipeline_at()` for: cold start (no slots), loading a new project, and
seek from Stopped state.

**Key implementation detail:** New slots start at running-time 0. They must be seeked to
`timeline_pos` while paused (before step 7) or the compositor receives frames from
position 0 instead of the current playhead.

**EOS before removal:** Before removing a departing slot, send EOS on its compositor
sink pad so `GstAggregator` releases it cleanly without waiting for a buffer that will
never arrive.

**Expected impact:** Boundary transition cost: 1 slot change ≈ 50 ms vs current 130 ms.
No-op boundaries (continuous playback through a region with unchanged clips) = 0 ms.
The 130ms stutter at every clip boundary is eliminated for typical edits.

---

### 2B. Proxy Workflow One-Click Toggle

**ROADMAP:** `[ ] Proxy Workflow: One-click toggle between original and proxy media`;
`[ ] Proxy media generation and management`

**Files:** `src/ui/window.rs` (toolbar area)

The proxy infrastructure is complete (`proxy_cache.rs`, path resolution in
`rebuild_pipeline_at()` ~lines 1847–1860, 4 ffmpeg worker threads). What's missing is
a discoverable toggle.

**Changes:**
- Add a "Proxy" toggle button to the toolbar or program monitor header.
- On enable: call `set_proxy_enabled(true)` + `rebuild_pipeline_at(current_pos)`.
- On disable: reverse and rebuild.
- Show proxy generation progress in the status bar (already plumbed via `ProxyProgress`).
- Optional: auto-enable when project has 3+ overlapping video tracks (configurable
  threshold in `PreferencesState`).

No changes to `program_player.rs` or `proxy_cache.rs` needed.

**Expected impact:** Immediate 4–6× CPU reduction in editing mode via half/quarter-res
proxies. Completes the ROADMAP item.

---

### 2C. `glvideomixer` (GPU Compositor) — with software fallback

**File:** `src/media/program_player.rs`

Replace `compositor` with `glvideomixer` to move compositing from CPU to GPU.
`glvideomixer` has the same sink pad properties (`zorder`, `alpha`, `xpos`, `ypos`,
`width`, `height`) as `compositor`.

**Pipeline changes:**

```
// Before:
effects_bin → compositor → comp_capsfilter → videoconvert_out → videoscale_out → ...

// After (GL path):
effects_bin → [glupload] → glvideomixer → gloverlaycompositor → comp_capsfilter_gl → ...
```

1. At `ProgramPlayer::new()` (~line 324): try `glvideomixer`, fall back to `compositor`.
   Store `compositor_type: CompositorType` (`Gl` or `Software`).
2. In `build_effects_bin()` (~line 1762): when GL compositor, append `glupload` at end
   of effects chain to upload system-memory RGBA to GL memory.
3. Replace `videoconvert_out` with `gloverlaycompositor`. PERFSUMMARY documents that
   `videoconvert` is required as a caps bridge for `GstVideoOverlayComposition` meta —
   `gloverlaycompositor` is the GL-memory equivalent.
4. Change `comp_capsfilter` caps to `video/x-raw(memory:GLMemory)` on the GL path.
5. The scope branch (`appsink` at 320×180) needs system memory: add `gldownload` before
   the `tee` split in `video_sink_bin`.

**GL context sharing:** `glvideomixer` and `gtk4paintablesink` share the same GL context
when in the same pipeline — GStreamer handles this automatically. Test with a 3-track
project before shipping; keep the software fallback path unconditionally.

**Effects elements are unaffected:** `videobalance`, `gaussianblur`, `videoflip`,
`videocrop`, `videobox`, `videoconvertscale` all operate on system memory. The `glupload`
at the end of `effects_bin` is the sole upload boundary.

**Expected impact:** GPU compositing of 6 tracks at 1080p costs ~0 CPU (vs. measurable
overhead with software compositor). Combined with Phase 1B hardware decode: 6-track
region targeting 20–24 fps.

---

## Phase 3 — Prerendering & Frame Cache

These two features directly address open ROADMAP items and provide the best experience
for heavy timelines where real-time decode is not feasible even with hardware acceleration.

### 3A. Background Region Prerendering

**ROADMAP:** `[ ] Background rendering for complex effect stacks`

**Goal:** In regions with N overlapping tracks, composite them once to a render file in
the background. Subsequent playback of those regions decodes a single H.264/H.265 file
rather than N live streams — the render file is a "pre-composited proxy" for the whole
region.

**Architecture:**

```rust
struct RenderCache {
    segments: BTreeMap<(u64, u64), RenderSegment>,  // (start_ns, end_ns) → segment
    work_tx:   mpsc::Sender<RenderJob>,
    result_rx: mpsc::Receiver<RenderResult>,
    render_dir: PathBuf,    // .ultimateslice_renders/ next to project file
}

struct RenderSegment {
    file_path: String,
    valid:       bool,   // false = stale (contributing clip was edited)
    in_progress: bool,
}

struct RenderJob {
    start_ns: u64,
    end_ns:   u64,
    clips:    Vec<ProgramClip>,
    project_width: u32, project_height: u32,
    frame_rate: u32,
    output_path: String,
}
```

**Render workers:** Reuse the existing ffmpeg export infrastructure (the
`-filter_complex overlay` chain already in the export path). Each `RenderJob` runs
`ffmpeg` writing to a temp `.mp4` in `.ultimateslice_renders/`. Pool size: 1–2 workers
(rendering competes with playback; cap to avoid starving the decoder).

**Trigger logic:** After `poll()` detects a region with 3+ overlapping clips, queue a
`RenderJob` for that region if no valid render exists. Priority: region around the
current playhead first, then expand outward.

**Playback integration:** In `rebuild_pipeline_at()`, before building slot decoders,
check `RenderCache::get(timeline_pos_ns)`. If a valid render covers this range, use a
single `uridecodebin3` pointed at the render file — the same substitution pattern as
proxy mode. Seek into the render file as:
```
seek_offset = timeline_pos_ns - segment.start_ns
```

**Staleness invalidation:** Wire to the `on_project_changed` callback in `window.rs`:
mark all segments containing a modified clip as `valid = false` and re-queue them.

**Timeline indicator:** Draw a colored bar above the affected timeline region in
`src/ui/timeline/mod.rs`:
- Green = valid render exists
- Orange/yellow = render in progress
- None = not yet rendered

This follows the Final Cut Pro / Premiere "render bar" pattern.

**Storage:** Renders in `.ultimateslice_renders/` next to the project file. Persist
across sessions; clear on project close. Add a "Clear render cache" button in Preferences.

**New file:** `src/media/render_cache.rs` (~300 lines)

---

### 3B. Frame Cache Around Playhead

**ROADMAP:** `[ ] Add short frame cache around playhead (previous/current/next frames)
to reduce stutter on scrubbing and pause/seek`

**Goal:** When scrubbing (paused playhead movement), display cached frames instantly
for positions already decoded, without triggering new decoder seeks.

**Architecture:**

```rust
struct FrameCache {
    frames: BTreeMap<u64, Vec<u8>>,  // timeline_pos_ns (rounded to frame) → RGBA data
    max_frames: usize,               // e.g. 120 frames (5s at 24fps)
    frame_width: u32,
    frame_height: u32,
}
```

**Capture:** Add a buffer probe on the compositor `src` pad. When the probe fires, store
the RGBA buffer in `FrameCache` keyed by `buf.pts() + base_timeline_ns`. Evict frames
furthest from the current playhead when `max_frames` is exceeded.

**Resolution:** Cache at the preview display resolution (the `preview_capsfilter`
resolution), not full 1080p. At `Half` quality (960×540 RGBA), 120 frames ≈ 59 MB.
At `Quarter` quality (480×270), 120 frames ≈ 15 MB. Gate capture on the same
`scope_enabled` AtomicBool pattern to avoid allocation when the program monitor is
hidden.

**Lookup in seek fast-path:** In `seek_slots_in_place()`, before issuing the decoder
seek, check `FrameCache::get(timeline_pos_ns ± half_frame_ns)`. If a cached frame
exists, deliver it to the display immediately via the existing scope-frame mechanism
(push to `latest_scope_frame`, increment `scope_frame_seq`), then issue the decoder
seek in the background.

**Expected impact:** Scrubbing feels instantaneous for frames already seen. Closes the
`[ ] Add short frame cache around playhead` ROADMAP item.

---

### 3C. Async Rebuild via `glib::idle_add_local_once`

Even with incremental slot management (Phase 2A), a ~50ms boundary rebuild still runs
synchronously in the 33ms timer tick, causing an occasional dropped frame. Moving it to
idle resolves this.

**File:** `src/ui/window.rs`, 33ms poll timer closure (~line 679)

Change `poll()` return type to:
```rust
pub struct PollResult {
    pub position_changed: bool,
    pub rebuild_needed:   bool,
}
```

In the 33ms timer closure, replace the direct rebuild call:

```rust
let result = player.poll();
if result.rebuild_needed && !is_rebuilding.get() {
    is_rebuilding.set(true);
    let pp2 = pp.clone();
    let flag = is_rebuilding.clone();
    let pos = player.timeline_pos_ns;
    glib::idle_add_local_once(move || {
        pp2.borrow_mut().update_slots_incrementally(pos);
        flag.set(false);
    });
}
```

Add `is_rebuilding: Rc<Cell<bool>>` in the timer setup closure.

**Expected impact:** 33ms heartbeat stays < 1ms even at clip boundaries. UI position
label, VU meter, and scope frames continue updating during rebuilds.

---

## Summary: ROADMAP Alignment

| ROADMAP Item | Plan Section | Phase |
|---|---|---|
| `[ ] Hardware-accelerated decoding/encoding` | 1B HW Rank Promotion | 1 |
| `[ ] Audio pan (audiopanorama)` *(ROADMAP `[x]` inaccurate — code is incomplete)* | 1C Audio Pan | 1 |
| `[ ] Proxy Workflow: One-click toggle` | 2B Proxy UX | 2 |
| `[ ] Add short frame cache around playhead` | 3B Frame Cache | 3 |
| `[ ] Background rendering for complex effect stacks` | 3A Background Prerendering | 3 |

---

## Expected Performance Projections

| Optimization | 3-Track FPS | 6-Track FPS |
|---|---|---|
| Baseline (current, post-PERFSUMMARY) | ~6 fps | ~2 fps (est.) |
| 1A: uridecodebin3 | ~8 fps | ~4 fps |
| 1B: vulkanh265dec HW decode | ~18–24 fps | ~12–18 fps |
| 1D: videorate drop-only | stable at 1B fps | stable |
| 2A: Incremental slot management | same fps, no boundary stutter | same fps |
| 2C: glvideomixer GPU compositor | 24+ fps | 20–24 fps |
| 3A: Background prerendering | 30 fps (rendered regions) | 30 fps (rendered regions) |
| 3B: Frame cache | instant scrub response | instant scrub response |

The dominant wins are **Phase 1B (hardware decode)** and **Phase 2C (GPU compositing)**.
**Phase 2A** is the most important for perceived smoothness (no boundary stutter).
**Phase 3A prerendering** is the escape hatch for 6+ tracks: renders complex regions
offline so playback costs only a single-stream H.264 decode.

---

## Critical Files

| File | Changes |
|---|---|
| `src/media/program_player.rs` | 1A uridecodebin3, 1B HW rank promotion, 1C audiopanorama, 1D videorate, 2A incremental slot management, 2C glvideomixer |
| `src/media/render_cache.rs` *(new)* | 3A `RenderCache`, `RenderJob`, `RenderSegment`, worker thread |
| `src/ui/window.rs` | 2B proxy toolbar UX, 3C async rebuild pattern (`PollResult`, `idle_add_local_once`) |
| `src/ui/timeline/mod.rs` | 3A timeline render bar indicator (green/orange) |
| `src/ui/preferences.rs` | 1B wire HW acceleration toggle; 3A "Clear render cache" button |
| `src/media/player.rs` | 1B call `set_hw_decoder_ranks()` on construction |

---

## Implementation Sequence

**Week 1 (Phase 1):**
1. `uridecodebin3` swap (30 min)
2. `set_hw_decoder_ranks()` + preference wiring (2–3 h); verify in `deep-element-added`
   logs that `vulkanh265dec` or `vah265dec` appears
3. `audiopanorama` audio path (2 h)
4. `videorate drop-only` (1 h)

**Weeks 2–3 (Phase 2A):**
5. `add_slot`, `remove_slot`, `compute_slot_diff`, `update_slots_incrementally` (8–12 h)
6. Replace rebuild call in `poll()`; regression-test all clip-boundary scenarios

**Week 4 (Phase 2B–C):**
7. Proxy toolbar UX (2–3 h)
8. `glvideomixer` with software fallback (8–12 h; keep fallback unconditionally)

**Month 2 (Phase 3):**
9. `PollResult` + `idle_add_local_once` async rebuild (3C, 4–6 h)
10. Frame cache via compositor buffer probe (3B, 6–8 h)
11. `RenderCache` + render workers + timeline render bar (3A, 2–3 weeks)

---

## Verification

1. **Build**: `cargo build --release`
2. **HW decode**: `GST_DEBUG=3 ./target/release/ultimateslice 2>&1 | grep deep-element`
   — confirm `vulkanh265dec` or `vah265dec` appears for H.265 tracks; CPU usage drops
   dramatically vs. baseline.
3. **3-track test**: Open `Sample-Media/three-video-tracks.fcpxml`, seek to ~10 s overlap
   region, play. Target: ≥ 24 fps (was ~6 fps).
4. **Boundary smoothness**: Play through a clip boundary. Verify no visible freeze
   (incremental slot management). Check log output for rebuild time per boundary.
5. **Audio pan**: Set `pan = -1.0` on a clip, verify audio is left-channel only; set
   `pan = 1.0`, verify right-channel only.
6. **Proxy toggle**: Enable proxy mode toolbar button; verify lower CPU usage; disable,
   verify full-res.
7. **GL fallback**: If `glvideomixer` is unavailable, verify `CompositorType::Software`
   fallback works without error.
8. **6-track stress test**: FCPXML with 6 overlapping 1080p H.264 clips; target ≥ 24 fps.
9. **Frame cache**: Scrub back and forth over a decoded region; second pass should
   display instantly (no decoder seek latency).
10. **Prerender**: Create 4+ overlapping tracks, let background render complete (green
    bar in timeline), then play; verify single-stream decode cost on CPU/GPU monitor.
