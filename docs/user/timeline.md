# Timeline

The **Timeline** panel (bottom) is where you arrange, trim, and edit clips into your final sequence.

When the current timeline has no clips yet, the track canvas shows a clear in-place drop target so a new project still has an obvious next action. You can drag clips in from the **Media Library** or drop files straight from your file manager.

## Layout

- **Ruler** — shows time positions with adaptive major/mid/minor tick marks; higher zoom levels add more marks and intermediate labels, while lower zoom levels reduce clutter. Click to seek the playhead.
- **Mini-map** — a thin overview strip (hidden by default) between the ruler and the tracks that shows the entire project at a glance. Each clip is drawn as a tiny coloured rectangle matching its colour label. Faint horizontal lane separators distinguish individual tracks. A white viewport rectangle shows the currently visible portion. Toggle via `Shift+M`, the **Show Mini-Map** button on the track-management bar, or in Preferences → Timeline. Interactions:
  - **Click** — centre the viewport on the clicked time position.
  - **Drag** — pan the viewport continuously.
  - **Ctrl+Click** — seek the playhead to the clicked position.
  - **Double-click** — zoom the timeline to fit the entire project.
- **Track rows** — each track (Video or Audio) shows clips as coloured rectangles.
- **Playhead** — the red vertical line indicates the current playback position.
- **Track header** — shows a drag handle (⠿), optional per-track color swatch dot, track name, and a badge row **[D] [M] [S] [L]** (Duck, Mute, Solo, Lock). A compact per-track stereo level meter (L/R) appears on the right when enabled.
- **Status bar** — bottom-left includes a **Track Audio Levels** eye toggle to show/hide track-header meters. Proxy queue label/progress appear only while proxies are actively generating.

When the track stack is taller than the viewport, the ruler stays in a fixed header above the scrolled tracks so seeking, scrubbing, and marker clicks remain available while you scroll vertically. Only the track rows scroll: the ruler remains visible without adding empty spacer bars above or below the visible timeline content.

## Navigation

| Action | How |
|---|---|
| Seek | Click on the ruler or left-drag in the ruler |
| Jump to exact timecode | `Ctrl+J` then enter `HH:MM:SS:FF` |
| Zoom in/out | Scroll the mouse wheel vertically |
| Pan left/right | Scroll the mouse wheel horizontally |
| Pan ruler view | Middle/right-drag in the ruler |

## Adding Clips to the Timeline

There are several ways to get clips onto the timeline:

- **Drag from Media Browser** — drag a clip from the media library panel and drop it onto a track at the desired position.
- **Drag from File Manager** — drag files directly from Nautilus, Thunar, or any file manager onto the timeline. Files are automatically imported into the media library and placed as clips at the drop position. Multiple files are placed sequentially. Video files with audio follow the same **Source Monitor A/V auto-link** setting as other source placements: enabled creates linked A/V clip pairs when matching tracks exist, while disabled uses single-clip placement behavior.
- **Append to Timeline** — use the "Append to Timeline" button in the media browser to place the selected clip at the end of the first matching track.
- **Insert / Overwrite** — use Insert (`,`) or Overwrite (`.`) to place the source monitor selection at the playhead position.
- **Source Monitor drag** — drag from the source monitor preview directly onto a timeline track.

## Tools

The main toolbar collapses **Select**, **Razor**, **Ripple**, **Roll**,
**Slip**, and **Slide** into one active-tool dropdown. The button always shows
the currently selected one of those edit tools, and it updates immediately when
you switch tools with the mouse or the keyboard shortcuts below. That dropdown
also stays visibly pressed whenever one of those edit tools is active, while
**Draw** uses its own separate button state.

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
- On edge clips (only one neighbor), slide remains available but movement is clamped and only the available-side neighbor is adjusted.
- Press **U** to toggle Slide mode on/off.

### Draw Tool (`D`)

- Click **Draw** in the linked toolbar control to enter vector-drawing mode on the Program Monitor.
- Click the adjacent **tools** icon in that same control to open brush options such as shape, color, width, fill, presets, reveal animation, and SVG export.
- Drag in the Program Monitor to draw on the drawing clip under the playhead; if none exists yet, UltimateSlice creates one automatically.

### Insert at Playhead (`,`)

