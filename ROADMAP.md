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
- [x] Snap-to-clip-edge when trimming (TrimIn and TrimOut snap to nearby edges)
- [ ] Timeline markers / chapter points ✅
- [ ] Magnetic timeline mode (gap-free)
- [ ] Cross-dissolve transitions between clips
- [ ] Ripple/roll/slip/slide edit modes
- [ ] Reorder tracks in the timeline

### Speed Ramps (per clip)
- [x] Constant speed change per clip (e.g. 0.5× slow-mo, 2× fast-forward) via GStreamer rate seek + ffmpeg `setpts`/`atempo` on export
- [x] Speed indicator badge on clip in timeline (yellow "2×" badge)
- [x] Persist speed data in FCPXML (`us:speed` attribute)
- [ ] Variable speed ramps: multiple keyframed speed segments within a single clip
- [ ] Reverse playback
- [ ] Optical flow / frame-blending for smooth slow-motion (ffmpeg `minterpolate` on export)

### Program Monitor
- [x] Program Monitor panel showing assembled timeline playback
  - Dedicated `ProgramPlayer` advances clip-by-clip from the project model
  - Play/Stop transport controls; timecode display
  - Timeline seek (click ruler) also seeks the program monitor
  - Clips reload automatically on every project change
- [ ] Preview rendering performance pass
  - [ ] Build a compositor-based preview pipeline (`compositor` + layered video tracks) so B-roll/overlays render in preview without clip switching
  - [ ] Run decode + waveform/thumbnail extraction on background workers with bounded queues and cancellation to keep GTK main thread responsive
  - [ ] Add short frame cache around playhead (previous/current/next frames) to reduce stutter on scrubbing and pause/seek
  - [ ] Introduce proxy preview mode (quarter/half resolution decode, full-res export) for large media
  - [x] Throttle UI redraws to monitor refresh rate and coalesce timeline invalidations (avoid redundant `queue_draw`)
  - [ ] Reuse per-clip filter bins/elements across seeks where possible instead of rebuilding pipeline state on every handoff

### Audio
- [x] Audio track clip display with waveform (see Timeline Improvements above)
- [x] Volume / pan controls per clip in the inspector (sliders, GStreamer volume + audiopanorama, persisted in FCPXML)
- [ ] Basic audio mixing (level meters)

### Color & Effects
- [x] Basic color correction (brightness / contrast / saturation) via GStreamer `videobalance`
- [x] Denoise filter per clip (GStreamer `gaussianblur` positive sigma; ffmpeg `hqdn3d` on export)
- [x] Sharpness / unsharp-mask per clip (GStreamer `gaussianblur` negative sigma; ffmpeg `unsharp` on export)
- [ ] LUT import / apply
- [ ] Apply multiple LUTs to a clip
- [x] Titles / text overlay (`textoverlay`)
- [ ] Transition effects (fade, wipe, etc.)

### Video Transform (per clip)
- [ ] Scale / resize clip (zoom in/out within frame)
- [x] Crop clip (left / right / top / bottom margins) via GStreamer `videocrop`
- [x] Rotate clip (90° / 180° / 270° presets) via GStreamer `videoflip`
- [x] Flip horizontal / flip vertical via GStreamer `videoflip`
- [ ] Position offset (X / Y translation within the output frame)
- [x] Persist transform settings in FCPXML (`us:crop-*`, `us:rotate`, `us:flip-h/v` attributes)
- [ ] Interactive crop/rotate handles in program monitor (removed; use Inspector sliders instead)

### Project Management
- [x] Project save / load as FCPXML (wired to New/Open/Save buttons in toolbar)
- [x] Recent projects list
- [x] Auto-save (60s timer, writes to /tmp/ultimateslice-autosave.fcpxml when project is dirty)
- [ ] Proxy media generation and management

### Canvas / Sequence Settings
- [x] Canvas size dialog (project resolution: 1080p, 4K, custom W×H)
- [x] Frame rate selector in project settings (23.976, 24, 25, 29.97, 30, 60 fps)
- [ ] Aspect ratio presets (16:9, 4:3, 9:16 vertical, 1:1 square)
- [x] Persist canvas settings in FCPXML `<format>` element

### Export
- [x] Advanced export dialog (replace current single-button export)
  - Codec selection: H.264, H.265/HEVC, VP9, ProRes, AV1
  - Container selection: MP4, MOV, WebM, MKV
  - Output resolution presets with downscale support (4K → 1080p → 720p → custom)
  - Bitrate control: CRF / target bitrate mode
  - Audio codec: AAC, Opus, FLAC, PCM
  - Audio sample rate and channel layout (stereo / mono)
- [ ] Export presets: save/load named configurations (e.g. "Twitter 720p", "Archive ProRes")
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
