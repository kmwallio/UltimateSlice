# Source Monitor

The **Source Monitor** previews individual library clips before they are added to the timeline.

## Controls

| Element | Description |
|---|---|
| Video display | Shows the current frame of the selected source clip |
| Scrubber bar | Click to seek; In/Out markers shown in green/orange |
| Timecode label | Current position / total duration |
| In/Out timecode bar | Shows the selected range (In → Out) |
| Play / Pause / Stop | Transport buttons |
| Close (`✕`) | Deselect current media item and hide the Source Monitor |

## Keyboard Shortcuts

| Shortcut | Action |
|---|---|
| `Space` | Play / Pause |
| `I` | Set In-point at current position |
| `O` | Set Out-point at current position |
| `J` | Shuttle reverse — press repeatedly to increase speed (1×, 2×, 4×) |
| `K` | Pause / stop shuttle |
| `L` | Shuttle forward — press repeatedly to increase speed (1×, 2×, 4×) |
| `←` | Step one frame back |
| `→` | Step one frame forward |
| `,` | Insert at playhead (shift subsequent clips) |
| `.` | Overwrite at playhead (replace existing material) |

> **Note:** Shuttle shortcuts require the Source Monitor panel to have keyboard focus. Click inside the monitor area first.

## Setting In/Out Points

1. Play or scrub to the desired start frame.
2. Press **I** (or click **Set In**) — the green marker moves to the current position.
3. Advance to the desired end frame.
4. Press **O** (or click **Set Out**) — the orange marker moves to the current position.

The selected region is highlighted in the scrubber bar.

When dragging the green/orange In/Out markers directly on the scrubber, Source Monitor now seeks to the marker position continuously so the preview frame follows the marker being moved.

## Scrubbing

- Click anywhere on the scrubber to jump to that position.
- Click and drag to scrub continuously.
- Repeated scrubs that land on the same frame are deduplicated internally to avoid redundant decoder seeks.

## Source Playback Priority

Source seek behavior can be tuned in **Preferences → Playback → Source monitor playback priority**:
- `Smooth` / `Balanced`: lighter keyframe seeks for higher responsiveness.
- `Accurate`: frame-accurate seeks for precision-first work.

## Appending to Timeline

After setting In/Out points, click **Append to Timeline** to add the marked range to the timeline. The button auto-detects whether the source is audio-only or contains video.

- Audio-only sources append to a matching audio track.
- Sources with video only append to a matching video track.
- For sources with both video and audio, **Source Monitor A/V auto-link** is configurable:
  - **Enabled**: append creates a linked A/V pair when matching video and audio tracks exist. UltimateSlice places picture on video, sound on audio, links the clips automatically, and mutes the video clip's embedded audio while the linked audio-track peer exists.
  - **Disabled**: append uses single-clip placement behavior.

If an active track of the matching kind is highlighted in the timeline, that track is preferred; otherwise UltimateSlice falls back to the first matching track of that kind.

## Insert and Overwrite Edits

In addition to Append, the Source Monitor provides two 3-point editing operations:

- **⤵ Insert** (or press `,`): Places the marked source range at the current playhead position on the timeline. All clips at or after the playhead are shifted right to make room — a ripple insert.
- **⏺ Overwrite** (or press `.`): Places the marked source range at the current playhead position, replacing any existing timeline material in the time range. Overlapping clips are trimmed, split, or removed as needed.

For eligible sources with both video and audio streams, Insert and Overwrite follow the same optional Source Monitor A/V auto-link behavior:

- **Enabled**: creates a linked A/V pair across matching video and audio tracks, and mutes the video clip's embedded audio while the linked audio-track peer exists.
- **Disabled**: uses single-clip placement behavior.

Both operations target the active track (if its kind matches), or fall back to the first matching track of each required kind. If only one required track kind is available, UltimateSlice falls back to placing a single clip on that available track kind. If no compatible track exists, the operation is skipped. Both support full undo/redo.

## Closing the Source Monitor

- Click the **✕** button in the Source Monitor header to close it.
- Closing clears the current media-library selection, hides the panel, stops source playback, and resets source in/out state.
- Select any media item again to reopen and load the Source Monitor.

## Proxy Preview

When Proxy mode is enabled (`Half Res` or `Quarter Res`), the Source Monitor automatically loads an available proxy for the selected media instead of the full-resolution original. If no proxy exists yet, a proxy transcode is requested in the background and Source Monitor continues using original media until the proxy is ready.

If a selected proxy URI later fails to load/decode, Source Monitor automatically retries once with the original media URI.

When global Proxy mode is **Off**, the Source Monitor stays on original media and does not request proxy transcodes.

## Adaptive Quality

The Source Monitor automatically adapts its internal processing resolution to match the widget size. Instead of processing video at a fixed 1920×1080, the preview pipeline scales to approximately 2× the actual display size (e.g. ~640×360 for a 320×180 widget). This dramatically reduces CPU usage — especially for high-resolution sources — while maintaining sharp visual quality. The resolution updates automatically as the panel is resized.

## Playback Smoothness Policy

During active Source Monitor playback, UltimateSlice prioritizes smooth visual motion by allowing late frames to be dropped under load instead of building latency. When paused or stopped, the player returns to conservative buffering behavior for stable seeking and frame display.

## Hardware Decode Detection and Fallback

When **Enable hardware acceleration** is on, the Source Monitor now checks for available VA-API decoders and prefers a hardware-fast decode path when possible. If the hardware path fails on a given clip (for example due to format negotiation/DMABuf issues), UltimateSlice automatically falls back to the software decode path for that clip and continues playback.
