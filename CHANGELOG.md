# Changelog

All notable project changes and progress should be recorded here.

## Unreleased

### Added
- **Timeline filmstrip thumbnails now follow clip time**:
  - Video clip thumbnail strips now sample frames across the clip instead of repeating a single frame.
  - Each tile maps to its corresponding position between clip `source_in` and `source_out`, so trims/in-out changes are reflected in the strip.
  - Rendering keeps async `ThumbnailCache` extraction and uses a per-clip tile cap to avoid excessive request churn.
- **Preferences window + `Ctrl+,` shortcut**:
  - Added a categorized Preferences dialog (General, Playback) opened with **`Ctrl+,`**.
  - Added an initial **Hardware Acceleration** preference toggle in Preferences.
  - Preferences are persisted in `~/.config/ultimateslice/ui-state.json`.
  - Added MCP tools `get_preferences` and `set_hardware_acceleration` for automation.
  - Hardware acceleration now applies immediately to source preview playback sink selection (with fallback support); export behavior is unchanged.
- **Source preview close action**:
  - Added a close (`✕`) button in the Source Monitor header.
  - Clicking close now deselects the active media-library item, hides the source preview panel, stops source playback, and resets source marks/timecode state.
  - Added MCP tool `close_source_preview` for automating the same behavior.
- **Magnetic timeline mode (gap-free, track-local)**:
  - Added a **Magnetic** toggle in the toolbar to enable/disable gap-free timeline behavior.
  - When enabled, edits compact the affected track so clips remain contiguous after delete/move/trim and timeline insertions (append/drop/MCP clip edits).
  - Timeline overlay now shows a magnetic-mode indicator when active.
  - Added MCP tools `set_magnetic_mode` and `get_timeline_settings` for automation and verification.
- **Recent projects list**: Last 10 opened/saved projects are persisted in `~/.config/ultimateslice/recent.json`. A new **Recent ▾** button in the toolbar shows a popover with file names; clicking any entry opens that FCPXML immediately.
- **Per-clip speed change**:
  - New **Speed** section in the Inspector with a slider (0.25×–4.0×) and marks at ½×, 1×, 2×.
  - Changing speed updates `clip.speed` in the model immediately; the slider fires `on_project_changed` so the timeline clip width and program player both update.
  - `Clip::duration()` now returns timeline duration (`source_duration / speed`); `source_duration()` helper returns raw source material length.
  - GStreamer preview: `pipeline.seek(rate, ...)` with `rate = clip.speed` so the program monitor plays at the correct speed.
  - `poll()` converts GStreamer source position back to timeline position accounting for speed.
  - ffmpeg export: video filter gets `setpts=PTS/{speed}`, audio gets a chained `atempo` filter (handles full 0.25–4.0 range by splitting into ≤2.0 steps), input `-t` uses `source_duration` so the full source material is read.
  - Yellow speed badge (e.g. "2×") drawn on the clip in the timeline when speed ≠ 1.0.
  - FCPXML persistence via `us:speed` attribute.
- **Project Settings dialog** (`⚙ Settings` button in toolbar)
- **Advanced Export dialog** (replaces "Export MP4…")

### Fixed
- **Timeline scrubber position preservation**: `on_project_changed` now saves the current playhead position before rebuilding the program monitor clip list and restores it via a seek afterward, preventing the playhead from jumping to 0:00 on every project change (clip rename, color adjustment, etc.).
- **Inspector callbacks wired correctly**: `build_inspector` was previously called with an empty `|| {}` closure before `on_project_changed` was defined; it is now called after, and receives the real callback so clip name changes trigger proper UI updates.
- **Color sliders update preview live**: Color slider changes now call `prog_player.update_current_color()` directly (sets GStreamer `videobalance` properties + issues a flush seek to force frame redecode) rather than routing through the full `load_clips` pipeline reset, giving instant visual feedback without position loss.
- **Same-clip seek optimization in ProgramPlayer**: `load_clip_idx` now detects when the requested clip is already loaded and performs a lightweight seek instead of a full pipeline teardown, making scrubbing within a single clip fast and reliable.
- **`list_clips` MCP response now includes color fields**: `brightness`, `contrast`, `saturation` are included alongside other clip properties.

### Added
- **Titles / text overlay per clip**:
  - New **Title Overlay** section in the Inspector panel: text entry, X/Y position sliders (0.0–1.0 relative).
  - GStreamer `textoverlay` element injected at the end of the video filter bin; hidden (`silent=true`) when text is empty.
  - Default font `Sans Bold 36`, default color white, default position bottom-center (x=0.5, y=0.9).
  - Changes apply live to the program monitor without pipeline reload.
  - FCPXML persistence via `us:title-text`, `us:title-font`, `us:title-color`, `us:title-x`, `us:title-y` attributes.
