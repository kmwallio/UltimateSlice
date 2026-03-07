# UltimateSlice — Keyboard Shortcuts Reference

> Press **?** or **/** anywhere in the timeline to open this reference as an in-app overlay.

---

## Global

| Shortcut | Action |
|---|---|
| `Ctrl+N` | New project |
| `Ctrl+O` | Open project XML (`.uspxml` / `.fcpxml`) |
| `Ctrl+S` | Save project XML (default `.uspxml`) |
| `Ctrl+,` | Open Preferences |
| `Ctrl+Z` | Undo |
| `Ctrl+Y` / `Ctrl+Shift+Z` | Redo |

---

## Source Monitor

| Shortcut | Action |
|---|---|
| `Space` | Play / Pause |
| `I` | Set In-point |
| `O` | Set Out-point |
| `J` | Shuttle reverse (1× → 2× → 4×) |
| `K` | Stop shuttle / Pause |
| `L` | Shuttle forward (1× → 2× → 4×) |
| `←` | Step one frame back |
| `→` | Step one frame forward |
| `,` | Insert at playhead (shift subsequent clips) |
| `.` | Overwrite at playhead (replace existing material) |

---

## Timeline

| Shortcut | Action |
|---|---|
| `Space` | Play / Pause program monitor |
| `J` | Shuttle reverse in program monitor (1× → 2× → 4× → 8×) |
| `K` | Stop shuttle / Pause program monitor |
| `L` | Shuttle forward in program monitor (1× → 2× → 4× → 8×) |
| `B` | Toggle Razor (Blade) tool |
| `R` | Toggle Ripple edit tool |
| `E` | Toggle Roll edit tool |
| `Y` | Toggle Slip edit tool |
| `U` | Toggle Slide edit tool |
| `,` | Insert at playhead (shift subsequent clips) |
| `.` | Overwrite at playhead (replace existing material) |
| `Escape` | Switch to Select tool |
| `Delete` / `Backspace` | Delete selected clip(s) |
| `Shift+Delete` / `Shift+Backspace` | Ripple delete selected clip(s) (track-local gap close) |
| `Ctrl+Shift+→` | Select clips forward from playhead |
| `Ctrl+Shift+←` | Select clips backward from playhead |
| `Ctrl+C` | Copy selected timeline clip |
| `Ctrl+V` | Paste copied clip as insert at playhead |
| `Ctrl+Shift+V` | Paste copied clip attributes onto selected clip |
| `Ctrl+G` | Group selected clips |
| `Ctrl+Shift+G` | Ungroup selected clips |
| `Shift+Click` (timeline) | Add range selection (same-track span, or cross-track time-range select) |
| `Ctrl`/`Cmd` + Click (timeline) | Toggle a clip in the current selection |
| `Ctrl+A` | Select all timeline clips |
| Drag in empty timeline body | Marquee-select clips intersecting the rectangle |
| `M` | Add chapter marker at playhead |
| `Right-click ruler` | Remove nearest marker |
| `Right-click transition marker` | Remove transition at clip boundary |
| `Scroll (vertical)` | Zoom timeline in/out |
| `Scroll (horizontal)` | Pan timeline left/right |
| `?` / `/` | Show keyboard shortcut reference |

---

## Inspector

All Inspector controls are mouse-driven sliders and text fields. Transform edits can also be nudged from the Program Monitor overlay:

| Shortcut | Action |
|---|---|
| `←` / `→` / `↑` / `↓` | Nudge selected clip position by 0.01 |
| `Shift + Arrow` | Nudge selected clip position by 0.1 |
| `+` | Increase selected clip scale |
| `-` | Decrease selected clip scale |

---

## Notes

- **J/K/L** shuttle works globally: J/K/L control the **Program Monitor** from anywhere in the window (no focus needed). In the Source Monitor, J/K/L still work as before when the Source Monitor panel has focus.
- **M** is captured globally so the timeline does not need to be focused.
- **Space** toggles playback in whichever monitor is contextually active.
- In timeline selection, **Ctrl/Cmd+Shift+Click** behaves like **Ctrl/Cmd+Click** (toggle clicked clip).
