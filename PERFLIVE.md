# Live Preview Pipeline: Performance Investigation & Decisions

This document records the investigations, experiments, and outcomes from the
live preview performance work that followed the initial 3-track profiling
(documented in PERFSUMMARY.md). The focus here is on **pipeline pacing,
backpressure, and audio-less clip handling** — issues that only surfaced once
the initial decode/effects bottleneck was resolved.

---

## Timeline

| Phase | Commit | Outcome |
|-------|--------|---------|
| Baseline (great playback) | `9e03b18` | 1–2 tracks smooth, 3 tracks ~0 FPS deadlock |
| Leaky q1 fix | (session) | 3 tracks unblocked → 91–95 FPS compositor, but… |
| FPS regression investigation | (session) | Display gets 0–5 FPS despite compositor producing 1000+ FPS |
| is-live + framerate caps | `67c5e3f` | Compositor paced at 30 FPS, but 1–2 track quality worse |
| Preview-resolution compositing | `ce7ff9f` | Reduced render work, but quality too low |
| **Revert to baseline + audio fix** | `beca636` | Keeps great playback, fixes audio-less stall |

---

## Phase 1: 3-Track Deadlock (Leaky Queue)

### Problem

With three or more overlapping video tracks, playback stalled at 0 FPS after
an initial burst of ~12 frames.

### Root Cause

`gtk4paintablesink` renders on the GTK main thread. During `poll()` or
pipeline rebuild, the main thread blocks. The non-leaky display queue (`q1`,
default `max-size-buffers=200`) filled up → backpressure propagated through
the `tee` → compositor blocked → all decoder slot queues blocked — a full
pipeline deadlock.

### Fix Applied

Changed `q1` to `leaky=downstream, max-size-buffers=2` so it drops old frames
instead of blocking.

### Result

3-track playback went from **0 FPS to 91–95 FPS** (compositor output).
Per-slot decoder→compositor queues remained non-leaky to preserve frame
accuracy on 1–2 track projects.

### Why This Was Reverted

The leaky queue *unblocked* the pipeline but removed backpressure entirely.
In a non-live GStreamer pipeline, the sink is the only element that provides
rate-limiting through backpressure. Without it, the compositor ran uncapped at
1000+ FPS. The leaky queue dropped 99%+ of frames, and `gtk4paintablesink`
received only 0–5 frames per second for display. Visual playback quality
degraded significantly for 1–2 track projects.

---

## Phase 2: Compositor FPS Investigation

### Diagnostic Instrumentation

Added temporary counters to measure the actual throughput:

- **`compositor_frame_seq`**: `Arc<AtomicU64>` incremented by a buffer probe on
  the compositor `src` pad. Counted total frames produced.
- **`display_frame_seq`**: `Arc<AtomicU64>` incremented by a buffer probe on the
  `q1` `src` pad. Counted frames actually delivered to `gtk4paintablesink`.
- **`[DIAG]` logging in `poll()`**: Every 2 seconds, logged compositor FPS,
  display FPS, frames dropped, slot count, and preview resolution.

### Measurements (with leaky q1, no is-live)

| Scenario | Compositor FPS | Display FPS | Frames Dropped/2s | Preview |
|----------|---------------|-------------|-------------------|---------|
| 1 slot   | ~1200         | 0–0.5       | ~2400             | 480×270 |
| 2 slots  | ~150          | 0.5         | ~600              | 480×270 |
| 1 slot (boundary) | 60–130 | 0.5–5.5 | 100–270           | 480×270 |

### Analysis

In a **non-live** GStreamer pipeline:

1. All elements produce buffers as fast as possible (limited only by CPU).
2. The sink element provides rate-limiting through backpressure — it consumes
   one buffer per display interval (e.g., 33 ms for 30 FPS) and blocks upstream.
3. Making `q1` leaky **removed** this backpressure. The compositor no longer
   waited for the sink; it just kept producing.
4. `gtk4paintablesink` could only render when the GTK main loop ran (between
   `poll()` ticks, ~33 ms apart). In each window it picked up 1 buffer and
   the queue had already dropped the rest.

**Key insight**: Leaky queues in non-live pipelines break the fundamental
rate-limiting mechanism. The compositor runs uncapped and the display starves.

---

## Phase 3: `is-live=true` on Background Sources

### Hypothesis

Setting `is-live=true` on the `videotestsrc` (black, compositor background)
would force the compositor's `GstAggregator` into live aggregation mode. In
live mode, the aggregator uses the system clock to pace output at the
negotiated framerate, regardless of how fast decoders produce.

### Implementation

```rust
// videotestsrc (compositor background)
let black_src = gst::ElementFactory::make("videotestsrc")
    .property_from_str("pattern", "black")
    .property("is-live", true)  // ← added
    .build()?;

// audiotestsrc (audiomixer background)
let silence_src = gst::ElementFactory::make("audiotestsrc")
    .property_from_str("wave", "silence")
    .property("is-live", true)  // ← added
    .build()?;
```