- **Timeline markers / chapter points**:
  - Press `M` at any playhead position to drop a colored marker on the timeline ruler.
  - Markers are drawn as filled triangles with a vertical line and label; default color is orange (0xFF8C00FF).
  - Right-click anywhere on the ruler to remove the nearest marker within 8 px.
  - Markers persist across save/load via FCPXML (written as `<marker>` elements with custom `us:color` attribute).
  - Keyboard shortcut dialog updated to list `M` and right-click-ruler actions.
- **Snap-to-clip-edge when trimming**: `TrimIn` and `TrimOut` drag operations now snap to nearby clip edges (start/end of any other clip) within a 10 px threshold, matching the existing snap behavior for clip moves.
- **Volume and Pan per clip**:
  - Added `volume: f32` (0.0–2.0, default 1.0) and `pan: f32` (−1.0–1.0, default 0.0) fields to `Clip` model with `#[serde(default)]`.
  - Inspector: new **Audio** section with **Volume** and **Pan** sliders that update the program monitor live via `update_current_audio()` (sets `playbin` volume property and `audiopanorama` element).
  - GStreamer: `audiopanorama` element injected as `audio-filter` on `playbin`; per-clip pan applied in `load_clip_idx` alongside existing volume.
  - FCPXML persistence: `us:volume` and `us:pan` custom attributes written/read in writer/parser for lossless round-trip.
- **Auto-save**: 60-second timer saves the project to `/tmp/ultimateslice-autosave.fcpxml` when the project is dirty. Window title briefly shows "(Auto-saved)" for 3 seconds then restores the dirty indicator.
- **Denoise and Sharpness per clip**:
  - Added `denoise: f32` (0.0–1.0, default 0.0) and `sharpness: f32` (-1.0–1.0, default 0.0) fields to `Clip` model with `#[serde(default)]`.
  - GStreamer preview: upgraded video-filter from a single `videobalance` element to a bin `videobalance ! videoconvert ! gaussianblur`. Positive sigma = denoise/blur; negative sigma = sharpen. Combined sigma = `denoise * 4 − sharpness * 6`.
  - Inspector: two new sliders — **Denoise** (0.0–1.0) and **Sharpness** (−1.0–1.0) — in a new "Denoise / Sharpness" section below Color. Sliders update the preview live via `update_current_effects` without a pipeline reload.
  - Export (ffmpeg): `hqdn3d` filter added per-clip when `denoise > 0`; `unsharp` filter added when `sharpness ≠ 0`, chained after the existing `eq` color filter.
  - MCP `set_clip_color` tool extended with optional `denoise` and `sharpness` parameters; `list_clips` response includes both new fields.
  - Added `denoise: f32` (0.0–1.0, default 0.0) and `sharpness: f32` (-1.0–1.0, default 0.0) fields to `Clip` model with `#[serde(default)]`.
  - GStreamer preview: upgraded video-filter from a single `videobalance` element to a bin `videobalance ! videoconvert ! gaussianblur`. Positive sigma = denoise/blur; negative sigma = sharpen. Combined sigma = `denoise * 4 − sharpness * 6`.
  - Inspector: two new sliders — **Denoise** (0.0–1.0) and **Sharpness** (−1.0–1.0) — in a new "Denoise / Sharpness" section below Color. Sliders update the preview live via `update_current_effects` without a pipeline reload.
  - Export (ffmpeg): `hqdn3d` filter added per-clip when `denoise > 0`; `unsharp` filter added when `sharpness ≠ 0`, chained after the existing `eq` color filter.
  - MCP `set_clip_color` tool extended with optional `denoise` and `sharpness` parameters; `list_clips` response includes both new fields.
- Basic color correction per clip (brightness / contrast / saturation):
  - Added `brightness` (f32, default 0.0), `contrast` (f32, default 1.0), `saturation` (f32, default 1.0) fields to `Clip` model with `#[serde(default)]` so existing FCPXML/save files load without change.
  - Inspector panel: new "Color" section with three horizontal `Scale` sliders (brightness −1→1, contrast 0→2, saturation 0→2). Sliders update the clip live and trigger project-changed; feedback loop prevented by an `updating` flag during programmatic value set.
  - Playback: `Player::set_color()` applies a GStreamer `videobalance` element injected via `playbin`'s `video-filter` property. Program monitor (`ProgramPlayer`) applies per-clip color when loading each clip during timeline playback.
  - Export: ffmpeg `eq` filter inserted into the per-clip video filter chain (`scale/pad/setsar/fps/format,eq=…`) when color values differ from neutral; neutral clips skip the filter to avoid no-op overhead.
  - `SetClipColor` EditCommand added to `undo.rs` (reversible).
  - MCP tool `set_clip_color` added: accepts `clip_id`, `brightness`, `contrast`, `saturation`; updates clip in place and fires `on_project_changed`.

