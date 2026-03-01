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

8. **`GstAggregator` flush propagation is ALL-or-nothing**: Flush events
   sent to individual aggregator sink pads are NOT propagated downstream
   until ALL pads have received FLUSH_START. If one pad (e.g., a background
   `videotestsrc`) is never flushed, downstream elements (tee, appsink)
   never see the flush and retain stale data.

9. **`uridecodebin` pad exposure depends on proxy file validity**: A zero-byte
   proxy file causes the internal `decodebin2` to reach Paused but never
   expose pads. Always validate proxy file size > 0, not just existence.

10. **`glib::MainContext::iteration(false)` is NOT safe from GTK callbacks**:
    Calling it from within a GTK signal handler or callback causes a
    `RefCell` double-borrow panic. Use `glib::timeout_add_local` instead
    to defer work to the next main loop iteration.

---

## Multi-Track Frozen-Frame Investigation (Feb–Mar 2026, Ongoing)

### Problem

After the above optimizations, paused scrubbing (MCP `seek_playhead` +
`export_displayed_frame`) shows **frozen frames** — the same image at every
timeline position when 3+ video tracks overlap.  1-track is inconsistent
(sometimes frozen), 2-track is fixed, 3-track is reliably frozen.

### Automated MCP Test Methodology

A Python script (`test_multitrack_preview.py`) drives the app via `--mcp`
(stdio JSON-RPC):

1. Open `three-video-tracks.fcpxml`
2. Seek to 7 positions across 1-track (2 s, 4 s), 2-track (7 s, 8 s),
   and 3-track (10 s, 11 s, 12 s) regions
3. Export 320×180 RGBA frames (PPM) at each position
4. Compare SHA-256 hashes — distinct hashes mean distinct frames

**Important**: A 5-second delay after `open_fcpxml` is needed for proxy
resolution and pipeline construction to complete before seeking.

### Root Cause Chain (Multi-layered)

The frozen-frame bug has **three distinct root causes** that compound:

#### Root Cause 1: Zero-Byte Proxy File (FIXED ✅)

The proxy cache (`proxy_cache.rs`) checked only `Path::exists()` when
validating pre-existing proxy files, accepting zero-byte files as valid.
The Screencast file's `proxy_half.mp4` was 0 bytes (from a previously
failed transcode), so `uridecodebin` was trying to decode an empty file,
which can never produce a pad-added signal.

**Fix**: Changed proxy validation to `std::fs::metadata(&p).map_or(false, |m| m.len() > 0)`.
Also added post-transcode verification to delete zero-byte output files.

This fix resolved the **3rd decoder pad-linking failure** — all 3 decoders
now successfully fire `pad-added` and link video.

#### Root Cause 2: GTK Main-Thread Deadlock (FIXED ✅)

The "playing pulse" pattern (`set_state(Playing)` → `pipeline.state(timeout)` →
`set_state(Paused)`) blocks the GTK main thread while waiting for the pipeline
to reach Playing.  `gtk4paintablesink` needs the GTK main thread to complete
its Paused-to-Playing preroll.  This creates a **deadlock**:

```
Main thread:  pipeline.state(300ms)  [BLOCKED waiting for all children → Playing]
    ↓ needs
gtk4paintablesink:  needs GTK main loop iteration to complete preroll
    ↓ needs
Main thread:  must return to GTK main loop [BLOCKED]
```

**Fix**: Split the playing pulse into two phases:
- `start_playing_pulse()` — locks audio sink, sets Playing, returns immediately
- `complete_playing_pulse()` — called from GTK timeout after main loop runs

After this fix, the pipeline successfully reaches Playing for 3+ tracks.
The `scope_frame_seq` counter now increments during playback pulses.

#### Root Cause 3: Compositor Flush Propagation (UNDER INVESTIGATION)

Even with all 3 decoders linked and the pipeline reaching Playing, the
**compositor output is identical at different seek positions**.  The
scope appsink receives preroll buffers with changing PTS values, but the
actual pixel data (verified by content hashing in the appsink callback)
is identical across seeks to 10s, 11s, and 12s.

**Mechanism**: When individual decoders are flush-seeked, each sends
`FLUSH_START`/`FLUSH_STOP` to its compositor pad.  However, the
background `videotestsrc` pad (compositor pad 0) is never flushed.
`GstAggregator` only propagates flush downstream when ALL sink pads are
flushed.  Since the background pad is never flushed, the compositor never
sends `FLUSH_START` downstream to the tee/appsink branch.  The appsink
gets stale preroll data from the compositor's previous aggregation.

**Complication**: Flushing the background `videotestsrc` alongside the
decoders causes a regression — the compositor clears ALL retained buffers
(including decoder frames in transit), resulting in black frames or
re-using stale data.

### Pipeline State Observations (from diagnostic logging)

| Scenario | After rebuild | After seek + playing_pulse | Notes |
|----------|--------------|---------------------------|-------|
| 1 track | `Paused, seq=2` | `Playing→Paused, seq increments` | Works after proxy fix |
| 2 tracks | `Paused, seq=4` | `Playing→Paused, seq increments` | Works |
| 3 tracks | `Paused, seq=5` | `Playing→Paused, seq increments` | Seq increments but pixel data unchanged |

### Audio Sink Contribution