- Places the current source selection (In → Out from the source monitor) at the playhead position, preferring the active track when kinds match.
- All clips at or after the playhead are **shifted right** to make room — a ripple insert.
- For sources with both video and audio, **Source Monitor A/V auto-link** controls behavior: **enabled** inserts a linked A/V pair when matching video+audio tracks exist (with embedded audio on the video clip muted), while **disabled** uses single-clip placement on a compatible track kind.
- Also available via the **⤵ Insert** button in the source monitor transport bar.
- Requires a source to be loaded with valid in/out marks; if no compatible track exists, the operation is skipped.

### Overwrite at Playhead (`.`)

- Places the current source selection at the playhead position, **replacing** any existing material in the time range and preferring the active track when kinds match.
- Clips that fall within the overwrite range are trimmed, split, or removed as needed.
- For sources with both video and audio, **Source Monitor A/V auto-link** controls behavior: **enabled** overwrites with a linked A/V pair when matching video+audio tracks exist (with embedded audio on the video clip muted), while **disabled** uses single-clip placement on a compatible track kind.
- No subsequent clips are shifted — the timeline duration only changes if you overwrite past the end.
- Also available via the **⏺ Overwrite** button in the source monitor transport bar.
- Requires a source to be loaded with valid in/out marks; if no compatible track exists, the operation is skipped.

### Timeline Copy/Paste (`Ctrl+C`, `Ctrl+V`, `Ctrl+Shift+V`)

- **Copy (`Ctrl+C`)** stores the currently selected timeline clip in the timeline clipboard.
- **Paste insert (`Ctrl+V`)** inserts the copied clip at the current playhead and shifts clips at/after the playhead to the right on the target track.
- **Paste attributes (`Ctrl+Shift+V`)** applies copied clip attributes (color/effects/audio/transform/title settings) onto the currently selected clip.
- Copy/paste currently operates on a single selected clip.

### Freeze Frame (`Shift+F`)

- Select a **video** clip and position the playhead on that clip, then press **Shift+F**.
- UltimateSlice opens a hold-duration prompt and creates a new freeze-frame clip at the playhead on the same track.
- The new freeze clip is **video-only and silent** by default, and stores freeze metadata for save/load/export pipelines.
- If the playhead is inside the selected clip, the source clip is split and downstream material on **all tracks** is rippled right to make room.
- On non-selected tracks, clips that overlap the playhead are split at the playhead; only their right segment is shifted.
- Also available via right-click clip context menu (**Create Freeze Frame…**) and the timeline track toolbar button (**❄ Freeze Frame…**).

### Join Through Edit (`Ctrl+Shift+B`)

