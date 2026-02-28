# Performance Profiling & Optimization Summary

This document records the profiling methodology, analysis, and optimizations
applied to the GStreamer compositor pipeline in UltimateSlice during the
3-track playback performance investigation (Feb 2026).

---

## Problem Statement

When playing a timeline with three overlapping video tracks (two 5.3K H.265
GoPro clips + one 3K H.264 screencast), the compositor produced only **~1–2
fps** — far below the 24 fps needed for real-time playback. One- and two-track
regions played back at 17–30 fps without issue.

### Test Project

`Sample-Media/three-video-tracks.fcpxml` — 3 video tracks + 1 audio track:

| Track | File | Codec | Resolution | Bitrate |
|-------|------|-------|------------|---------|
| V1 | GX010429.MP4 | H.265 Main 10 | 5320×2280 | ~95 Mbps |
| V2 | GX010430.MP4 | H.265 Main 10 | 5320×2280 | ~93 Mbps |
| V3 | Screencast.mp4 | H.264 High 4:4:4 | 3000×1642 | ~2.4 Mbps |
| A1 | Music | AAC | — | — |

Three-clip overlap window: **~9.17 s → ~13.46 s**.

### Hardware

- **CPU**: Intel i7-12700KF (8 Performance + 4 Efficiency = 20 threads)
- **GPU**: AMD Radeon RX 6800 XT
- **RAM**: 32 GB DDR5

---

## Profiling Methodology

### 1. Instrumentation (in-app)

Added `[PERF]` timing instrumentation throughout the pipeline rebuild path in
`program_player.rs`:

- **`poll()`** — total poll time per tick, clip-boundary detection cost.
- **`rebuild_pipeline_at()`** — overall rebuild time, broken down into:
  - Teardown slots (old decoders/effects)
  - Build slots loop (per-slot element creation + linking)
  - Flush/seek per decoder
  - Post-seek decoder settle
- **`build_effects_bin()`** — element creation and linking time per clip.
- **Compositor frame counter** — `Arc<AtomicU64>` bumped by a buffer probe on
  the compositor `src` pad; logged every `poll()` tick for effective FPS.

All instrumentation was gated behind `log::info!` and removed after analysis.

### 2. Isolated GStreamer Benchmarks (CLI)

To separate decode cost from conversion/scaling cost, standalone GStreamer
pipelines were benchmarked outside the application using `gst-launch-1.0`:

```bash
# Decode only (H.265 → fakesink, no sync)
gst-launch-1.0 filesrc location=GX010429.MP4 ! qtdemux ! h265parse \
  ! avdec_h265 max-threads=4 ! fakesink sync=false

# Decode + separate convert + scale (→ RGBA 1920×1080)
gst-launch-1.0 filesrc location=GX010429.MP4 ! qtdemux ! h265parse \
  ! avdec_h265 max-threads=4 ! videoconvert \
  ! videoscale ! "video/x-raw,format=RGBA,width=1920,height=1080" \
  ! fakesink sync=false

# Decode + combined videoconvertscale (→ RGBA 1920×1080)
gst-launch-1.0 filesrc location=GX010429.MP4 ! qtdemux ! h265parse \
  ! avdec_h265 max-threads=4 ! videoconvertscale \
  ! "video/x-raw,format=RGBA,width=1920,height=1080" \
  ! fakesink sync=false
```

Each pipeline was also tested with 3 concurrent instances (via `&` and `wait`)
to simulate real multi-track load.

### 3. GStreamer Debug Tracing

Used `GST_DEBUG` environment variable for targeted subsystem logs:

- `aggregator:5` — compositor pad enqueue/consume cycles to identify stalls.
- `GST_DEBUG=2` (warnings+errors) — caps negotiation failures
  (`not-negotiated`) when experimenting with element removal.

### 4. MCP Socket Automation

Automated open → play → pause → seek sequences via the MCP JSON-RPC socket at
`/run/user/1000/ultimateslice-mcp.sock` using a Python client, avoiding manual
UI interaction during timed runs.

---

## Benchmark Results

### Single-Pipeline Throughput (5.3K H.265 Main 10, 31.6 s clip)

| Pipeline | Wall Time | Throughput | Notes |
|----------|-----------|------------|-------|
| Decode → fakesink | 8.4 s | **~90 fps** (3.75× RT) | H.265 decode alone is fast |
| Decode → videoconvert → videoscale → RGBA 1080p | 60+ s | **~12 fps** (0.5× RT) | `videoconvert` at 5.3K is the bottleneck |
| Decode → videoconvertscale → RGBA 1080p | 39.3 s | **~19 fps** (0.81× RT) | 58% faster than separate elements |

### 3× Concurrent Pipeline Throughput

