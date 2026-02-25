# UltimateSlice Roadmap

A Final Cut Pro–inspired non-linear video editor built with GTK4 and Rust.

---

Tracking docs:
- [`CHANGELOG.md`](CHANGELOG.md) — running history of implemented changes/progress
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — agent/contributor implementation guide

## ✅ Implemented

### Foundation
- [x] GTK4 + Rust project scaffold (`gtk4-rs 0.11`, `gstreamer-rs 0.25`, `glib 0.22`)
- [x] Dark theme via custom CSS (`src/style.css`)
- [x] GApplication entry point with CSS loading

### Data Model
- [x] `Clip` — source path, source in/out (ns), timeline position, label, kind
- [x] `Track` — ordered list of clips, muted/locked flags, `TrackKind` (Video/Audio)
- [x] `Project` — title, frame rate, resolution, tracks, dirty flag
- [x] `MediaItem` — library entry (path, duration, label); separate from timeline clips
- [x] `SourceMarks` — shared in/out selection state for the source monitor

### Media Library Browser
- [x] Import media via file chooser (video/audio/image MIME filter)
- [x] GStreamer Discoverer probes duration on import
- [x] Library list with clip name + filename display
- [x] Selecting a library item loads it in the source preview
- [x] Imported clips are **not** auto-added to the timeline

### Source Preview / Monitor
- [x] GStreamer `playbin` + `gtk4paintablesink` video display
- [x] Source scrubber `DrawingArea` with click-to-seek
- [x] In-point (green) / Out-point (orange) markers on scrubber
- [x] Selected region highlighted in scrubber
- [x] **Set In (I)** / **Set Out (O)** keyboard shortcuts and buttons
- [x] In/Out timecode labels
- [x] Play/Pause (Space), Stop transport buttons
- [x] Timecode label (`position / duration`)

### Timeline
- [x] Cairo-rendered `DrawingArea` with ruler (adaptive tick intervals)
- [x] Multi-track rows (currently 1 Video + 1 Audio track created on project init)
- [x] Clip rendering with rounded rectangles, labels, selected highlight
- [x] Trim handles (in-edge / out-edge) shown when clip is selected
- [x] Playhead (red line + triangle) updated at 100 ms intervals from player position
- [x] **Select tool** — click to select/deselect clips
- [x] **Razor/Blade tool** — B to toggle; click splits clip at playhead
- [x] **Clip move** — drag clip body to reposition on timeline
- [x] **Trim in-point** — drag left edge of selected clip
- [x] **Trim out-point** — drag right edge of selected clip
- [x] **Seek/Scrub** — click and drag on ruler/playhead for continuous timeline scrubbing (no snap-back to 0)
- [x] **Zoom** — scroll wheel zoom (10–2000 px/s range)
- [x] **Pan** — horizontal scroll
- [x] **Undo/Redo** — Ctrl+Z / Ctrl+Y / Ctrl+Shift+Z; full command history
- [x] **Delete** — Delete/Backspace removes selected clip
- [x] **Play/Pause** — Space bar toggles player
- [x] Tool indicator overlay (Razor mode)

### Undo / Redo System
- [x] `EditCommand` trait with `execute` / `undo` / `description`
- [x] `EditHistory` with undo/redo stacks
- [x] Commands: MoveClip, TrimIn, TrimOut, DeleteClip, SplitClip
- [x] Live drag preview with commit-to-history on drag-end

### Inspector Panel
- [x] Right-side inspector showing selected clip properties
- [x] Fields: clip name, source path, source in/out, duration, timeline start

### Toolbar / Header
- [x] New / Open / Save / Export MP4 buttons
- [x] Undo / Redo buttons
- [x] Select / Razor tool toggle buttons

### Append to Timeline
- [x] "Append to Timeline" button in media browser
- [x] Appends marked region (in → out) of selected source clip
- [x] Placed at end of first Video track

### Export
- [x] MP4/H.264 + AAC export via ffmpeg (`-filter_complex` concat + adelay/amix for audio)
- [x] Background thread with `mpsc::channel` progress reporting
- [x] Audio from embedded video-clip streams and standalone audio-track clips included in export
- [x] Clips without audio streams safely skipped via `ffprobe` probe

### FCPXML
- [x] FCPXML 1.10 import (`quick-xml`) — parses assets, spine, asset-clip elements
- [x] FCPXML 1.10 export — writes resources/format/asset + library/event/project/sequence/spine

### MCP Server (`--mcp` flag)
- [x] `--mcp` flag enables the MCP (Model Context Protocol) server at startup
- [x] JSON-RPC 2.0 over stdio (MCP 2024-11-05 protocol)
- [x] `--mcp` flag is stripped from argv before GLib sees it
- [x] Background thread reads stdin; main-thread polling via `glib::timeout_add_local`
- [x] Tools: `get_project`, `list_tracks`, `list_clips`, `add_clip`, `remove_clip`, `move_clip`, `trim_clip`, `set_project_title`, `save_fcpxml`, `export_mp4`, `list_library`, `import_media`

---

## 🔜 Planned

### Source Monitor Improvements
- [x] Clip name shown in source monitor header
- [x] Frame-accurate jog/shuttle control
- [x] Mark-in / Mark-out visible as timecodes in a dedicated bar

### Timeline Improvements
- [x] Clip thumbnails in video track rows (background GStreamer extraction via `ThumbnailCache`)
- [x] Snap-to-clip-edge when moving clips (10 px threshold, snaps both start and end edges)
- [x] Multiple video tracks and audio tracks (Add/Remove Track buttons below timeline)
- [x] Audio waveform rendering in audio track rows (background GStreamer decode, normalized peaks)
- [x] Drag-and-drop from media browser onto a specific timeline track/position
- [ ] Snap-to-clip-edge when moving/trimming
- [ ] Timeline markers / chapter points
- [ ] Magnetic timeline mode (gap-free)
- [ ] Cross-dissolve transitions between clips
- [ ] Ripple/roll/slip/slide edit modes
- [ ] Reorder tracks in the timeline

### Program Monitor
- [x] Program Monitor panel showing assembled timeline playback
  - Dedicated `ProgramPlayer` advances clip-by-clip from the project model
  - Play/Stop transport controls; timecode display
  - Timeline seek (click ruler) also seeks the program monitor
  - Clips reload automatically on every project change

### Audio
- [x] Audio track clip display with waveform (see Timeline Improvements above)
- [ ] Volume / pan controls per clip in the inspector
- [ ] Basic audio mixing (level meters)

### Color & Effects
- [x] Basic color correction (brightness / contrast / saturation) via GStreamer `videobalance`
- [ ] LUT import / apply
- [ ] Apply multiple LUTs to a clip
- [ ] Titles / text overlay (`textoverlay`)
- [ ] Transition effects (fade, wipe, etc.)

### Project Management
- [x] Project save / load as FCPXML (wired to New/Open/Save buttons in toolbar)
- [ ] Recent projects list
- [ ] Auto-save
- [ ] Proxy media generation and management

### Export
- [ ] Export presets (resolution, bitrate, codec)
- [ ] ProRes / WebM / GIF export options
- [x] Export progress dialog with cancel (ProgressBar + status label)

### Polish
- [x] Keyboard shortcut reference overlay (? or / key opens a modal dialog)
- [ ] Preferences dialog (theme, default frame rate, etc.)
- [ ] Accessibility: keyboard navigation in all panels
- [ ] Welcome window for choosing recent project or new one
- [ ] Help documentation and tutorials
- [ ] Application icon and desktop integration (`.desktop` file)

---

## Architecture Notes

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the codebase layout,
key data-flow decisions, and agent contribution guidelines.