- Select one side of a join-safe through-edit cut (or select both clips), then press **Ctrl+Shift+B`.
- UltimateSlice merges the two adjacent segments back into a single clip when the boundary is through-edit-safe and clip metadata/effect settings are compatible.
- The merged clip keeps the left segment identity/timing and carries forward the right segment's outgoing transition metadata (if any).
- Join Through Edit is unavailable when metadata/effect settings have diverged between the two segments or when the selection resolves to multiple candidate boundaries.
- Also available from the right-click clip context menu as **Join Through Edit**.

### Remove Silent Parts

Cut out long silences from a clip using ffmpeg `silencedetect`.

1. Right-click an audio-bearing clip on the timeline and choose **Remove Silent Parts…**
2. Set the **Silence threshold (dBFS)** — audio below this level counts as silence. Default −50 dBFS works well for clean speech; try −40 for noisier rooms.
3. Set the **Minimum silence duration (seconds)** — short pauses below this length are kept. Default 0.5 s.
4. Click **Suggest** to auto-pick the threshold from the clip's measured noise floor (uses ffmpeg `astats`, 5th-percentile RMS + 6 dB headroom). The suggestion appears in the spinner with a status label below.
5. Click **Remove** — ffmpeg analyzes the clip in a background thread and the silent segments are removed (the clip is split into back-to-back sub-clips around the speech regions, preserving the speech timing).
6. **Undo** with `Ctrl+Z` to restore the original clip.

The Suggest button shares its analysis logic with the Inspector's voice-isolation Suggest button. If the suggested threshold seems too aggressive (cutting real audio) or too lax (leaving silences), nudge the spinner manually before clicking Remove.

### Detect Scene Cuts

Automatically find scene/shot boundaries in a clip using ffmpeg video analysis.

1. Right-click a video or audio clip on the timeline and choose **Detect Scene Cuts…**
2. Set the **threshold** (1–50, default 10). Lower values detect more cuts, including subtle transitions. Higher values only detect obvious hard cuts.
3. Click **Detect** — ffmpeg analyzes the video in a background thread. The bottom status bar shows ongoing progress, and an in-app toast confirms the result when detection finishes.
4. The clip is split at each detected cut point into back-to-back sub-clips, preserving the total duration.
5. **Undo** with `Ctrl+Z` to restore the original clip.

Also available via MCP `detect_scene_cuts` tool for automation.

### Convert LTC Audio to Timecode

Decode **Linear Timecode (LTC)** from a clip's audio and turn it into normal clip/source timecode metadata.

1. Right-click a single source-backed audio or video clip and choose **Convert LTC Audio to Timecode…**
2. Pick the LTC source channel:
   - **Auto Detect** tries left, right, then mono mix.
   - **Left Channel** / **Right Channel** are best when one stereo side is known to carry LTC.
   - **Mono Mix** is useful for mono LTC or already mixed-down recordings.
3. Optionally override the LTC frame rate. **Project / Source Default** uses the source frame rate when known, otherwise the project frame rate.
4. Click **Convert**.

What happens next:

- The decoded base timecode is stored on the media source and on every matching clip already in the project.
- If LTC is on one stereo side, UltimateSlice routes the **other** side to both speakers using the existing clip channel-routing controls.
- If the recording is effectively **mono LTC only**, the clip is muted after conversion.
- You can then group clips and use **Align Grouped Clips by Timecode** normally.

Also available via MCP `convert_ltc_audio_to_timecode` for automation.

### Generate Music on an Audio Track

Use MusicGen to fill an empty span on an audio track with generated background music.

1. Right-click an **audio track header** and choose **Generate Music Region…**.
2. Drag across **empty space on that same audio track** to define the target region.
3. Enter a prompt in the MusicGen dialog and click **Generate**.
4. UltimateSlice shows the requested region inline while generation is pending, then replaces it with the generated WAV clip when the job completes.

- Regions must be **between 1 and 30 seconds**.
- Regions must stay in **empty audio-track space**; drawing across an existing clip is rejected.
- Press **Escape** to cancel an armed region draw before you start dragging.
- The existing **Generate Music…** track action still works when you want a playhead-based generation instead of drawing a region first.
- **Reference audio (optional):** click **Choose Reference Audio…** in the dialog to point at any audio or video file. UltimateSlice analyzes its tempo (BPM), musical key/mode, brightness, and dynamics, then prefills a **Style hints** field with a natural-language descriptor (e.g. *"around 128 BPM, in the key of C major, bright timbre, dynamic energy"*). The hints are appended to your prompt before generation; you can edit or clear them in place. Analysis runs in the background and only the first 30 seconds of the reference are inspected. The MusicGen model itself is unchanged — this is text-prompt augmentation on top of the existing musicgen-small model. The MCP `generate_music` tool accepts an equivalent `reference_audio_path` argument.
- Music generation requires the MusicGen ONNX models under `~/.local/share/ultimateslice/models/musicgen-small/` and a build with the `ai-inference` feature enabled.

## Image Clips

Still images (PNG, JPEG, GIF, BMP, TIFF, WebP, HEIC, SVG) can be placed on the timeline like video clips.

- **Default duration**: Static images are imported with a **4-second** default duration.
- **Animated SVG duration**: Animated SVG clips use their authored animation duration by default.
- **Placement**: Images are always placed on a **Video track** as `ClipKind::Image`. No linked audio clip is created.
- **Extending duration**: Drag the right edge (trim-out handle) of an image clip to extend it to any length — there is no upper limit. Animated SVG clips hold on their last rendered frame when extended past the authored animation duration.
- **Shortening duration**: Drag the right edge inward to shorten the clip.
- **Color/effects**: All color correction, grading, LUT, transform, title, and chroma key controls work on image clips, just as they do on video clips.
- **Export**: Static image clips are exported with the FFmpeg `tpad` hold filter, consistent with freeze-frame video clips. Animated SVG clips are exported from their cached rendered video.

### Multi-Select (staged rollout)

- **Shift+Click** adds a range from the anchor to the clicked clip:
  - on the same track: selects the same-track span between the two clips;
  - across different tracks: selects clips that intersect the anchor↔click time range across all tracks.
- **Ctrl/Cmd+Click** toggles individual clips in the current selection.
- When both **Ctrl/Cmd+Shift** are held, toggle selection takes precedence over Shift range selection.
- **Ctrl+A** selects all clips in the timeline.
- **Marquee drag** (drag in empty timeline body) selects clips intersecting the rectangle.
- Selecting a linked clip also selects its linked peers so synchronized A/V-linked edits stay together.
- Modifier-based selection is preserved when a clip drag starts, so Ctrl/Cmd+click and Shift+click selections do not unexpectedly collapse.
- Dragging a selected clip moves the current selected set together while preserving relative offsets across tracks; grouped clips are still expanded and move as a unit.
- The Inspector still follows the primary selected clip.

### Ripple Delete (`Shift+Delete`)

- Removes selected clip(s) and closes gaps on the affected track(s) only.
- Works with single selection and multi-selection.
- Uses track-local compaction (does not shift unrelated tracks).

### Select Forward / Backward from Playhead (`Ctrl+Shift+→`, `Ctrl+Shift+←`)

- **Select Forward** selects all clips with timeline content after the playhead.
- **Select Backward** selects all clips with timeline content before the playhead.
- Useful for bulk delete, ripple delete, grouping, and other multi-clip edits.

### Clip Grouping (`Ctrl+G`, `Ctrl+Shift+G`)

- **Group (`Ctrl+G`)** links the current multi-selection into one clip group.
- **Ungroup (`Ctrl+Shift+G`)** removes grouping for any selected grouped clips.
- Grouped clips move together when dragging any member.
- Grouped clips delete together for both normal delete and ripple delete.
- Selecting one clip in a group shows a secondary border on the other clips in that group for quick visual context.
- Right-clicking a grouped clip can now run **Align Grouped Clips by Timecode** when that clip group carries stored source-time metadata; the selected clip acts as the anchor when possible.
- Source timecode metadata is automatically extracted from media files on import (camera creation timestamps) and also preserved for FCPXML-imported clips and UltimateSlice-saved projects.
- First pass scope: grouped trim behavior is not yet enabled.

### Compound Clips (Nested Timelines)

- **Create Compound Clip (`Alt+G`)** — select 2+ clips, then press `Alt+G` or right-click → "Create Compound Clip". The selected clips are replaced by a single compound clip containing them as an internal sub-timeline.
- **Break Apart Compound Clip** — right-click a compound clip → "Break Apart Compound Clip" to restore the internal clips to the timeline.
- **Drill-down editing** — double-click a compound clip to enter its sub-timeline. A teal breadcrumb bar shows your navigation path (e.g. "Project > Compound Clip 1"). Press `Escape` to go back one level.
- Compound clips render with a teal fill color and a stacked-layers badge.
- Compound clips now get a **simple first-pass thumbnail strip** in the timeline by sampling the top visible source-backed child clip inside the compound window.
- This thumbnail strip is intentionally lightweight: it does **not** yet composite titles, adjustment layers, or other overlay-only children into the thumbnail, and compound waveforms are still a separate follow-up.
- Preview and export correctly flatten compound clips, rendering all internal clips at the right timeline positions.
- Compound clips are saved/loaded via FCPXML (`us:clip-kind="compound"` + `us:compound-tracks` vendor attribute).
- MCP tools: `create_compound_clip` (takes `clip_ids` array), `break_apart_compound_clip` (takes `clip_id`).
- Full undo/redo support.

### Multicam Editing

- **Create Multicam Clip (`Alt+M`)** — select 2+ video clips on the timeline, then press `Alt+M` or right-click → "Create Multicam Clip". Alternatively, select 2+ items in the **Media Library** (Ctrl+click) and click the "Create Multicam Clip" button — the clips are placed on the timeline and converted in one step. The clips are synced by audio cross-correlation and combined into a single multicam clip with per-angle source data.
- Multicam clips render with an **orange fill** on the timeline, showing segment labels and switch markers at each angle change.
- **Switch angles (1-9)** — with a multicam clip selected, press `1`–`9` to switch to the corresponding angle at the current playhead position. A switch point is inserted (or updated) at the playhead.
- **Angle viewer panel** — when a multicam clip is selected, the sidebar shows an angle viewer with buttons for each angle. The active angle at the playhead is highlighted. Click a button to switch angles.
- **Audio mixing** — each angle has independent volume and mute controls. By default, only the first angle's audio plays; unmute additional angles to mix them together. Audio from all unmuted angles plays continuously regardless of which video angle is active. Control via MCP `set_multicam_angle_audio` or the angle viewer panel (audio status shown per angle).
- Preview and export flatten multicam segments to the active angle's source media at each point in time, with audio mixed from all unmuted angles.
- Multicam clips are saved/loaded via FCPXML (`us:clip-kind="multicam"` + `us:multicam-angles` and `us:multicam-switches` vendor attributes).
- MCP tools: `create_multicam_clip`, `add_angle_switch`, `list_multicam_angles`, `set_multicam_angle_audio`.
- Full undo/redo support.

### Auditions (Alternate Takes)

- **Create Audition from Selection** — multi-select 2+ clips on the same track, then right-click → "Create Audition from Selection". The selection collapses to one gold "AUD" clip; the first selected clip becomes the active take and the others are stored as alternates.
- Audition clips render with a **gold fill** and an "AUD" badge plus an "n / m" indicator showing which take is active.
- **Switch active take** in the **Inspector → Audition** panel by clicking any take row. The Program Monitor and timeline update immediately. Color grade, transforms, transitions, masks, and effects attached to the slot stay in place across take swaps.
- **Add Take from Source** / **Remove Take** / **Finalize Audition** — Inspector buttons handle the rest of the lifecycle. Right-click also offers **Finalize Audition** to collapse an audition to a normal clip referencing only the active take.
- Auditions are saved/loaded via `.uspxml` (`us:clip-kind="audition"` + `us:audition-takes` JSON). Strict-mode `.fcpxml` export collapses to a plain asset-clip referencing the active take. OTIO round-trips through `metadata.ultimateslice.audition_*`.
- MCP tools: `create_audition_clip`, `add_audition_take`, `remove_audition_take`, `set_active_audition_take`, `list_audition_takes`, `finalize_audition`.
- Full undo/redo support.
- See [auditions.md](auditions.md) for the complete guide.

### Sync Selected Clips by Audio (right-click menu)

- Select 2 or more clips on the timeline, then right-click → **Sync Selected Clips by Audio**.
- The first selected clip is the **anchor** — it stays in place. All other clips are repositioned based on matching audio content using FFT cross-correlation.
- Sync runs on a background thread; the bottom status bar shows ongoing progress and an in-app toast confirms completion or failure.
- If no reliable audio match is found (low confidence), a status message is shown and no changes are applied.
- The operation is undoable (`Ctrl+Z`).
- Clips without audio streams are not eligible for audio sync (the button is insensitive when fewer than 2 clips are selected).
- Typical use case: multi-cam footage from cameras that were not jam-synced — each camera's audio captures the same ambient sound, enabling automatic alignment.

### Clip Linking (`Ctrl+L`, `Ctrl+Shift+L`)

- Dragging or MCP-placing a source that contains both video and audio follows **Source Monitor A/V auto-link**. When enabled, UltimateSlice auto-creates a linked A/V pair if matching video and audio tracks exist. When disabled, those operations use single-clip placement behavior.
- Auto-linked pairs share the same clip link group immediately, so the picture and sound stay selected/moved/deleted together without requiring a manual `Ctrl+L`.
- While a linked same-source audio-track peer exists, UltimateSlice suppresses the duplicate embedded audio from the linked video-track clip to avoid doubled playback/export audio. Unlinking restores the video clip's own embedded audio automatically.
- **Link (`Ctrl+L`)** assigns the current multi-selection to a shared clip link group.
- **Unlink (`Ctrl+Shift+L`)** clears linking for the selected linked clip(s) and any linked peers in the same link group.
- Linked clips are selected together, move together when dragging any linked member, and delete together for both normal delete and ripple delete.
- Link behavior is intentionally narrower than clip grouping: trims remain independent in this first pass.
- Right-clicking a selected clip opens a context menu that only shows currently actionable clip operations (for example Link/Unlink when applicable), so link editing is available without extra disabled entries.
- Linked clips show a **LINK** badge in the timeline so linked relationships stay visible even when nothing is selected.
- Clips whose source files are unavailable show an **OFFLINE** badge in the timeline clip header.
- When a linked selection spans multiple clips, non-primary linked peers also get a cyan inset border so they stay visually distinct from the primary selected clip.

## Keyboard Shortcuts

| Shortcut | Action |
|---|---|
| `Space` | Play / Pause program monitor |
| `B` | Toggle Razor (Blade) tool |
| `R` | Toggle Ripple edit tool |
| `E` | Toggle Roll edit tool |
| `Y` | Toggle Slip edit tool |
| `U` | Toggle Slide edit tool |
| `S` | Toggle solo for selected track |
| `M` | Toggle mute for selected track |
| `Shift+L` | Toggle lock for selected track |
| `F` | Match Frame — load selected clip's source in Source Monitor and seek to matching frame |
| `Shift+F` | Create freeze-frame clip from selected video clip at playhead |
| `Ctrl+Shift+B` | Join selected through-edit boundary into one clip |
| `,` | Insert at playhead (from source monitor) |
| `.` | Overwrite at playhead (from source monitor) |
| `Escape` | Switch to Select tool |
| `Delete` / `Backspace` | Delete selected clip(s) |
| `Shift+Delete` / `Shift+Backspace` | Ripple delete selected clip(s) (track-local gap close) |
| `Ctrl+Shift+→` | Select clips forward from playhead |
| `Ctrl+Shift+←` | Select clips backward from playhead |
| `Ctrl+J` | Go to timecode (jump playhead) |
| `Ctrl+C` | Copy selected timeline clip |
| `Ctrl+V` | Paste copied clip as insert at playhead |
| `Ctrl+Shift+V` | Paste copied clip attributes onto selected clip |
| `Ctrl+G` | Group selected clips |
| `Ctrl+Shift+G` | Ungroup selected clips |
| `Ctrl+L` | Link selected clips |
| `Ctrl+Shift+L` | Unlink selected clips |
| `Right-click clip` | Open clip context menu with only currently actionable clip actions (join-through-edit, freeze-frame, link/unlink, grouped timecode-align, audio sync, detect scene cuts when applicable) |
| `Shift+Click` (timeline) | Add range selection (same-track span, or cross-track time-range select) |
| `Ctrl`/`Cmd` + Click (timeline) | Toggle clip in current selection |
| `Ctrl+A` | Select all timeline clips |
| `Drag in empty timeline body` | Marquee-select clips intersecting the rectangle |
| `M` | Add chapter marker at current playhead position |
| `Right-click ruler` | Remove the nearest marker |
| `Right-click transition marker` | Remove transition at clip boundary |
| `Ctrl+Z` | Undo |
| `Ctrl+Y` / `Ctrl+Shift+Z` | Redo |
| `Scroll (vertical)` | Zoom timeline |
| `Scroll (horizontal)` | Pan timeline |
| `?` / `/` | Show in-app keyboard shortcut reference |
| `Alt+Left` | Jump playhead to previous keyframe of selected clip |
| `Alt+Right` | Jump playhead to next keyframe of selected clip |
| `Shift+K` | Toggle animation mode (Record Keyframes) |

## Chapter Markers

- Press **M** to drop a marker at the playhead — a label dialog allows you to name it.
- Markers appear as coloured flags on the ruler with their label.
- Right-click on the ruler to remove the nearest marker.
- Markers are exported in the FCPXML file.
- On FCPXML import, standard markers and chapter markers are read and placed at their correct timeline positions.
- When exporting to MP4, MOV, or MKV, markers are embedded as **chapter metadata** in the output file (see [export.md](export.md#chapter-markers)).
- Adding and removing markers is **undoable** (Ctrl+Z / Ctrl+Shift+Z).

### Markers panel

Use the **Show Markers** button on the track-management bar to open the Markers panel (shares the bottom slot with Keyframes and Transcript).

The panel lists every project marker with:

| Column | Description |
|--------|-------------|
| **Color** | Click the swatch to choose from 8 preset colours |
| **Name** | Click to rename via inline popover |
| **Time** | Timecode at which the marker sits |
| **Notes** | Click to add or edit free-text notes |
| **🗑** | Delete the marker (undoable) |

- **Double-click** any row to seek the playhead to that marker's position.
- Click **+ Add** in the panel header to add a marker at the current playhead.
- Panel visibility is saved/restored per workspace layout.

## Tracks

- **Add Track** buttons below the timeline add a new Video or Audio track.
- **Remove Track** removes the currently active (highlighted) track, or the last track if none is selected. At least one track is always kept.
- **Reorder tracks** by dragging a track's label vertically; a blue indicator line shows the drop target. Release to confirm.
- **Active track** — click anywhere in a track row (including empty space) to highlight it. The active track shows a coloured accent bar on its label (uses the track colour label when set, otherwise blue). The active track is used as the target for the Append button and the Remove Track button.
- **Track height presets** — right-click a track header and choose **Track Height: Small / Medium / Large** to resize that track row independently.
- **Direct track resizing** — drag the bottom edge of a track header when the **row-resize** cursor appears. The row snaps between the same **Small / Medium / Large** presets used by the context menu, so you can resize quickly without opening a menu.
- Audio tracks show a waveform visualisation (decoded in the background after import).
- Waveform drawing now reuses a shared multi-resolution cache after extraction, so zoomed-out timelines stay responsive even when many clips are visible at once.
- New waveform data now repaints automatically when background extraction finishes, so you do not need to start playback just to make freshly loaded waveforms appear.
- **Mute track** — click the **M** badge on a track header (or press **M** with that track active) to mute it. Muted tracks are silenced in playback and export; a dimming overlay appears on the track content area.
- **Lock track** — click the **L** badge on a track header (or press **Shift+L** with that track active) to lock it. Locked tracks cannot be edited; a hatch overlay appears on the track content area.
- **Solo track** — click the **S** badge on a track header (or press **S** with that track active) to solo it. When one or more tracks are soloed, only soloed non-muted tracks are active for Program Monitor playback and export.
- **Duck track** — click the **D** badge on an audio track header to toggle automatic ducking.
- **Track colour** — right-click a track header and use the **Track Color** swatch row to assign a colour label. A 10-pixel colour dot appears to the left of the track name, and the active-track accent bar adopts the colour.

## Automatic Audio Crossfades

- Automatic audio crossfades are controlled in **Preferences → Timeline**.
- When enabled, adjacent same-track edits with audio fade across the cut in Program Monitor playback and export.
- The selected curve (`Equal power` or `Linear`) and duration are shared by preview/export.
- Fade duration is automatically clamped for short adjacent clips.

## Transitions

- Use the **Transitions** pane on the right (below Inspector) to browse available transitions.
- The pane exposes the preview-supported transition set: `Cross-dissolve`, `Fade to black`, `Fade to white`, `Wipe left/right/up/down`, `Circle open/close`, `Cover left/right/up/down`, `Reveal left/right/up/down`, and `Slide left/right/up/down`.
- The transition list scrolls, and the divider above it can be dragged to resize how much of the right sidebar the pane uses.
- Use the pane's button to **hide/show** the transition list.
- Saved **Workspace** layouts remember the transitions-pane split height.
- Drag a transition from the pane and drop it near a clip boundary in the timeline to apply a transition marker.
- While dragging, the two clips that will receive the transition are highlighted as a live preview.
- New drops start with a default **500 ms** duration and can be refined from the clip **Inspector → Transition** section.
- The Inspector's **Alignment** control changes where the overlap sits relative to the cut: **End on cut** keeps the overlap before the cut, **Center on cut** straddles it, and **Start on cut** pushes the overlap after it.
- **Remove a transition** by right-clicking its boundary marker in the timeline or by clicking **Remove Transition** in the Inspector.
- Preview, export, and background prerender all follow the same transition duration/alignment timing.
- Export and background prerender use the exact selected supported transition, matching the live Program Monitor preview.
- For after-cut overlap, UltimateSlice keeps the outgoing clip visible by holding its final frame through the post-cut portion of the transition.
- Transitions are designed to be extensible: future transition types will appear in the same pane.

## Keyframes panel (dopesheet)

- Use the **Show/Hide Keyframes** button on the right side of the track-management bar to toggle the panel (hidden by default).
- The dopesheet appears as a resizable panel between the timeline tracks and the track-management bar. Drag the split handle to resize the dopesheet area.
- The dopesheet shows per-property lanes with keyframe points and value-curve overlays; drag points left/right to retime.
- When a keyframe is selected and has a following keyframe in the same lane, the segment shows Bezier handles. Drag either handle to shape the segment curve live; the exact handle shape is preserved in preview/runtime, while the interpolation dropdown reflects the nearest preset mode.
- **◀ Prev / Next ▶** buttons jump the playhead to the previous or next keyframe across all properties (same as **Alt+Left/Right**).
- **Ctrl/Cmd+Click** toggles a keyframe in the current dopesheet selection.
- **Shift+Click** on a keyframe selects a same-lane range between the anchor and the clicked keyframe.
- **Add @ Playhead** adds a keyframe at the playhead on the selected lane.
- **Remove** deletes selected keyframe(s).
- **Apply Interp** applies the selected interpolation mode (Linear / Ease In / Ease Out / Ease In/Out) to selected keyframe(s).
- Lane visibility toggles let you focus on specific animated properties while editing.
- Keyboard edits (panel focus): **Delete/Backspace** removes selected keyframe(s), **Left/Right** nudges by 1 frame, and **Shift+Left/Right** nudges by 10 frames.
- Time-scale controls: use **− / + / 100%** in the panel header row or **Ctrl+Scroll** over the dopesheet to zoom in/out.
- Scroll over the dopesheet to pan along time when zoomed in.

## Undo / Redo

All clip moves, trims, splits, deletions, track add/remove operations, transition application, and keyframe panel edits are undoable.

- `Ctrl+Z` — Undo
- `Ctrl+Y` or `Ctrl+Shift+Z` — Redo

When an undo or redo succeeds, UltimateSlice shows a short toast such as **"Undid: trim clip out-point"** or **"Redid: move clip"**. If the history is empty, the shortcut remains quiet.

The undo history is per-session (not persisted in the FCPXML).

## Clip Appearance

- Video clips show a time-mapped thumbnail strip (extracted in the background): tiles progress across the clip's source range instead of repeating one frame.
- Thumbnail strips now load progressively with adaptive tile density to keep timeline warm-up responsive on heavy media.
- Preferences → Timeline → **Show timeline preview** lets you switch to start/end-only thumbnails per video clip.
- Audio clips show a normalised waveform.
- When **Show audio waveforms on video clips** is enabled, the video-overlay view reuses the same cached waveform path instead of a separate slower draw pass.
- Those video-clip waveform overlays also repaint automatically as soon as waveform extraction completes.
- A **yellow speed badge** (e.g. `2×`) appears on clips with a speed multiplier ≠ 1.0. Clips with speed keyframes (variable speed ramps) show a **⏲ Ramp** badge instead.
- Clips with phase-1 keyframes show color-coded keyframe ticks/guides on the clip body (Scale, Opacity, Position X, Position Y, Volume, Pan, Rotate, Crop Left/Right/Top/Bottom), a `KF <count>` badge, and a `◆` prefix in the clip label when keyframes are present. **Click a keyframe tick** to select the clip and jump the playhead to that keyframe time.
- Hovering a keyframe marker shows a tooltip with the clip name, keyframe time, and which properties are modified at that keyframe moment.
- Clips can use semantic color labels (set in the Inspector) for quick visual categorization.
- Selected clips have a yellow highlight border.
- Adjacent join-safe through-edits (same source with contiguous source/timeline ranges, no boundary transition, and compatible clip metadata/effects) are marked with a subtle dotted line at the cut on the track row.
- Group peers (same `Ctrl+G` group) show a lighter secondary border when a group member is selected.
- Linked clips show a `LINK` badge whenever they belong to a clip link group.
- Non-primary linked peers in the current linked selection show a cyan inset border.
- **Adjustment layers** appear as purple hatched clips with an "ADJ" badge. They have no source media — their effects (color grading, LUTs, frei0r, blur) apply to the composited result of all tracks below. Add one by right-clicking a video track label and choosing "Add Adjustment Layer". Use the Inspector to set effects. Opacity controls effect intensity (0.0 = no effect, 1.0 = full effect).
  - **Preview**: Brightness, contrast, and saturation are shown in real-time. Frei0r user effects (e.g. vignette), LUTs, temperature/tint, and blur require **Background Render** to be visible in the Program Monitor — enable it in the status bar (the toggle reads **Live Rendering** while prerendering is off). Without Background Render, these effects are applied on **export only**.
  - **Export**: All adjustment layer effects (including frei0r plugins) are fully applied in the exported file.
- Title text positioning in the preview may differ from the export when large text extends beyond frame edges at reduced preview quality (Half/Quarter). This is because text clipping at lower resolutions produces different visible bounds than at full resolution. The export is always correct. Setting preview quality to **Full** eliminates this difference.