- Source Monitor — frame-accurate jog/shuttle control:
  - Frame step forward/backward buttons (◀▮ / ▮▶) in source monitor transport bar.
  - Left/Right arrow keyboard shortcuts for single-frame stepping.
  - J/K/L keyboard shortcuts for shuttle reverse/pause/forward at increasing speeds (1×, 2×, 4×).
  - Frame-accurate seeking via new `Player::seek_accurate()` (uses GStreamer `ACCURATE` flag).
  - `Player::step_forward()` / `step_backward()` methods for frame-level navigation.
  - Frame-accurate timecode display (`H:MM:SS:FF`) in position/duration label.
- Source Monitor — dedicated mark-in / mark-out timecode bar:
  - New styled `.marks-bar` showing In, Out, and Duration timecodes with frame accuracy.
  - In-point (green), Out-point (orange), and Duration labels with monospace font.
  - `SourceMarks.frame_ns` field for configurable frame duration (defaults to 24 fps).
- MCP `export_mp4` tool:
  - Added `McpCommand::ExportMp4` and MCP tool schema/dispatch (`export_mp4`).
  - Added main-thread handler in `window.rs` to run export in a background worker and return JSON results.
- Agent workflow rule:
  - Added instruction that new user-facing features should also be added to MCP (when automatable and not already exposed).

### Fixed
- MP4 export audio tracks:
  - Export previously produced silent video due to `-an` flag, `a=0` in concat filter, and audio tracks never being consulted.
  - Fixed: embedded audio from `ClipKind::Video` clips is extracted via `[i:a]adelay=DELAY:all=1` and mixed with `amix`.
  - Audio-only clips from dedicated audio tracks are also included and positioned at their timeline offsets.
  - `ClipKind::Image` clips and video clips without an audio stream (detected via `ffprobe`) are safely skipped.
  - Output is encoded as AAC 192 kbps stereo alongside the existing H.264 video stream.
  - Fixed missing `;` separator between the last `adelay` output label and the `amix` input list in the filter complex string (caused ffmpeg EINVAL / exit 234).
  - MCP export confirmed working end-to-end on `sample-project.fcpxml` (5 clips, ~60s AAC output).
- MP4 export ffmpeg discovery:
  - `Command::new("ffmpeg")` failed when the app's process PATH did not include `/usr/bin`.
  - Added `find_ffmpeg()` which tries the bare name first, then falls back to common absolute paths (`/usr/bin/ffmpeg`, `/usr/local/bin/ffmpeg`, `/opt/homebrew/bin/ffmpeg`).
  - Added `probe_has_audio()` using co-located `ffprobe` to check for audio streams before building the filter graph.
- MP4 export error visibility:
  - FFmpeg error output was silently discarded; exit failures only reported the exit code.
  - Non-progress stderr lines are now captured, logged via `eprintln!`, and included in the returned error message.
- MP4 export reliability:
  - Reworked export pipeline to use ffmpeg clip concat/transcode with per-clip in/out trimming.
  - Normalized sample aspect ratio (`setsar=1`) to prevent concat filter mismatch across mixed sources.
  - Confirmed MCP sample export success:
    - `Sample-Media/mcp-export-test.mp4` (~4.29s)
    - `Sample-Media/mcp-export-full.mp4` (~64.83s)
- Project load visibility:
  - Ensured timeline redraw/content-height update after project load.
  - Reset timeline view state (playhead/scroll/zoom/selection) on New/Open.
  - Synced project clip sources into media library and refreshed browser list when library changes externally.
- Media browser interaction:
  - Fixed click-to-select conflict introduced by drag source handling.
- Timeline scrubber interaction:
  - Fixed continuous click-and-drag scrubbing on the timeline ruler/playhead.
  - Scrubbing now works even when Razor tool is active.
  - Fixed timeline click/seek jumping back to 0 by syncing timeline playhead from program timeline position (not source monitor player position).

### Previous implemented milestones (recent)
- Program monitor playback panel and timeline-linked seeking.
- Audio waveform rendering in timeline audio tracks.
- Drag-and-drop from media browser to timeline.
- Keyboard shortcut overlay and export progress dialog.
- Comprehensive dark theme CSS coverage.