Also added `framerate=30/1` to the `comp_capsfilter`, `preview_capsfilter`,
and `black_capsfilter` to ensure the negotiated rate was explicit.

### Result

The compositor was indeed paced at ~30 FPS. The display received frames at a
reasonable rate. The leaky queue rarely needed to drop frames.

### Why This Was Reverted

While `is-live=true` solved the FPS pacing problem in theory, it introduced
subtle regressions on 1–2 track projects:

1. **Paused preroll behavior changed**: Live sources behave differently during
   `PAUSED → PLAYING` transitions. The compositor's preroll semantics shifted,
   making seek-and-display-one-frame (used for scrubbing) less reliable.
2. **Visual quality felt worse**: The fixed 30 FPS pacing from the live source
   competed with the decoders' natural framerate (24 FPS for GoPro content).
   Frame timing mismatches caused periodic frame doubling or drops.
3. **The original pipeline (9e03b18) already worked well** for 1–2 tracks —
   the non-live backpressure mechanism was the correct pacing approach. The
   only problem was the 3-track deadlock caused by the audio-less screencast
   clip, which stalled the audiomixer.

---

## Phase 4: Framerate Capsfilters

### Change

Added `framerate=30/1` to:
- `comp_capsfilter` (compositor output)
- `preview_capsfilter` (final output before tee)
- `black_capsfilter` (background source)
- `apply_compositor_caps()` (dynamic re-caps)

### Why It Didn't Help (Without is-live)

In a non-live pipeline, capsfilters negotiate framerate but **do not enforce
it**. The `framerate` field is used for caps negotiation (downstream elements
know what rate to expect) but doesn't throttle buffer flow. The compositor
still produces as fast as decoders feed it.

### Why It Was Reverted

The capsfilters were part of the is-live approach. Without is-live, they added
no value and risked unexpected caps negotiation side effects (e.g., forcing
renegotiation when preview quality changed).

---

## Phase 5: Preview-Resolution Compositing

### Hypothesis

If the preview is displayed at 480×270 (divisor=4), compositing at full
1920×1080 wastes ~16× the pixel work. Processing at preview resolution saves
GPU/CPU proportionally.

### Implementation

- `build_effects_bin()` received `render_width/render_height` (divided by
  preview_divisor) instead of `project_width/project_height`.
- `capsfilter_proj` and `capsfilter_zoom` set to render resolution.
- `apply_compositor_caps()` set compositor and background to preview resolution.
- `apply_transform_to_slot()` received `preview_divisor` and scaled crop values.
- `apply_zoom_to_slot()` received preview-resolution dimensions.
- Default preview quality changed from `Full` to `Auto`.

### Why It Was Reverted

1. **Crop precision loss**: Integer division of crop pixels by the divisor
   introduced rounding errors. A 10px crop at divisor=4 became 2px, losing
   fine control.
2. **Quality degradation visible at all times**: With the `Auto` default,
   users got Half/Quarter resolution without opting in. The preview looked
   noticeably worse even for simple 1-track projects.
3. **The root cause was audio, not compositor cost**: Once the audio-less clip
   fix was applied, the baseline pipeline (full-resolution compositing, non-leaky
   queue, non-live sources) played back smoothly. The preview-resolution
   optimization solved the wrong problem.
4. **Export unaffected but scrubbing degraded**: While export uses ffmpeg (not
   the preview pipeline), paused scrubbing quality depended on the preview
   resolution, making frame-accurate editing harder.

---

## Phase 6: Audio-Less Clip Root Cause (The Actual Fix)

### Discovery

Probing the test media with ffprobe revealed:

| File | Audio Streams | Video FPS | Resolution |
|------|--------------|-----------|------------|
| GX010429.MP4 (GoPro) | 1 (AAC 48kHz) | 24 fps fixed | 5320×2280 |
| GX010430.MP4 (GoPro) | 1 (AAC 48kHz) | 24 fps fixed | 5320×2280 |
| Screencast.mp4 | **0 (none)** | ~7–12 fps VFR | 3000×1642 |

The screencast has **no audio stream**.

### Root Cause

The slot builder always created an `audioconvert` element and requested an
`audiomixer` sink pad for every clip, regardless of whether the source had
audio. For the screencast:

1. `audioconvert` was created and linked to an `audiomixer` sink pad.
2. `uridecodebin` never fired `pad-added` for audio (no audio stream exists).
3. The `audioconvert` sink pad remained unlinked — no data flowed.
4. The `audiomixer` (`GstAggregator`, non-live) waited for **all** sink pads to
   produce a buffer before aggregating.
5. The unfed pad blocked the audiomixer indefinitely.
6. The audiomixer's blocked output prevented the audio sink from consuming,
   stalling the pipeline clock.
