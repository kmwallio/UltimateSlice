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

After setting In/Out points, click **Append to Timeline** to add the marked range to the end of the first Video track.

## Closing the Source Monitor

- Click the **✕** button in the Source Monitor header to close it.
- Closing clears the current media-library selection, hides the panel, stops source playback, and resets source in/out state.
- Select any media item again to reopen and load the Source Monitor.
