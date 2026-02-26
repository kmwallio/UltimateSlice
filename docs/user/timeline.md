# Timeline

The **Timeline** panel (bottom) is where you arrange, trim, and edit clips into your final sequence.

## Layout

- **Ruler** — shows time positions with adaptive tick marks; click to seek the playhead.
- **Track rows** — each track (Video or Audio) shows clips as coloured rectangles.
- **Playhead** — the red vertical line indicates the current playback position.
- **Track header** — shows the track name; click the mute button to silence an audio track.

## Navigation

| Action | How |
|---|---|
| Seek | Click on the ruler or drag the playhead |
| Zoom in/out | Scroll the mouse wheel vertically |
| Pan left/right | Scroll the mouse wheel horizontally |

## Tools

### Select Tool (`Escape`)

The default tool. Use it to:
- **Select** a clip by clicking on it (highlighted yellow border).
- **Move** a clip by dragging its body.
- **Trim** the In-point by dragging the left edge of a selected clip.
- **Trim** the Out-point by dragging the right edge of a selected clip.

Snapping: clip edges snap to nearby clip boundaries (±10 px threshold) while moving or trimming.

### Razor / Blade Tool (`B`)

- Click on a clip body to **split** it at the playhead position.
- Press **B** or **Escape** to toggle back to Select tool.

## Keyboard Shortcuts

| Shortcut | Action |
|---|---|
| `Space` | Play / Pause program monitor |
| `B` | Toggle Razor (Blade) tool |
| `Escape` | Switch to Select tool |
| `Delete` / `Backspace` | Delete selected clip |
| `M` | Add chapter marker at current playhead position |
| `Right-click ruler` | Remove the nearest marker |
| `Ctrl+Z` | Undo |
| `Ctrl+Y` / `Ctrl+Shift+Z` | Redo |
| `Scroll (vertical)` | Zoom timeline |
| `Scroll (horizontal)` | Pan timeline |
| `?` / `/` | Show in-app keyboard shortcut reference |

## Chapter Markers

- Press **M** to drop a marker at the playhead — a label dialog allows you to name it.
- Markers appear as coloured flags on the ruler with their label.
- Right-click on the ruler to remove the nearest marker.
- Markers are exported in the FCPXML file.

## Tracks

- **Add Track** buttons below the timeline add a new Video or Audio track.
- Audio tracks show a waveform visualisation (decoded in the background after import).
- Muting an audio track excludes it from both preview and export.

## Undo / Redo

All clip moves, trims, splits, and deletions are undoable.

- `Ctrl+Z` — Undo
- `Ctrl+Y` or `Ctrl+Shift+Z` — Redo

The undo history is per-session (not persisted in the FCPXML).

## Clip Appearance

- Clips show a thumbnail strip (extracted in the background) for video tracks.
- Audio clips show a normalised waveform.
- A **yellow speed badge** (e.g. `2×`) appears on clips with a speed multiplier ≠ 1.0.
- Selected clips have a yellow highlight border.
