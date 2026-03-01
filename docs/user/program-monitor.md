# Program Monitor

The **Program Monitor** shows the assembled timeline played back in real time, clip by clip.

When no timeline clips are present, the monitor area shows a short first-use hint to import media and append/insert clips.

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

## Docked Resize

- When the Program Monitor is docked, you can drag the splitter between the preview and scopes area to resize how much space each gets.
- If scopes are hidden, the scopes pane is fully collapsed (the splitter/pane disappears).
- The docked splitter position is saved and restored on next launch.

## Keyboard Shortcuts

| Shortcut | Action |
|---|---|
| `Space` | Play / Pause (when timeline has focus) |
| `←` / `→` / `↑` / `↓` | Nudge selected clip position in transform overlay (0.01) |
| `Shift + Arrow` | Coarse nudge selected clip position (0.1) |
| `+` / `-` | Increase / decrease selected clip scale in transform overlay |

## Transform Overlay Controls

When a timeline clip is selected, the Program Monitor overlay provides direct transform editing:

- **Corner handles**: drag to scale; hold **Shift** for constrained scaling.
- **Center drag**: pan (Position X/Y).
- **Edge midpoint handles**: drag top/bottom/left/right handles to adjust crop directly in preview.
- Keyboard nudges work when the overlay has focus (click the monitor once).

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
- During playback boundary handoffs (when the active clip set changes because a clip starts/ends), UltimateSlice uses accurate decoder seeks so long-GOP proxy media does not jump to an earlier keyframe.
- Proxy preview mode can be enabled in **Preferences → Playback** to generate lightweight proxy files for smoother playback with large media. Export always uses original full-resolution media.
- Preview quality (`Full` / `Half` / `Quarter`) downscales the composed monitor output while preserving full-frame fit/framing in the Program Monitor.
- Preview quality `Auto` dynamically adjusts effective monitor output quality from the current Program Monitor canvas size (including resize/zoom changes) to balance clarity and performance.

## Seeking

- Click on the **ruler** in the timeline to seek the program monitor to that position.
- The program monitor seeks to the correct source position within the appropriate clip, accounting for clip speed.
- When scrubbing within the same clip, the existing decoder is seeked in-place (no pipeline rebuild) so the monitor shows the frame at the exact playhead position without a black-screen or first-frame flash.
- When the playhead crosses a clip boundary (different clips become active), the pipeline is briefly rebuilt for the new set of active clips.
- During paused scrubbing, UltimateSlice waits for a fresh post-seek preroll frame so the Program Monitor and transform overlay update to the new playhead frame instead of showing black.
- During paused scrubbing, active clip decoder branches are created before preroll/seek settle so the monitor does not remain stuck on a black frame after moving the playhead.
- Manual timeline seeks use the paused accurate-seek path and then resume playback if it was active, so the frame shown at the playhead is updated before playback continues.
- While paused, the monitor is repainted continuously so delayed post-seek frame updates still appear without requiring playback to resume.

## Playhead Accuracy

- When you seek and then press Play, UltimateSlice rebuilds the compositor pipeline for the active clips at the playhead position and waits for post-seek preroll (up to ~2 seconds in paused accurate mode for long-GOP media) before transitioning back to Playing. This ensures playback starts from the correct frame rather than jumping to position 0.
- During active playback boundary handoffs, preroll waits are tuned for responsiveness (shorter than paused scrubbing waits) to reduce visible stutter while preserving accurate clip positioning.

## Speed Change Preview

When a clip has a speed multiplier set (see [inspector.md](inspector.md)), the program monitor plays it at that rate using GStreamer's rate-seek mechanism. Audio pitch is **not** corrected in the preview (it sounds higher/lower pitched). The exported file uses `atempo` for proper pitch correction.

## MCP Automation

- `seek_playhead` seeks the timeline/program-monitor playhead to an absolute nanosecond position.
- `export_displayed_frame` exports the current displayed frame to a binary PPM (`P6`) image file.
- `take_screenshot` captures a PNG screenshot of the full application window using the GTK snapshot API and GSK `CairoRenderer`. The PNG is written to the current working directory as `ultimateslice-screenshot-<unix_epoch>.png`.
