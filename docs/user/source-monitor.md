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

## Scrubbing

- Click anywhere on the scrubber to jump to that position.
- Click and drag to scrub continuously.

## Appending to Timeline

After setting In/Out points, click **Append to Timeline** to add the marked range to the timeline.  The button auto-detects whether the source is audio-only or contains video.  If an active track of the matching kind is highlighted in the timeline, the clip is appended there; otherwise it goes to the first track of that kind.

## Insert and Overwrite Edits

In addition to Append, the Source Monitor provides two 3-point editing operations:

- **⤵ Insert** (or press `,`): Places the marked source range at the current playhead position on the timeline. All clips at or after the playhead are shifted right to make room — a ripple insert.
- **⏺ Overwrite** (or press `.`): Places the marked source range at the current playhead position, replacing any existing timeline material in the time range. Overlapping clips are trimmed, split, or removed as needed.

Both operations target the active track (if its kind matches), or fall back to the first matching track. Both support full undo/redo.

## Closing the Source Monitor

- Click the **✕** button in the Source Monitor header to close it.
- Closing clears the current media-library selection, hides the panel, stops source playback, and resets source in/out state.
- Select any media item again to reopen and load the Source Monitor.

## Proxy Preview

When a proxy file exists for the selected media (see [Preferences → Proxy Preview](preferences.md)), the Source Monitor automatically loads the proxy instead of the full-resolution original. If no proxy exists yet, a proxy transcode is requested in the background; once it completes, the player reloads with the proxy automatically. This ensures smooth preview playback even with high-resolution footage (e.g. 5.3K GoPro HEVC) without any manual steps.
