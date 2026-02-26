# Program Monitor

The **Program Monitor** shows the assembled timeline played back in real time, clip by clip.

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

- The program monitor loads clips from the timeline in order and switches between them automatically at clip boundaries.
- The **timeline playhead** moves in sync with program monitor playback (polled at ~33 ms, with redraw coalescing during playback).
- All per-clip effects (color, denoise, sharpness, crop, rotate, flip, title overlay, speed) are applied during playback.
- Playback priority can be set in **Preferences → Playback** (`Smooth`, `Balanced`, `Accurate`) to control smoothness vs seek precision.
- Proxy preview mode can be enabled in **Preferences → Playback** to generate lightweight proxy files for smoother playback with large media. Export always uses original full-resolution media.

## Seeking

- Click on the **ruler** in the timeline to seek the program monitor to that position.
- The program monitor seeks to the correct source position within the appropriate clip, accounting for clip speed.

## Playhead Accuracy

- When you set the playhead and then press Play, UltimateSlice blocks briefly (up to 100 ms) for the GStreamer pipeline to reach PAUSED state, then re-issues the seek before starting playback. This prevents the common issue of play starting from position 0 after a seek.

## Speed Change Preview

When a clip has a speed multiplier set (see [inspector.md](inspector.md)), the program monitor plays it at that rate using GStreamer's rate-seek mechanism. Audio pitch is **not** corrected in the preview (it sounds higher/lower pitched). The exported file uses `atempo` for proper pitch correction.
