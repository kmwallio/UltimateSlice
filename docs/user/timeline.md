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
- **Move** a clip by dragging its body (horizontally within a track, or vertically to another track of the same kind).
- **Trim** the In-point by dragging the left edge of a selected clip.
- **Trim** the Out-point by dragging the right edge of a selected clip.

Snapping: clip edges snap to nearby clip boundaries (±10 px threshold) while moving or trimming.

### Razor / Blade Tool (`B`)

- Click on a clip body to **split** it at the playhead position.
- Press **B** or **Escape** to toggle back to Select tool.

### Magnetic Mode (Toolbar Toggle)

- Use the **Magnetic** toggle in the main toolbar to enable/disable gap-free editing.
- When enabled, the edited track is compacted after clip edits so gaps are removed.
- In v1, magnetic behavior is **track-local** (it does not ripple other tracks).
- Magnetic mode affects timeline edits from UI and MCP clip-edit tools.

### Ripple Edit Tool (`R`)

- Activates ripple trimming: trim a clip's in-point or out-point and all subsequent clips on the same track shift to fill or accommodate the change.
- Press **R** to toggle Ripple mode on/off.

### Roll Edit Tool (`E`)

- Click near an edit point (boundary between two adjacent clips) to adjust the cut point.
- The left clip's out-point and the right clip's in-point move together — the overall timeline duration stays the same.
- Press **E** to toggle Roll mode on/off.

### Slip Edit Tool (`Y`)

- Drag a clip body to shift its **source window** (source in/out) without moving the clip on the timeline or changing its duration.
- Useful for adjusting which portion of the source footage appears in a fixed-length clip.
- Press **Y** to toggle Slip mode on/off.

### Slide Edit Tool (`U`)

- Drag a clip body to **move it on the timeline** while the neighboring clips adjust their edit points to compensate.
- The left neighbor's out-point extends/shrinks and the right neighbor's in-point shrinks/extends — overall timeline duration stays the same.
- Press **U** to toggle Slide mode on/off.

## Keyboard Shortcuts

| Shortcut | Action |
|---|---|
| `Space` | Play / Pause program monitor |
| `B` | Toggle Razor (Blade) tool |
| `R` | Toggle Ripple edit tool |
| `E` | Toggle Roll edit tool |
| `Y` | Toggle Slip edit tool |
| `U` | Toggle Slide edit tool |
| `Escape` | Switch to Select tool |
| `Delete` / `Backspace` | Delete selected clip |
| `M` | Add chapter marker at current playhead position |
| `Right-click ruler` | Remove the nearest marker |
| `Right-click transition marker` | Remove transition at clip boundary |
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
- **Remove Track** removes the currently active (highlighted) track, or the last track if none is selected. At least one track is always kept.
- **Reorder tracks** by dragging a track's label vertically; a blue indicator line shows the drop target. Release to confirm.
- **Active track** — click anywhere in a track row (including empty space) to highlight it. The active track shows a blue accent bar on its label. The active track is used as the target for the Append button and the Remove Track button.
- Audio tracks show a waveform visualisation (decoded in the background after import).
- Muting an audio track excludes it from both preview and export.

## Transitions

- Use the **Transitions** pane on the right (below Inspector) to browse available transitions.
- Use the pane's button to **hide/show** the transition list.
- Drag **Cross-dissolve** from the pane and drop it near a clip boundary in the timeline to apply a transition marker.
- While dragging, the two clips that will receive the transition are highlighted as a live preview.
- **Remove a transition** by right-clicking its boundary marker in the timeline.
- Exports apply cross-dissolves on the primary video track.
- Preview shows transition fade ramps at clip boundaries for cross-dissolve markers.
- Transitions are designed to be extensible: future transition types will appear in the same pane.

## Undo / Redo

All clip moves, trims, splits, deletions, track add/remove operations, and transition application are undoable.

- `Ctrl+Z` — Undo
- `Ctrl+Y` or `Ctrl+Shift+Z` — Redo

The undo history is per-session (not persisted in the FCPXML).

## Clip Appearance

- Video clips show a time-mapped thumbnail strip (extracted in the background): tiles progress across the clip's source range instead of repeating one frame.
- Audio clips show a normalised waveform.
- A **yellow speed badge** (e.g. `2×`) appears on clips with a speed multiplier ≠ 1.0.
- Selected clips have a yellow highlight border.
