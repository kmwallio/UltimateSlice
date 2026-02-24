# UltimateSlice Roadmap

A Final Cut Pro–inspired non-linear video editor built with GTK4 and Rust.

---

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
- [x] **Seek** — click ruler or scrub playhead
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
- [x] MP4/H.264 export via GStreamer pipeline (`concat → x264enc + aacenc → mp4mux`)
- [x] Background thread with `mpsc::channel` progress reporting

### FCPXML
- [x] FCPXML 1.10 import (`quick-xml`) — parses assets, spine, asset-clip elements
- [x] FCPXML 1.10 export — writes resources/format/asset + library/event/project/sequence/spine

### MCP Server (`--mcp` flag)
- [x] `--mcp` flag enables the MCP (Model Context Protocol) server at startup
- [x] JSON-RPC 2.0 over stdio (MCP 2024-11-05 protocol)
- [x] `--mcp` flag is stripped from argv before GLib sees it
- [x] Background thread reads stdin; main-thread polling via `glib::timeout_add_local`
- [x] Tools: `get_project`, `list_tracks`, `list_clips`, `add_clip`, `remove_clip`, `move_clip`, `trim_clip`, `set_project_title`, `save_fcpxml`

---

## 🔜 Planned

### Source Monitor Improvements
- [ ] Frame-accurate jog/shuttle control
- [ ] Mark-in / Mark-out visible as timecodes in a dedicated bar
- [ ] Clip name shown in source monitor header

### Timeline Improvements
- [ ] Clip thumbnails in video track rows (frame extraction via AppSink)
- [ ] Audio waveform rendering in audio track rows
- [ ] Multiple video tracks and audio tracks (add/remove tracks)
- [ ] Drag-and-drop from media browser onto a specific timeline track/position
- [ ] Snap-to-clip-edge when moving/trimming
- [ ] Timeline markers / chapter points
- [ ] Magnetic timeline mode (gap-free)
- [ ] Cross-dissolve transitions between clips
- [ ] Ripple/roll/slip/slide edit modes

### Program Monitor
- [ ] Separate "Program Monitor" that composites and plays back the assembled timeline
  (requires a GStreamer `concat` + `compositor` pipeline driven by the timeline model)
- [ ] Preview the output of the timeline in real-time

### Audio
- [ ] Audio track clip display with waveform
- [ ] Volume / pan controls per clip in the inspector
- [ ] Basic audio mixing (level meters)

### Color & Effects
- [ ] Basic color correction (brightness / contrast / saturation) via GStreamer `videobalance`
- [ ] LUT import / apply
- [ ] Titles / text overlay (`textoverlay`)

### Project Management
- [ ] Project save / load as FCPXML (wired to New/Open/Save buttons)
- [ ] Recent projects list
- [ ] Auto-save
- [ ] Proxy media generation and management

### Export
- [ ] Export presets (resolution, bitrate, codec)
- [ ] ProRes / WebM / GIF export options
- [ ] Export progress dialog with cancel

### Polish
- [ ] Keyboard shortcut reference overlay (? key)
- [ ] Preferences dialog (theme, default frame rate, etc.)
- [ ] Accessibility: keyboard navigation in all panels
- [ ] Application icon and desktop integration (`.desktop` file)

---

## Architecture Notes

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the codebase layout,
key data-flow decisions, and agent contribution guidelines.
