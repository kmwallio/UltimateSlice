# Speed Ramp Keyframe Crash Investigation

## Symptom

SIGSEGV in `gst_qtdemux_push_buffer+1836` (NULL stream dereference at
offset `0x50`) triggered when editing the middle of three speed keyframes
and clicking **Set speed keyframe**. The crash is in GStreamer's
`libgstisomp4.dylib` on thread `qtdemux<N>:sink`, with no application code
on the crashing thread's stack.

## Root Cause

A pre-existing race condition in GStreamer's qtdemux element where the
streaming thread (`gst_qtdemux_loop`) accesses a `QtDemuxStream` pointer
that has become NULL. The crash occurs at the same offset (`0x50`) and
instruction every time, across different qtdemux instances and different
pipelines.

### Why speed keyframe editing triggered it

1. **Flush seeks racing with qtdemux streaming threads.**
   `update_speed_keyframes_for_clip` originally called
   `reseek_slot_for_current()`, which flushed the compositor and sent
   `FLUSH | ACCURATE` seeks to ALL decoder slots. Each slot's `uridecodebin`
   contains a qtdemux whose streaming thread could race with the flush event,
   leading to a NULL stream dereference.

2. **Thumbnail extraction pipeline teardown.**
   Speed keyframe changes trigger timeline redraws, which request new
   thumbnails. The thumbnail cache spawns up to 4 concurrent `extract_rgba`
   threads, each creating an independent GStreamer pipeline with
   `uridecodebin` (which creates qtdemux for MP4 files). When these
   pipelines are torn down (`PipelineGuard::drop`), the same qtdemux race
   can fire.

3. **Spontaneous occurrence during normal playback.**
   The same crash signature also appears in the program player's own
   qtdemux instances during normal paused-state operation, with no
   application code running. This confirms the bug is internal to GStreamer.

## Changes Made

### 1. Remove reseek from speed keyframe updates

**File:** `src/media/program_player.rs` — `update_speed_keyframes_for_clip`

The method no longer calls any GStreamer pipeline seek when speed keyframes
change. It only updates the in-memory `speed`, `speed_keyframes`, and
`source_out_ns` fields on the `ProgramClip`. The new source-position
mapping takes effect on the next natural seek (playhead scrub, play/pause,
timeline boundary rebuild).

This eliminates the primary application-triggered path to the crash.

### 2. Wait for Null state in PipelineGuard

**File:** `src/media/mod.rs` — `PipelineGuard::drop`

Added a `state(ClockTime::from_seconds(5))` call after `set_state(Null)` so
the drop blocks until the pipeline has fully transitioned to Null and all
streaming threads have stopped. Previously the pipeline could be freed while
qtdemux threads were still running.

This protects the thumbnail extraction teardown path.

## What remains unfixed

The crash can still occur spontaneously inside GStreamer's qtdemux during
normal program player operation. In all observed instances:

- The crashing thread is purely GStreamer library code
  (`gst_qtdemux_loop` → `gst_qtdemux_decorate_and_push_buffer` →
  `gst_qtdemux_push_buffer`)
- No application code is on the stack
- The main thread is idle in the GTK event loop
- Register `x21` (stream pointer) is `0x0`, and the fault address is `0x50`
  (offset of a field within the stream struct)

This is a GStreamer upstream bug. A report should be filed against the
`gst-plugins-good` isomp4 plugin with:

- **Platform:** macOS 26.4 (25E5233c), ARM64 (Apple M1 Max)
- **GStreamer:** Homebrew gstreamer (gstreamer-rs 0.25)
- **Crash signature:** `gst_qtdemux_push_buffer+1836`, KERN_INVALID_ADDRESS
  at 0x50, `x21=0x0`
- **Reproduction:** Multiple concurrent `uridecodebin` pipelines decoding
  the same or different H.264 MP4 files; crash occurs during normal paused
  streaming or during pipeline Null transitions