| Pipeline | Per-Stream FPS | Concurrency Penalty |
|----------|---------------|---------------------|
| 3× videoconvert + videoscale | **~7 fps** each | 42% drop from single |
| 3× videoconvertscale | **~18.5 fps** each | 3% drop from single |

**Key insight**: `videoconvertscale` avoids allocating an intermediate
full-resolution RGBA buffer between the convert and scale steps, reducing both
memory bandwidth and CPU time dramatically under concurrency.

### Application-Level Compositor FPS

| Region | Before Optimization | After Optimization |
|--------|--------------------|--------------------|
| 1 clip (H.265 5.3K) | ~17 fps | ~30 fps |
| 2 clips (2× H.265 5.3K) | ~19 fps | ~30 fps |
| 3 clips (2× H.265 + H.264) | **~1–2 fps** | **~6 fps** |
| Rebuild time (3 slots) | ~384 ms | **~130 ms** |

### Pipeline Build Time Breakdown

| Phase | Before | After |
|-------|--------|-------|
| Build slots loop (3 slots) | 293 ms | 65 ms |
| Teardown slots | ~40 ms | ~40 ms |
| Flush/seek per decoder | ~15 ms | ~15 ms |
| Total rebuild | 384 ms | 130 ms |

---

## Root Cause Analysis

### Primary Bottleneck: `videoconvert` at Source Resolution

The effects chain originally applied `videoconvert` (NV12/I420_10LE → RGBA)
at the **full source resolution** (5320×2280 = 12.1 Mpx/frame) before any
downscaling. This single operation consumed **~87% of per-frame processing
time** per clip. With three concurrent clips all converting at 5.3K, the CPU
was completely saturated on colour conversion alone.

### Secondary Bottleneck: Separate Convert + Scale Elements

GStreamer's `videoconvert` + `videoscale` used as separate elements requires:

1. Decode output (NV12 5.3K) → allocate RGBA 5.3K intermediate buffer
2. Convert NV12 → RGBA at 5.3K (12.1 Mpx × 4 bytes = 48 MB/frame)
3. Copy RGBA 5.3K into videoscale input
4. Scale RGBA 5.3K → 1080p (2 Mpx)

The intermediate 48 MB/frame buffer allocation and the full-resolution RGBA
write are eliminated by the combined `videoconvertscale` element, which does
colour conversion and scaling in a single pass.

### Tertiary Issue: Tee Backpressure

The `tee` element in `video_sink_bin` pushes synchronously to both the display
branch (`glsinkbin/gtk4paintablesink`) and the scope branch
(`videoscale → videoconvert → appsink` at 320×180). When the scope queue
filled up, it blocked the tee, which in turn blocked the display path.

### Tertiary Issue: No-Op Effects Elements

Every clip always created the full effects chain (~17 GStreamer elements) even
when most effects were at their default/no-op values. Element creation is
expensive in GStreamer (~20–30 ms per element for some types), and unnecessary
elements add scheduling overhead.

---

## Optimizations Applied

### 1. Replace `videoconvert` + `videoscale` with `videoconvertscale`

Single combined element does NV12→RGBA colour conversion and 5.3K→1080p
downscaling in one pass, avoiding the intermediate full-resolution RGBA buffer.

**Impact**: 2.6× faster per-stream (3× concurrent: ~18.5 fps each vs ~7 fps).

### 2. Early Downscale (Before Effects)

Moved the `videoconvertscale` + resolution capsfilter to the **start** of the
effects chain, so all subsequent effects (balance, blur, rotate, flip, overlay)
process at project resolution (1920×1080) instead of source resolution
(5320×2280).

**Impact**: Effects processing 6× fewer pixels per frame.

### 3. Conditional Element Creation

Skip GStreamer element creation for effects at their default/no-op values:

| Element | Skipped When |
|---------|-------------|
| `videocrop` | All crop margins = 0 |
| `videobalance` + `videoconvert` | brightness=0, contrast=1, hue=0, saturation=1 |
| `gaussianblur` + `videoconvert` | sigma = 0 |
| `videoflip` (rotate) + `videoconvert` | rotation = 0° |
| `videoflip` (flip) + `videoconvert` | no horizontal/vertical flip |
| `textoverlay` | no title text |
| `alpha` | always skipped (opacity handled by compositor pad property) |

**Impact**: Build slots loop 293 ms → 65 ms (4.5× faster).

### 4. Leaky Scope Queue

Made the scope branch queue (`q2` inside `video_sink_bin`) leaky with
`max-size-buffers=1, leaky=downstream`. Dropped frames in the scope branch
no longer block the display path via tee backpressure.

**Impact**: Eliminates display stalls caused by slow waveform/scope rendering.

### 5. Decoder Thread Tuning

Set explicit thread limits on software decoders during playback:

- `avdec_h265`: `max-threads=4`
- `avdec_h264`: `max-threads=2`