`autoaudiosink` (PulseAudio backend) is slow to connect during Ready→Paused,
compounding the delay for 3+ tracks.  **Fix applied**: Lock audio sink state
(`set_locked_state(true)`) during `playing_pulse` for 3+ tracks to remove
PulseAudio from the critical path.

### Experiments Attempted (20+)

| # | Approach | Result |
|---|----------|--------|
| 1 | `thread::sleep(50ms)` replacing `pipeline.state()` | Blocks main thread, no state transition |
| 2 | `glib::MainContext::iteration(false)` | Reentrancy crash (`RefCell` double-borrow) |
| 3 | Bus `timed_pop_filtered(StateChanged)` | Returns on first StateChanged (not AsyncDone) |
| 4 | Background videotestsrc flush + decoder flushes | 2-track regression (compositor clears ALL pad buffers) |
| 5 | Async export with `glib::timeout_add_local` polling | 2-track improved, 3-track still frozen |
| 6 | Force rebuild for 3+ track seeks | Black frames (Ready state kills videotestsrc preroll) |
| 7 | Audio sink locking alone | Pipeline reaches Playing for ≤2, still stuck for 3+ |
| 8 | Skip playing_pulse, spin-wait on `scope_frame_seq` | seq never increments (compositor doesn't re-preroll) |
| 9 | Lock display sink + audio during playing_pulse | Pipeline still stuck at `Async(Ready→Playing)`, black frames |
| 10 | Flush background videotestsrc alongside decoder seeks | Black frames; compositor clears all buffers |
| 11 | `tee` with `allow-not-linked=true` | No effect — tee wasn't the blocker |
| 12 | Non-blocking async playing pulse (Phase 0) | Pipeline reaches Playing ✅, but frames frozen (Root Cause 3) |
| 13 | Increase `wait_for_video_links` timeout to 2500ms | 3rd decoder still stuck (was proxy issue, not timing) |
| 14 | Queue between effects_bin and compositor | No effect on pad linking |
| 15 | `uridecodebin3` swap | 3rd decoder reaches Paused but no pad-added (same proxy issue) |
| 16 | Staggered decoder initialization (sequential Paused) | 3rd decoder still stuck at Ready→Paused (proxy issue) |
| 17 | Set slot chain (effects_bin, queue) to Paused explicitly | No improvement — decoder itself stuck (proxy issue) |
| 18 | Background flush AFTER decoder seeks in seek_slots_in_place | Regression — all positions produce same hash |
| 19 | Restructured export: complete_playing_pulse before polling | Preroll fires but pixel data still identical |
| 20 | Content hashing in appsink callbacks | Confirmed: all preroll buffers have identical pixel data |

### What Works (After Fixes Applied)

- **Zero-byte proxy validation** ✅ — all 3 decoders link correctly
- **Non-blocking Playing pulse** ✅ — pipeline reaches Playing for 3+ tracks
- **Audio sink locking** ✅ — removes PulseAudio from the critical path
- **2-track compositing** ✅ — flush-seeking works correctly
- **`wait_for_paused_preroll()`** ✅ — per-decoder waits avoid deadlock
- **Scope appsink callbacks fire** ✅ — preroll and sample events arrive

### What's Broken

- **3-track seek produces identical pixel data** — the compositor output
  doesn't reflect the new decoder positions after flush seeks.
- **Root cause 3** (compositor flush propagation) is the remaining blocker.

### Remaining Investigation Directions

1. **Pipeline-level seek**: Instead of per-decoder flush seeks, seek the
   entire pipeline — this sends FLUSH atomically through all elements
   including the background videotestsrc.  Requires careful handling to
   avoid clearing the compositor's retained buffers.
2. **Segment event injection**: After per-decoder flush seeks, manually
   inject a `FLUSH_START`/`FLUSH_STOP` event pair on the background pad
   to trigger full downstream flush propagation without losing decoder
   buffers.
3. **Direct compositor pad probe**: Install a buffer probe on the
   compositor `src` pad to capture the composited frame directly, bypassing
   the tee/queue/appsink path.
4. **Dedicated export pipeline**: Create a separate pipeline for frame
   export without `gtk4paintablesink`, eliminating the main-thread
   dependency entirely.
5. **Pipeline-level flush seek**: Send a single FLUSH|ACCURATE seek on the
   pipeline (not individual decoders), using a segment event with the
   correct per-decoder offsets.

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
2. **Start with MCP**: `target/release/ultimate-slice --mcp`
3. **Run the MCP test script**:
   ```bash
   python3 test_multitrack_preview.py
   ```
   Or manually via MCP JSON-RPC:
   ```python
   call('open_fcpxml', {'path': '.../Sample-Media/three-video-tracks.fcpxml'})
   call('seek_playhead', {'timeline_pos_ns': 10_000_000_000})
   call('export_displayed_frame', {'path': '/tmp/frame_10s.ppm'})
   ```
4. **Compare frame hashes** — different SHA-256 = different frames.

To re-add the `[PERF]` instrumentation, add `log::info!` timings around the
`rebuild_pipeline_at()`, `build_effects_bin()`, and `poll()` methods in
`src/media/program_player.rs`, plus a buffer probe on the compositor `src` pad
incrementing an `Arc<AtomicU64>` frame counter logged each `poll()` tick.