7. With 3 tracks, this stall cascaded into the compositor (shared pipeline
   clock), causing the 0 FPS deadlock.

### Fix (Kept)

Two changes, both retained in the final codebase:

**1. `is-live=true` on the background `audiotestsrc`**

```rust
let silence_src = gst::ElementFactory::make("audiotestsrc")
    .property_from_str("wave", "silence")
    .property("is-live", true)
    .build()?;
```

This makes the audiomixer operate in live aggregation mode. In live mode, the
aggregator uses clock-based timeouts instead of waiting indefinitely for all
pads. An unfed pad is treated as "late" and skipped rather than blocking.

**2. Skip audiomixer pad for audio-less clips**

Added `probe_has_audio_stream()` using GStreamer Discoverer to detect whether a
source file contains an audio stream. The slot builder now skips the
`audioconvert → audiomixer` path entirely for clips without audio:

```rust
let clip_has_audio = Self::probe_has_audio_stream(&effective_path);
let (audio_conv, amix_pad) = if clip_has_audio {
    // ... create audioconvert + audiomixer pad ...
} else {
    log::info!("skipping audio path for clip {} (no audio)", clip.id);
    (None, None)
};
```

Added `has_audio` field to `ProgramClip` (default `true`) and `ProbeResult` in
`probe_cache.rs`.

### Result

With only this fix applied on top of the baseline (`9e03b18`), playback is
smooth for all track counts. The non-live compositor with backpressure works
correctly because the audiomixer no longer stalls the pipeline clock.

---

## Key Takeaways

### 1. Non-Live Pipelines Need Backpressure

GStreamer's non-live pipeline model relies on sink backpressure for rate
control. Making display queues leaky removes this mechanism and causes the
compositor to run uncapped. **Never make the display queue leaky unless the
pipeline is live.**

### 2. `is-live=true` Changes Aggregator Semantics

Setting `is-live=true` on a source forces downstream aggregators into live
mode. This changes:
- Preroll behavior (affects paused scrubbing)
- Latency negotiation
- Timeout-based pad aggregation (vs. wait-for-all)

Use `is-live=true` only when live behavior is actually needed (e.g., the
audiomixer with potentially unlinked pads).

### 3. Capsfilter `framerate` Does Not Enforce Rate

In non-live mode, `framerate=30/1` in a capsfilter is advisory — it affects
negotiation but not buffer flow. Only live sources or `videorate` elements
actually enforce framerate.

### 4. Diagnose Before Optimizing

The 3-track stall appeared to be a compositor throughput issue. Multiple
rounds of optimization (leaky queues, live sources, resolution reduction)
addressed symptoms. The actual root cause was a single audio-less clip
creating an unfed audiomixer pad. A targeted fix (skip audio path + live
audiomixer) solved the problem without any quality trade-offs.

### 5. Preview-Resolution Compositing Has Trade-Offs

While processing at preview resolution (e.g., 480×270 instead of 1920×1080)
reduces per-frame cost by up to 16×, it introduces:
- Crop value precision loss (integer division rounding)
- Degraded scrubbing/paused frame quality
- User-visible quality regression with `Auto` default

This optimization may be worth revisiting for an explicit "Performance"
preview mode, but should not be the default behavior.

---

## Reverted Changes Reference

For future reference, the reverted changes are preserved in git history:

| Change | Commit | Why Reverted |
|--------|--------|-------------|
| Leaky q1 (`leaky=downstream, max-size-buffers=2`) | `67c5e3f` | Removed backpressure; compositor ran uncapped at 1000+ FPS |
| `is-live=true` on `videotestsrc` (compositor) | `67c5e3f` | Changed preroll/scrubbing semantics; 1–2 track quality worse |
| `framerate=30/1` on capsfilters | `67c5e3f` | No effect without is-live; removed as unnecessary |
| `cseq.fetch_add` before scope check | `67c5e3f` | Diagnostic correctness fix; reverted with diagnostics |
| Preview-resolution compositing | `ce7ff9f` | Crop precision loss; quality degradation; wrong problem |
| Auto preview quality default | `ce7ff9f` | Quality regression without user opt-in |
| Crop scaling by preview_divisor | `ce7ff9f` | Integer rounding lost fine crop control |
| Zoom/position scaled to render resolution | `ce7ff9f` | Coupled to preview-resolution compositing |

## Retained Changes

| Change | Commit | Why Kept |
|--------|--------|---------|
| `is-live=true` on `audiotestsrc` (audiomixer) | `beca636` | Prevents audiomixer stall on unlinked pads |
| `probe_has_audio_stream()` | `beca636` | Detects clips without audio streams |
| Skip audio path for audio-less clips | `beca636` | Eliminates unfed audiomixer pads entirely |
| `has_audio` on ProgramClip/ProbeResult | `beca636` | Supports audio detection plumbing |