Balances decode parallelism across available CPU cores without over-subscribing.

---

## Failed Experiments

### Removing `videoconvert_out` / `videoscale_out` from Output Chain

**Hypothesis**: Since the compositor already outputs RGBA 1080p (matching the
`comp_capsfilter`), the downstream `videoconvert_out` and `videoscale_out`
elements are no-ops and can be removed.

**Result**: Pipeline produced 0 frames. `GST_DEBUG=2` revealed:
`videotestsrc0: streaming stopped, reason not-negotiated (-4)`.

**Root cause**: `glsinkbin` (wrapping `gtk4paintablesink`) requires the
`meta:GstVideoOverlayComposition` caps feature on its sink pad templates. The
compositor outputs plain `video/x-raw` without this meta. `videoconvert` acts
as a bridge element that accepts both plain and meta-annotated caps, enabling
negotiation. **It must remain in the output chain.**

### Adding a Queue Between Compositor and Capsfilter

**Hypothesis**: A decoupling `queue` after the compositor would let the
compositor aggregate thread run independently of downstream sink sync.

**Result**: Same `not-negotiated` error. The `queue` element doesn't properly
forward `GstVideoOverlayComposition` meta feature caps queries upstream.

**Conclusion**: Do NOT place a `queue` between the compositor and its
downstream `videoconvert` bridge.

### NV12-First Downscale (`videoscale` Before `videoconvert`)

**Hypothesis**: Scaling in the decoder's native NV12 format before converting
to RGBA would avoid the expensive full-resolution colour conversion entirely.

**Result**: Pipeline hung at 275% CPU. GStreamer could not negotiate caps when
`videoscale` preceded `videoconvert` with H.265 10-bit output (I420_10LE).

**Conclusion**: `videoconvert` must come before `videoscale` (or use the
combined `videoconvertscale` element).

---

## GStreamer Pipeline Lessons Learned

1. **`pipeline.state()` deadlocks the GTK main thread** when
   `gtk4paintablesink` needs the main context for Paused preroll. Wait on
   individual decoder elements instead.

2. **STREAM_LOCK ordering**: Streaming threads hold pad locks while pushing
   through the compositor. Calling `set_state(Null)` on upstream elements
   requires those same locks for pad deactivation → deadlock. **Always flush
   pads first** before state transitions.

3. **`videoconvert` is a caps negotiation bridge** — it forwards
   `GstVideoOverlayComposition` meta features that `queue` elements do not.
   Required between compositor and `glsinkbin`.

4. **`videoconvertscale` vs `videoconvert` + `videoscale`**: The combined
   element is dramatically faster under concurrency (2.6×) because it avoids
   an intermediate full-resolution RGBA buffer allocation per frame.

5. **`videoscale` before `videoconvert` doesn't work** with 10-bit decoder
   output formats. Always convert colour format first (or use the combined
   element).

6. **Tee elements push synchronously** to all branches. A slow branch blocks
   all other branches unless the branch queue is set to leaky mode.

7. **Non-live compositor (`GstAggregator`)** waits indefinitely for one buffer
   from every connected sink pad. Send EOS on unlinked or stalled pads to
   prevent permanent pipeline stalls.

---

## Future Optimization Opportunities

| Opportunity | Expected Impact | Complexity |
|-------------|----------------|------------|
| Hardware-accelerated decode (`vulkanh265dec` rank promotion) | Offload H.265 decode to GPU; frees ~10 CPU threads | Low |
| GL compositor (`glvideomixer`) | GPU-accelerated compositing instead of CPU | Medium |
| Proxy resolution mode for playback | Decode at half/quarter res during editing | Low (already has infra) |
| `videorate` frame dropping for 3+ clips | Drop to 15 fps in heavy regions | Low |
| Incremental slot management | Only add/remove changed slots at clip boundaries, avoid full pipeline rebuild | High |
| Frame cache around playhead | Cache previous/current/next decoded frames for scrub | Medium |

---

## Reproducing the Profiling

1. **Build release**: `cargo build --release`
2. **Start with MCP**: launch the app and enable the MCP socket via
   Preferences → General → Enable MCP Server.
3. **Open the test project** via MCP:
   ```python
   call('open_fcpxml', {'path': '.../Sample-Media/three-video-tracks.fcpxml'})
   ```
4. **Seek to the 3-clip region** (~10 s) and play.
5. **Observe** compositor frame throughput in the Program Monitor.

To re-add the `[PERF]` instrumentation, add `log::info!` timings around the
`rebuild_pipeline_at()`, `build_effects_bin()`, and `poll()` methods in
`src/media/program_player.rs`, plus a buffer probe on the compositor `src` pad
incrementing an `Arc<AtomicU64>` frame counter logged each `poll()` tick.
