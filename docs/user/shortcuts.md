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
| `Shift+P` | Toggle proxy playback on/off (switches the bottom status bar between `Original Media` and `Using Proxies`, and restores the last non-Off proxy size) |
| `Ctrl+J` | Go to timecode (jump playhead) |
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

The same **I / O** Source Monitor marks are also used by the keyword-range controls beneath the preview when you save or update Media Library keyword ranges.

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
| `D` | Toggle Draw tool |
| `S` | Toggle solo for selected track |
| `M` | Toggle mute for selected track |
| `Shift+L` | Toggle lock for selected track |
| `F` | Match Frame — load selected clip's source in Source Monitor at matching timecode |
| `Shift+F` | Create freeze-frame clip from selected video clip at playhead |
| `Ctrl+Shift+B` | Join selected through-edit boundary into one clip |
| `,` | Insert at playhead (shift subsequent clips) |
| `.` | Overwrite at playhead (replace existing material) |
| `Escape` | Switch to Select tool, or cancel an armed **Generate Music Region** draw |
| `Delete` / `Backspace` | Delete selected clip(s) |
| `Shift+Delete` / `Shift+Backspace` | Ripple delete selected clip(s) (track-local gap close) |
| `Ctrl+Shift+→` | Select clips forward from playhead |
| `Ctrl+Shift+←` | Select clips backward from playhead |
| `Ctrl+C` | Copy selected timeline clip |
| `Ctrl+V` | Paste copied clip as insert at playhead |
| `Ctrl+Shift+V` | Paste copied clip attributes onto selected clip |
| `Ctrl+Alt+C` | Copy color grade from selected clip |
| `Ctrl+Alt+V` | Paste color grade onto selected clip |
| `Ctrl+Alt+M` | Match selected clip's color to a reference clip |
| `Ctrl+G` | Group selected clips |
| `Ctrl+Shift+G` | Ungroup selected clips |
| `Alt+G` | Create compound clip from selected clips |
| `Alt+M` | Create multicam clip from selected clips |
| `1`–`9` (multicam) | Switch to angle 1–9 at playhead (when multicam clip selected) |
| `Double-click` (compound clip) | Drill into compound clip sub-timeline |
| `Escape` (in compound) | Exit compound clip drill-down (go back one level) |
| `Ctrl+L` | Link selected clips |
| `Ctrl+Shift+L` | Unlink selected clips |
| `Shift+Click` (timeline) | Add range selection (same-track span, or cross-track time-range select) |
| `Ctrl`/`Cmd` + Click (timeline) | Toggle a clip in the current selection |
| `Ctrl+A` | Select all timeline clips |
| Drag in empty timeline body | Marquee-select clips intersecting the rectangle |
| `M` | Add chapter marker at playhead |
| `Right-click clip` | Open clip context menu with only currently actionable clip actions (join-through-edit, freeze-frame, link/unlink, grouped timecode-align, audio sync when applicable) |
| `Right-click ruler` | Remove nearest marker |
| `Right-click transition marker` | Remove transition at clip boundary |
| `Scroll (vertical)` | Zoom timeline in/out |
| `Scroll (horizontal)` | Pan timeline left/right |
| Click (mini-map) | Centre viewport on clicked position |
| Drag (mini-map) | Pan viewport continuously |
| `Ctrl`/`Cmd` + Click (mini-map) | Seek playhead to clicked position |
| Double-click (mini-map) | Zoom timeline to fit entire project |
| `Shift+M` | Toggle timeline mini-map |
| `?` / `/` | Show keyboard shortcut reference |

Right-click an audio track header and choose **Generate Music Region…** to arm the one-shot MusicGen draw gesture; `Escape` cancels it before you drag.

---

## Inspector

All Inspector controls are mouse-driven sliders and text fields. Transform edits can also be nudged from the Program Monitor overlay:

| Shortcut | Action |
|---|---|
| `←` / `→` / `↑` / `↓` | Nudge selected clip position by 0.01 |
| `Shift + Arrow` | Nudge selected clip position by 0.1 |
| `+` | Increase selected clip scale |
| `-` | Decrease selected clip scale |
| `Ctrl`/`Cmd` + Click *(Keyframes panel focused)* | Toggle clicked keyframe in dopesheet selection |
| `Shift+Click` *(Keyframes panel focused)* | Select same-lane keyframe range from anchor to clicked keyframe |
| `Delete` / `Backspace` *(Keyframes panel focused)* | Remove selected dopesheet keyframe(s) |
| `←` / `→` *(Keyframes panel focused)* | Nudge selected dopesheet keyframe(s) by 1 frame |
| `Shift + ←` / `Shift + →` *(Keyframes panel focused)* | Nudge selected dopesheet keyframe(s) by 10 frames |
| `Ctrl + Scroll` *(Keyframes panel focused)* | Zoom dopesheet time scale in/out |
| `Scroll` *(Keyframes panel focused)* | Pan dopesheet time view |

---

## Transcript

The Transcript panel lives next to the keyframe dopesheet at the bottom of the timeline; toggle it via **Show Transcript** in the track-management bar. See [transcript.md](transcript.md) for details.

| Shortcut | Action |
|---|---|
| `Click` *(Transcript panel)* | Seek the playhead to the clicked word |
| `Shift+Click` *(Transcript panel)* | Extend the word selection within the same clip |
| `Delete` / `Backspace` *(Transcript panel focused)* | Split the underlying clip at the selection edges and ripple-delete the middle slice as one undo entry |

---

## Notes

- **J/K/L** shuttle works globally: J/K/L control the **Program Monitor** from anywhere in the window (no focus needed). In the Source Monitor, J/K/L still work as before when the Source Monitor panel has focus.
- **M** is captured globally so the timeline does not need to be focused.
- **Space** toggles playback in whichever monitor is contextually active.
- In timeline selection, **Ctrl/Cmd+Shift+Click** behaves like **Ctrl/Cmd+Click** (toggle clicked clip).
