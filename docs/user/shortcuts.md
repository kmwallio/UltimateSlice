# UltimateSlice — Keyboard Shortcuts Reference

<!-- GENERATED from src/keybindings.rs by `UPDATE_SHORTCUTS_DOC=1 cargo test keybindings` — edit the registry, not this file. -->

> Press **?** or **/** in the timeline to open this reference in-app. Every shortcut below (except the fixed keys and mouse gestures) can be changed in **Preferences → Shortcuts**.

---

## Global

| Shortcut | Action |
|---|---|
| `Ctrl+N` | New project |
| `Ctrl+O` | Open project XML (`.uspxml` / `.fcpxml`) |
| `Ctrl+S` | Save project XML (default `.uspxml`) |
| `Ctrl+,` | Open Preferences |
| `Ctrl+Shift+P` | Command palette |
| `J` | Shuttle the Program Monitor in reverse (1× → 2× → 4× → 8×) |
| `K` | Stop shuttle / pause the Program Monitor |
| `L` | Shuttle the Program Monitor forward (1× → 2× → 4× → 8×) |
| `Ctrl+J` | Go to timecode (jump playhead) |
| `,` | Insert the Source Monitor selection at the playhead (shift subsequent clips) |
| `.` | Overwrite at the playhead with the Source Monitor selection (replace existing material) |
| `M` | Add a chapter marker at the playhead |
| `Alt+←` | Jump to the previous keyframe of the selected clip |
| `Alt+→` | Jump to the next keyframe of the selected clip |
| `Shift+K` | Toggle Inspector animation mode (Record Keyframes) |
| `Shift+M` | Toggle timeline mini-map |
| `Shift+H` | Toggle the Program Monitor HUD overlay (timecode / frame / fps / resolution / dropped) |
| `Shift+P` | Toggle proxy playback on/off (switches the status bar between `Original Media` and `Using Proxies`, restoring the last non-Off proxy size) |
| `Ctrl+Shift+T` | Generate subtitles (speech-to-text) for the selected clip |

---

## Timeline

| Shortcut | Action |
|---|---|
| `Space` | Play / pause the Program Monitor |
| `Ctrl+Z` | Undo — shows a brief `Undid: …` toast |
| `Ctrl+Shift+Z` / `Ctrl+Y` | Redo — shows a brief `Redid: …` toast |
| `Ctrl+A` | Select all clips |
| `Ctrl+Shift+→` | Select clips forward from playhead |
| `Ctrl+Shift+←` | Select clips backward from playhead |
| `Ctrl+C` | Copy selected timeline clip |
| `Ctrl+V` | Paste copied clip as insert at playhead |
| `Ctrl+Shift+V` | Paste copied clip attributes onto selected clip |
| `Del` / `Backspace` | Delete selected clip(s) |
| `Shift+Del` / `Shift+Backspace` | Ripple delete selected clip(s) (track-local gap close) |
| `Ctrl+Shift+B` | Join selected through-edit boundary into one clip |
| `Enter` | Open Precision Trim for the active Ripple/Roll/Slip/Slide tool, or commit the current Precision Trim session |
| `F` | Match Frame — load the selected clip's source in the Source Monitor at the matching timecode |
| `Shift+F` | Create a freeze-frame clip from the selected video clip at the playhead |
| `Ctrl+Alt+C` | Copy color grade from selected clip |
| `Ctrl+Alt+V` | Paste color grade onto selected clip |
| `Ctrl+Alt+Shift+V` | Paste color grade onto every selected clip (single undo) |
| `Ctrl+Alt+M` | Match selected clip's color to a reference clip |
| `Ctrl+G` | Group clips |
| `Ctrl+Shift+G` | Ungroup clips |
| `Ctrl+L` | Link clips |
| `Ctrl+Shift+L` | Unlink clips |
| `Alt+G` | Create compound clip from selected clips |
| `Alt+M` | Create multicam clip from selected clips |
| `1` | Set selected keyframes to Linear interpolation |
| `2` | Set selected keyframes to Ease In |
| `3` | Set selected keyframes to Ease Out |
| `4` | Set selected keyframes to Ease In/Out |
| `S` | Toggle solo for the selected track |
| `Ctrl+M` | Toggle mute for the selected track |
| `Shift+L` | Toggle lock for the selected track |
| `B` | Toggle Razor (Blade) tool |
| `R` | Toggle Ripple edit tool |
| `E` | Toggle Roll edit tool (2-up precision trim display in the Program Monitor while dragging) |
| `Y` | Toggle Slip edit tool (4-up precision trim display while dragging) |
| `U` | Toggle Slide edit tool (4-up precision trim display while dragging) |
| `D` | Toggle Draw tool |
| `Esc` | Cancel Precision Trim or an armed Generate Music Region draw, exit a compound clip drill-down, or switch to the Select tool |
| `?` / `/` | Show this keyboard shortcut reference |

---

## Source Monitor

| Shortcut | Action |
|---|---|
| `Space` | Play / Pause |
| `I` | Set In-point |
| `O` | Set Out-point |
| `J` | Shuttle reverse (1× → 2× → 4×) |
| `K` | Stop shuttle / pause |
| `L` | Shuttle forward (1× → 2× → 4×) |
| `←` | Step one frame back |
| `→` | Step one frame forward |

---

## Timeline — fixed keys and mouse

| Shortcut | Action |
|---|---|
| `←  /  →  (Precision Trim active)` | Nudge the active Precision Trim session by 1 frame |
| `Shift+←  /  Shift+→  (Precision Trim active)` | Nudge the active Precision Trim session by 5 frames |
| `5 – 9  (multicam clip selected)` | Switch to angle 5–9 at the playhead |
| `Double-click (compound clip)` | Drill into compound clip sub-timeline |
| `Shift+Click` | Add range selection (same-track span, or cross-track time-range select) |
| `Ctrl/Cmd+Click` | Toggle a clip in the current selection |
| `Drag in empty timeline body` | Marquee-select clips intersecting the rectangle |
| `Right-click clip` | Clip context menu with the currently actionable clip actions (including **Precision Trim…**) |
| `Right-click ruler` | Remove nearest marker |
| `Right-click transition marker` | Remove transition at clip boundary |
| `Scroll (vertical)` | Zoom timeline in/out |
| `Scroll (horizontal)` | Pan timeline left/right |
| `Click (mini-map)` | Centre viewport on clicked position |
| `Drag (mini-map)` | Pan viewport continuously |
| `Ctrl/Cmd+Click (mini-map)` | Seek playhead to clicked position |
| `Double-click (mini-map)` | Zoom timeline to fit entire project |

---

## Inspector / Program Monitor overlay

All Inspector controls are mouse-driven sliders and text fields. Transform edits can also be nudged from the Program Monitor overlay:

| Shortcut | Action |
|---|---|
| `←  /  →  /  ↑  /  ↓` | Nudge selected clip position by 0.01 |
| `Shift+Arrow` | Nudge selected clip position by 0.1 |
| `+` | Increase selected clip scale |
| `-` | Decrease selected clip scale |
| `1 – 4  (Draw tool)` | Stroke / Rectangle / Ellipse / Arrow |
| `Delete / Backspace  (Draw tool)` | Delete the selected (or last) drawing item |
| `Ctrl/Cmd+Click  (Keyframes panel focused)` | Toggle clicked keyframe in dopesheet selection |
| `Shift+Click  (Keyframes panel focused)` | Select same-lane keyframe range from anchor to clicked keyframe |
| `Delete / Backspace  (Keyframes panel focused)` | Remove selected dopesheet keyframe(s) |
| `←  /  →  (Keyframes panel focused)` | Nudge selected dopesheet keyframe(s) by 1 frame |
| `Shift+←  /  Shift+→  (Keyframes panel focused)` | Nudge selected dopesheet keyframe(s) by 10 frames |
| `Ctrl+Scroll  (Keyframes panel focused)` | Zoom dopesheet time scale in/out |
| `Scroll  (Keyframes panel focused)` | Pan dopesheet time view |

---

## Transcript

The Transcript panel lives next to the keyframe dopesheet at the bottom of the timeline; toggle it via **Show Transcript** in the track-management bar. See [transcript.md](transcript.md) for details.

| Shortcut | Action |
|---|---|
| `Click  (Transcript panel)` | Seek the playhead to the clicked word |
| `Shift+Click  (Transcript panel)` | Extend the word selection within the same clip |
| `Delete / Backspace  (Transcript panel focused)` | Split the underlying clip at the selection edges and ripple-delete the middle slice as one undo entry |

---

## Notes

- **Customise**: Preferences → **Shortcuts** lists every rebindable action; click a shortcut and press the new key combination. Overrides are stored in `~/.config/ultimateslice/keybindings.json` (only actions that differ from the defaults), which can be exported, imported or deleted to reset.
- **Contexts**: *Global* shortcuts work anywhere in the window (and in the popped-out Program Monitor); *Timeline* shortcuts need the timeline to have focus; *Source Monitor* shortcuts need the Source Monitor panel to have focus. A Timeline or Source Monitor binding takes precedence over a Global one for the same key while that panel has focus.
- **Typing**: while a text field has focus, shortcuts without Ctrl or Alt are passed to the field.
- **J/K/L** control the **Program Monitor** from anywhere; in the Source Monitor, J/K/L shuttle the source when that panel has focus.
- **Space** toggles playback in whichever monitor has focus.
- Right-click an audio track header and choose **Generate Music Region…** to arm the one-shot MusicGen draw gesture; `Esc` cancels it before you drag.
- In timeline selection, **Ctrl/Cmd+Shift+Click** behaves like **Ctrl/Cmd+Click** (toggle clicked clip).
