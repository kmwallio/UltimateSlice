# Program Monitor

The **Program Monitor** shows the assembled timeline played back in real time, clip by clip.

## Canvas Aspect Ratio

The program monitor constrains its video display area to the **project canvas ratio**
(e.g. 16:9 for a 1920×1080 project). This means:

- If a source clip has a **different aspect ratio** than the canvas (e.g. a 21:9 wide-screen
  clip on a 16:9 canvas), the program monitor will show **black letterbox bars** above and
  below the clip — exactly matching what the exported video will look like.
- If the canvas is wider than the clip (e.g. a 4:3 clip on a 16:9 canvas), black **pillarbox
  bars** appear on the sides.
- The canvas ratio updates automatically when you change the project resolution in
  **Project Settings**.

This makes it much easier to judge clip placement, scale, and whether content is inside
or outside the export frame.

## Controls

| Element | Description |
|---|---|
| Video display | Renders the assembled sequence at the playhead position |
| Timecode label | Current timeline position |
| Play / Pause button | Toggle playback |
| Stop button | Stop and return to position 0 |

## Keyboard Shortcuts

| Shortcut | Action |
|---|---|
| `Space` | Play / Pause (when timeline has focus) |

## Playback Behaviour

- The program monitor uses a GStreamer **compositor** pipeline that layers all active video tracks simultaneously at the playhead position.
- Each active clip gets its own decoder branch with per-clip effects, connected to the compositor with correct z-ordering (higher tracks render on top).
- Audio from all active video clips is mixed through an **audiomixer** element; audio-only tracks use a separate playbin.
- Timeline position is tracked via wall-clock timing for reliable playhead movement — no seek-anchor heuristics needed.
- Audio boundaries are enforced via GStreamer seek stop positions, so audio stops precisely at the clip's source out-point.
- When clip boundaries are crossed during playback (a clip starts or ends), the pipeline is briefly rebuilt with the new set of active clips.
- All per-clip effects (color, denoise, sharpness, crop, rotate, flip, scale, position, title overlay, speed) are applied per-slot during playback.
- Scale/Position edits from the Inspector and transform overlay are applied to the active preview clip immediately in both paused and playing states.
- If optional denoise filters are unavailable in your GStreamer runtime, Program Monitor still applies crop/scale/position transforms.
- Program Monitor normalizes preview output to square pixels (`PAR 1:1`) so 21:9/ultra-wide sources don't keep aspect-ratio bars after zoom scaling.
- Playback priority can be set in **Preferences → Playback** (`Smooth`, `Balanced`, `Accurate`) to control smoothness vs seek precision.
- Proxy preview mode can be enabled in **Preferences → Playback** to generate lightweight proxy files for smoother playback with large media. Export always uses original full-resolution media.
- Preview quality (`Full` / `Half` / `Quarter`) downscales the composed monitor output while preserving full-frame fit/framing in the Program Monitor.
- Preview quality `Auto` dynamically adjusts effective monitor output quality from the current Program Monitor canvas size (including resize/zoom changes) to balance clarity and performance.

## Seeking

- Click on the **ruler** in the timeline to seek the program monitor to that position.
- The program monitor seeks to the correct source position within the appropriate clip, accounting for clip speed.

## Playhead Accuracy

- When you seek and then press Play, UltimateSlice rebuilds the compositor pipeline for the active clips at the playhead position, waits briefly (up to 200 ms) for all decoders to preroll, then transitions to Playing. This ensures playback starts from the correct frame rather than jumping to position 0.

## Speed Change Preview

When a clip has a speed multiplier set (see [inspector.md](inspector.md)), the program monitor plays it at that rate using GStreamer's rate-seek mechanism. Audio pitch is **not** corrected in the preview (it sounds higher/lower pitched). The exported file uses `atempo` for proper pitch correction.
