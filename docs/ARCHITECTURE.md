# UltimateSlice — Architecture & Agent Guide

This document is the primary reference for AI agents and contributors working
on the UltimateSlice codebase. Read it before making changes.

---

## Required Documentation Updates (Agent Rule)

When making changes, update these files as part of the same work:

1. `CHANGELOG.md` — append a concise entry under **Unreleased** describing what changed and why.
2. `ROADMAP.md` — keep implemented/planned checklists accurate for any affected feature area.
3. **`docs/user/`** — update (or create) the relevant feature markdown file(s) in `docs/user/`:
   - Each feature has its own file (e.g. `timeline.md`, `inspector.md`, `export.md`).
   - Add or update keyboard shortcuts in both the feature file **and** `docs/user/shortcuts.md`.
   - Keep `docs/user/README.md` table of contents accurate.
4. MCP coverage — when adding a new user-facing feature, also add/update an MCP tool for it if one does not already exist and the feature is automatable via MCP. Test each feature using the MCP server.
5. Dependency/license coverage — when adding a new crate to `Cargo.toml`:
   - Verify the crate license is compatible with the project.
   - Add/update the crate listing in the in-app **About & Open-source credits** view.
   - Add/update the crate listing in `README.md`.

Do this continuously as work is completed (not only at the end of large efforts).

---

## Project Layout

```
src/
  main.rs                   Entry point — initialises env_logger, calls app::run()
  app.rs                    GApplication setup, CSS loading
  style.css                 Dark theme CSS for all GTK widgets

  model/
    clip.rs                 Clip struct — source path, source_in/out (ns), timeline_start, label, ClipKind (Video/Audio/Image/Title/Adjustment/Compound)
    track.rs                Track struct — id, TrackKind, Vec<Clip>, muted, locked
    project.rs              Project struct — title, FrameRate, resolution, Vec<Track>, dirty flag
    media_library.rs        MediaItem (library entry), MediaBin (folder), MediaLibrary (items + bins) + SourceMarks (source in/out state)

  media/
    audio_sync.rs           FFT cross-correlation audio sync (rustfft, GStreamer raw audio extraction)
    player.rs               GStreamer playbin wrapper (load/play/pause/stop/seek/position/duration)
    thumbnail.rs            Frame extraction via GStreamer AppSink pipeline (unused in UI yet)
    export.rs               MP4 export via ffmpeg subprocess: filter_complex concat (video) + adelay/amix (audio) → libx264 + aac
    proxy_cache.rs          Background proxy transcoding (half/quarter-res H.264 via ffmpeg) for preview playback

  fcpxml/
    parser.rs               FCPXML 1.10-1.14 → Project (quick-xml; parses assets, spine, asset-clip,
                            native <param>/<keyframeAnimation>/<keyframe> elements for FCP interop)
    writer.rs               Project → FCPXML 1.14 (emits native keyframe elements + us:* vendor attrs)

  otio/
    schema.rs               OTIO JSON schema types (serde Serialize/Deserialize) + time conversion helpers
    writer.rs               Project → OTIO JSON (implicit gaps → explicit, transitions, markers, metadata)
    parser.rs               OTIO JSON → Project (explicit gaps → implicit, transitions, markers, metadata)

  undo.rs                   EditCommand trait + EditHistory (undo/redo stacks)
                            Commands: MoveClip, TrimIn, TrimOut, DeleteClip, SplitClip

  ui/
    window.rs               Root window builder — wires all panels together, owns shared state
    toolbar.rs              HeaderBar — New/Open/Save/Export + Undo/Redo + Select/Razor toggles
    media_browser.rs        Media Library panel — import, list, select, Append to Timeline
    preview.rs              Source Monitor — video display, scrubber, in/out marks, transport
    inspector.rs            Right-side clip inspector — shows/edits selected clip properties
    preferences.rs          Preferences dialog — categorized app-level settings UI
    timeline/
      mod.rs                Re-exports TimelineState and build_timeline()
      widget.rs             Full timeline: Cairo drawing + all gesture/key controllers

  mcp/
    mod.rs                  McpCommand enum; start_mcp_server() → mpsc::Receiver<McpCommand>
    server.rs               Stdio JSON-RPC 2.0 loop; dispatches MCP tools to main thread

docs/
  ARCHITECTURE.md           This file — architecture reference and agent rules
  user/
    README.md               User documentation index
    shortcuts.md            Complete keyboard shortcut reference
    getting-started.md      Installation, build, window layout
    media-library.md        Importing and browsing source clips
    source-monitor.md       Source preview, In/Out points, shuttle controls
    timeline.md             Clip arrangement, trimming, tools, markers
    inspector.md            Per-clip color, effects, audio, transform, titles, speed
    preferences.md          Application-level settings and performance preferences
    program-monitor.md      Assembled timeline playback
    export.md               Advanced export: codecs, resolution, audio
    project-settings.md     Canvas size, frame rate, save/load
```

---

## Key Data Structures

### `TimelineState` (`src/ui/timeline/widget.rs`)

Shared via `Rc<RefCell<TimelineState>>` between the timeline widget and `window.rs`.

```rust
pub struct TimelineState {
    pub project: Rc<RefCell<Project>>,
    pub history: EditHistory,
    pub active_tool: ActiveTool,       // Select | Razor
    pub magnetic_mode: bool,           // gap-free edits on edited track when enabled
    pub pixels_per_second: f64,        // zoom level
    pub scroll_offset: f64,            // horizontal pan (pixels)
    pub playhead_ns: u64,              // current playhead in nanoseconds
    pub selected_clip_id: Option<String>,
    pub selected_track_id: Option<String>,
    drag_op: DragOp,                   // None | MoveClip | TrimIn | TrimOut (private)
    pub on_seek: Option<Rc<dyn Fn(u64)>>,
    pub on_project_changed: Option<Rc<dyn Fn()>>,
    pub on_play_pause: Option<Rc<dyn Fn()>>,
    pub on_drop_clip: Option<Rc<dyn Fn(String, u64, usize, u64)>>,
}
```

### `SourceMarks` (`src/model/media_library.rs`)

Shared via `Rc<RefCell<SourceMarks>>` between the media browser and preview panel.
Holds the currently-loaded source clip path and the user's in/out selection.

---

## Compound Clips, Timelines & Coordinate Spaces

### Data model

A compound clip is a regular `Clip` with `kind = ClipKind::Compound` and an
additional field `compound_tracks: Option<Vec<Track>>`. The internal tracks
contain full `Clip` objects with their own keyframes, subtitles, effects, etc.

When a compound clip is **created** from selected clips, each clip's
`timeline_start` is rebased so the earliest clip starts at internal position 0.
The compound clip is placed on the parent timeline at the original
`earliest_start` with `source_in = 0` and `source_out = internal_duration`.

### source_in windowing

`source_in` / `source_out` define a **visible window** into the compound's
internal timeline. A fresh compound has `source_in = 0`. After a razor cut,
the right half gets `source_in = cut_offset` — only internal content from
`source_in` onward is rendered.

**Critical**: when flattening internal clips for preview or export, the
compound's `source_in` must be accounted for:

```
absolute_position = compound.timeline_start + (inner.timeline_start - compound.source_in)
```

Do NOT use `saturating_sub(source_in)` on the compound offset — it underflows
to 0 when the compound is moved earlier than its original position, leaving a
gap. Instead, subtract `source_in` from each inner clip's position **after**
windowing (which guarantees `inner.timeline_start >= source_in`):

```rust
// CORRECT — no underflow risk
let rebased = compound_offset + (windowed.timeline_start - source_in);

// WRONG — underflows when timeline_start < source_in
let compound_offset = timeline_start.saturating_sub(source_in); // clamps to 0!
let rebased = compound_offset + windowed.timeline_start;
```

### Windowing internal clips

When flattening, internal clips outside the `[source_in, source_out]` window
must be excluded or trimmed:

1. Skip clips where `timeline_end() <= source_in` or `timeline_start >= source_out`
2. Trim left edge: increase `source_in`, set `timeline_start = window_start`
3. Trim right edge: decrease `source_out`
4. **Rebase keyframes**: call `retain_keyframes_in_local_range(left_trim, duration - right_trim)`
5. **Rebase subtitles**: call `retain_subtitles_in_local_range(left_trim, duration - right_trim)`

Keyframes and subtitles are in **clip-local time** (0 = clip start on
timeline). When the left edge is trimmed, their times must shift to stay
aligned with the content they reference.

### Three flattening paths

The same windowing logic must be applied in all three consumer paths:

| Path | File | Purpose |
|------|------|---------|
| `clip_to_program_clips()` | `src/ui/window.rs` | Preview playback |
| `flatten_clips()` | `src/media/export.rs` | MP4/ffmpeg export |
| `break_apart_compound()` | `src/ui/timeline/widget.rs` | Restore clips to parent |

### Drill-down editing

Double-clicking a compound enters drill-down mode via `compound_nav_stack`.
`resolve_editing_tracks()` navigates the stack to return the innermost
compound's internal tracks. Key rules:

- **Content height** must include the 22px breadcrumb bar
  (`+ st.breadcrumb_bar_height()`)
- **Track Y positions** (`track_row_top_in_tracks`, `track_index_at_y`) must
  offset by `breadcrumb_bar_height()` so hit testing aligns with drawing
- **Playhead** must be translated to compound-internal time via
  `editing_playhead_ns()`: `(playhead - compound.timeline_start) + compound.source_in`
- **Razor cuts** inside compounds use the translated playhead

### Clip lookup — always use recursive methods

Clips inside compounds are invisible to direct `project.tracks` iteration.
**Every** clip lookup must use the recursive `Project` methods:

```rust
// CORRECT — searches recursively through compound_tracks
project.clip_ref(&clip_id)   // read
project.clip_mut(&clip_id)   // write
project.track_mut(&track_id) // also recursive

// WRONG — only finds top-level clips
project.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == id)
for track in &mut project.tracks { for clip in &mut track.clips { ... } }
```

This applies to inspector handlers, MCP tool handlers, undo commands, and any
code that resolves a `clip_id` to a `&Clip` or `&mut Clip`.

### Multicam clips

Multicam clips store camera angles as `MulticamAngle` structs (source path +
in/out), not full `Clip` objects. `multicam_segments()` returns angle switch
segments relative to the **visible window** (accounting for `source_in`).
When flattening, add `clip.source_in` to segment positions to map into the
correct angle source offset.

### Subtitle timing coordinate space

Subtitle `start_ns` / `end_ns` and word-level timings are in **clip-local
time** — 0 corresponds to `source_in` (the start of the visible clip content).
They are NOT in absolute source-file time.

When converting to timeline-absolute time for rendering or export:

```rust
// CORRECT — subtitles are already relative to clip start
let abs_time = clip.timeline_start + (seg.start_ns as f64 / clip.speed) as u64;

// WRONG — double-counts source_in
let abs_time = clip.timeline_start + ((seg.start_ns - clip.source_in) / speed);
// Also WRONG:
let local_ns = clip_local_time + clip.source_in; // adds source_in
if local_ns >= seg.start_ns { ... }              // compares absolute vs relative
```

### FCPXML round-trip for compound/multicam clips

Compound and multicam clips have no `<asset>` in FCPXML `<resources>` (they
have no source file). The parser creates a **synthetic asset** when
`us:clip-kind` is `compound`, `multicam`, `title`, or `adjustment` so the
clip is parsed instead of silently dropped.

---

## Critical Rules for GTK4 + RefCell

### ⚠️ GTK4 C trampolines cannot unwind

Every GTK4 signal/gesture callback runs inside a `extern "C"` trampoline.
**Any Rust panic inside a callback is a hard abort** — there is no recovery.
This means `RefCell::borrow_mut()` panics (caused by double-borrow) are fatal.

### ⚠️ Never borrow a `RefCell` across a callback invocation

**Pattern to avoid:**
```rust
// WRONG — holds borrow_mut while calling cb() which re-borrows state
let mut st = state.borrow_mut();
if let Some(ref cb) = st.on_project_changed { cb(); } // cb() calls state.borrow() → PANIC
```

**Correct pattern — clone the Rc, drop the RefMut, then call:**
```rust
let proj_cb = st.on_project_changed.clone(); // clone Rc (cheap)
drop(st);                                     // release borrow_mut
if let Some(cb) = proj_cb { cb(); }           // safe: no active borrows
```

This is why all callbacks in `TimelineState` are `Option<Rc<dyn Fn()>>` (not `Box`)
— `Rc` is `Clone`, which allows extracting the callback before releasing the borrow.

### `on_project_changed` must always be called after dropping `state.borrow_mut()`

The `on_project_changed` closure (defined in `window.rs`) calls
`timeline_state.borrow().selected_clip_id` — a shared borrow of the same
`Rc<RefCell<TimelineState>>`. If any `borrow_mut()` is active when it fires, you get a
double-borrow abort.

**Same rule applies to any callback that touches shared `Rc<RefCell<...>>` state.**

### Methods that mutate state and need to notify

If a `&mut self` method (e.g., `delete_selected`, `razor_cut_at_playhead`) needs to
fire `on_project_changed`, **don't call it from inside the method**. Instead:
1. Do the mutation in the method (returns normally)
2. Let the caller clone `on_project_changed`, drop the `RefMut`, then fire

---

## GStreamer Notes

- **Library version**: `gstreamer-rs 0.25`, aligned on `glib 0.22`.
  Do not mix crates that pull in different glib versions (e.g., gstreamer 0.23 + gtk4 0.10).
- **Video sink**: `gtk4paintablesink` (optional `glsinkbin` wrapper for GPU upload).
  Get the paintable as: `sink.property::<glib::Object>("paintable").dynamic_cast::<gdk4::Paintable>()`.
- **Playback**: One shared `Player` instance (in `Rc<RefCell<Player>>`).
  Currently used as both a source monitor and a timeline player — they share the same pipeline.
- **Duration probe**: `gstreamer_pbutils::Discoverer` — run synchronously during import
  (acceptable; import is user-triggered, not in a tight loop).
- **API note**: In gstreamer-rs 0.25, `get_state(timeout)` became `state(Some(timeout))`.

---

## Adding a New Feature

### Adding a new timeline tool

1. Add a variant to `ActiveTool` in `widget.rs`.
2. Handle it in `click.connect_pressed` and `drag.connect_drag_begin`.
3. Add a `ToggleButton` to the toolbar in `toolbar.rs`.
4. Wire the button to set `timeline_state.borrow_mut().active_tool`.

### Adding a new undo-able edit command

1. Define a struct implementing `EditCommand` in `undo.rs`:
   ```rust
   pub struct MyCommand { /* fields capturing before/after state */ }
   impl EditCommand for MyCommand {
       fn execute(&self, proj: &mut Project) { /* apply */ }
       fn undo(&self, proj: &mut Project) { /* reverse */ }
       fn description(&self) -> &str { "My command" }
   }
   ```
2. Call `history.execute(Box::new(cmd), &mut proj)` to apply + push to stack.
   For live-drag edits (applied incrementally), push directly to `history.undo_stack`
   after the drag ends (bypasses re-execution).

### Adding a new panel / view

1. Create `src/ui/my_panel.rs` with a `build_my_panel(...)` function returning a GTK widget.
2. Declare it in `src/ui/mod.rs`: `pub mod my_panel;`
3. Add it to the layout in `window.rs` using `Paned` or `Box`.
4. Pass shared state (`Rc<RefCell<...>>`) and callbacks (`Rc<dyn Fn()>`) as parameters —
   **never** use global/static state.

### Sharing state between panels

- Wrap state in `Rc<RefCell<T>>` and pass clones to each panel.
- For notifications: use `Rc<dyn Fn()>` callbacks (not channels — this is single-threaded GTK).
- Always follow the borrow safety rules above.

---

## Dependency Versions (Cargo.toml)

| Crate | Version | Notes |
|---|---|---|
| `gtk4` | `0.11` | glib 0.22 |
| `gdk4` | `0.11` | glib 0.22 |
| `pango` | `0.22` | Font description for title font chooser |
| `glib` | `0.22` | shared base |
| `gio` | `0.22` | GIO |
| `gstreamer` | `0.25` | glib 0.22 |
| `gstreamer-video` | `0.25` | |
| `gstreamer-pbutils` | `0.25` | Discoverer |
| `gstreamer-app` | `0.25` | AppSink |
| `quick-xml` | `0.37` | FCPXML parsing |
| `serde` | `1` | serialization |
| `uuid` | `1` | clip IDs |
| `serde_json` | `1` | JSON for MCP |
| `anyhow` | `1` | error handling |
| `thiserror` | `1` | error types |
| `log` + `env_logger` | latest | logging |
| `rustfft` | `6` | FFT for audio cross-correlation sync |
| `ort` | `2.0.0-rc.12` | ONNX Runtime for AI background removal and MusicGen inference |
| `ndarray` | `0.17` | N-dimensional array for ONNX tensor I/O |
| `tokenizers` | `0.21` | Hugging Face tokenizer for MusicGen T5 text encoding |
| `hound` | `3` | WAV audio file writer for MusicGen output |
| `tempfile` | `3` | Temporary files for ffmpeg chapter metadata |

**Do not upgrade gstreamer without also upgrading gtk4/gdk4/glib to the matching glib version.**

---

## Running

```bash
cargo build
cargo run
# With GStreamer debug output:
GST_DEBUG=2 cargo run
# With MCP server enabled (stdio JSON-RPC):
cargo run -- --mcp
# Attach to a running instance via Unix socket (stdio proxy):
cargo run -- --mcp-attach
# Via installed Flatpak (used by .mcp.json / AI agents):
flatpak run io.github.kmwallio.ultimateslice --mcp
```

> **Flatpak build:** Run `python3 flatpak-cargo-generator.py Cargo.lock -o cargo-sources.json`
> then `flatpak-builder --user --install --force-clean flatpak-build io.github.kmwallio.ultimateslice.yml`
> after any dependency changes (Cargo.lock update) to regenerate `cargo-sources.json`.
> The ONNX Runtime Flatpak mirror lives in `onnxruntime-sources.json`; regenerate it whenever the pinned ONNX Runtime version
> or its mirrored CPU/shared-lib `cmake/deps.txt` inputs change.

> **Single-instance enforcement for `--mcp`:** Only one MCP-enabled instance may
> run at a time. On startup with `--mcp`, the binary reads
> `/tmp/ultimateslice-mcp.pid`, sends SIGTERM (then SIGKILL after 3 s) to any
> prior instance, and writes its own PID to the file. The PID file is removed on
> normal exit. This lets agents or CI scripts safely restart the server by simply
> re-launching with `--mcp`.

---

## MCP Server

UltimateSlice exposes a Model Context Protocol server (newline-delimited
JSON-RPC 2.0, protocol version `2024-11-05`) via two transports:

1. **Stdio** (`--mcp` flag) — agents spawn the process and pipe stdin/stdout.
2. **Unix domain socket** (Preferences → Integration toggle) — agents connect to
   a running instance at `$XDG_RUNTIME_DIR/ultimateslice-mcp.sock`.

For socket transport, a built-in stdio proxy (`--mcp-attach`) bridges
stdin/stdout to the socket so standard MCP clients can use either transport
via `.mcp.json`.

### Architecture

```
Agent (stdin/stdout)                  Agent (Unix socket)
    ↓ JSON-RPC lines                      ↓ JSON-RPC lines
MCP stdio thread                     MCP socket thread (per-connection)
    ↓ McpCommand + SyncSender            ↓ McpCommand + SyncSender
    └──────────── shared mpsc channel ────┘
                        ↓
            GTK main thread  (src/ui/window.rs — polled every 10 ms)
                ↓ mutates Project, calls on_project_changed()
                ↑ sends Value reply back via SyncSender
```

`--mcp-attach` stdio proxy (no GUI):
```
Agent (stdio) ↔ ultimate-slice --mcp-attach ↔ Unix socket ↔ running instance
```

Key design points:
- The MCP thread **blocks** waiting for each reply — requests are serialized.
- The main thread **never blocks** — it drains the channel via `try_recv()` in a timer.
- `McpCommand` variants carry a `std::sync::mpsc::SyncSender<serde_json::Value>`
  as a one-shot reply channel. All types are `Send`.
- `glib::Sender` / `MainContext::channel` are **not used** (API changed in glib 0.22).
- The socket server accepts **one client at a time**; additional connections are
  rejected with a JSON-RPC error.
- The socket can be enabled/disabled at runtime via Preferences; the listener
  thread is started/stopped accordingly.

### Agent completion verification (required)

Before declaring a task finished, agents must verify via MCP:

1. A **new project** can be created and media can be imported.
2. An **existing project** can be opened.
3. When possible, any new or modified functionality is exercised via MCP.

### Available Tools

| Tool | Description |
|---|---|
| `get_project` | Full project JSON (title, tracks, clips) |
| `batch_call_tools` | Execute multiple MCP tool calls in-order in one request; supports optional `stop_on_error` and `include_timing`, returning per-call success/error records (plus optional elapsed timing) |
| `list_tracks` | Track list; accepts optional `compact` flag for automation-focused output (`index/id/kind/clip_count` only) |
| `list_clips` | Clip list; accepts optional `compact` flag for automation-focused timing/source output (`id/source path/track/timing`) |
| `get_timeline_settings` | Timeline settings JSON (includes `magnetic_mode`) |
| `get_playhead_position` | Current program playhead position (`timeline_pos_ns`) |
| `set_magnetic_mode` | Enable/disable magnetic (gap-free) timeline mode |
| `set_track_solo` | Set solo state for a track id; soloed non-muted tracks become the active preview/export set |
| `list_ladspa_plugins` | List all available LADSPA audio effect plugins with parameters |
| `add_clip_ladspa_effect` | Add a LADSPA audio effect to a clip by plugin name |
| `remove_clip_ladspa_effect` | Remove a LADSPA audio effect from a clip by effect id |
| `set_clip_ladspa_effect_params` | Set parameters on a LADSPA audio effect instance |
| `set_track_role` | Set audio role for a track (`none`/`dialogue`/`effects`/`music`) for submix categorization |
| `set_track_duck` | Enable/disable automatic ducking on a track; ducked tracks have volume reduced when dialogue is present |
| `close_source_preview` | Deselect current source media and hide the source preview |
| `get_preferences` | Get persisted application preferences |
| `set_hardware_acceleration` | Set hardware-acceleration preference and apply to source preview playback |
| `set_playback_priority` | Set program-monitor playback priority (`smooth` / `balanced` / `accurate`) |
| `set_source_playback_priority` | Set source-monitor playback priority (`smooth` / `balanced` / `accurate`) |
| `set_crossfade_settings` | Set automatic audio crossfade preferences (`enabled`, `curve`, `duration_ns`) with strict validation |
| `set_proxy_mode` | Set proxy preview mode (`off` / `half_res` / `quarter_res`) |
| `set_gsk_renderer` | Set GTK renderer backend (`auto` / `cairo` / `opengl` / `vulkan`); requires restart |
| `set_preview_quality` | Set compositor preview quality (`auto` / `full` / `half` / `quarter`); takes effect immediately |
| `set_realtime_preview` | Toggle real-time preview decoder prebuilds (`true` / `false`) |
| `set_experimental_preview_optimizations` | Toggle occlusion optimization (audio-only decode for fully-occluded clips) |
| `set_background_prerender` | Toggle background prerender of complex overlap windows (`true` / `false`) |
| `set_preview_luts` | Toggle LUT-baked project-resolution preview media generation when proxy mode is off (`true` / `false`) |
| `add_clip` | Add source clip(s) at `track_index` + timeline position using source-placement rules (Source Monitor A/V auto-link enabled: linked A/V pair when both matching kinds exist + embedded-video-audio suppression; disabled: single-clip placement; single-kind fallback otherwise) |
| `remove_clip` | Remove clip by id |
| `move_clip` | Change a clip's `timeline_start_ns` |
| `link_clips` | Assign a shared clip link group to two or more clips |
| `unlink_clips` | Clear clip link groups for the provided clips and their linked peers |
| `align_grouped_clips_by_timecode` | Align grouped clips referenced by clip ids using stored source-time metadata |
| `sync_clips_by_audio` | Synchronize 2+ clips by FFT audio cross-correlation (first clip is anchor); optional `replace_audio` flag links clips and mutes anchor's embedded audio |
| `copy_clip_color_grade` | Copy color grading static values from a clip into the internal color-grade clipboard |
| `paste_clip_color_grade` | Paste previously copied color grading values onto a target clip |
| `trim_clip` | Change a clip's `source_in_ns` / `source_out_ns` |
| `slip_clip` | Shift a clip's source window by a delta (source_in/out move equally, timeline position fixed) |
| `slide_clip` | Move a clip on timeline by a delta, adjusting neighbor edit points to compensate |
| `insert_clip` | Insert source clip(s) at `timeline_pos_ns` (or playhead when omitted) using source-placement rules (including optional Source Monitor A/V auto-link behavior); shifts subsequent clips right on affected track(s) |
| `overwrite_clip` | Overwrite timeline content at `timeline_pos_ns` (or playhead when omitted) with source clip(s) (3-point overwrite) using source-placement rules (including optional Source Monitor A/V auto-link behavior) on affected track(s) |
| `seek_playhead` | Seek the timeline/program monitor to an absolute `timeline_pos_ns` |
| `export_displayed_frame` | Export current program-monitor displayed frame to an image file (PPM/P6) |
| `play` | Start program monitor playback |
| `pause` | Pause program monitor playback |
| `stop` | Stop program monitor playback and return playhead to start |
| `take_screenshot` | Capture a PNG screenshot of the full application window (GTK snapshot + GSK CairoRenderer); saved to CWD as `ultimateslice-screenshot-<epoch>.png` |
| `match_frame` | Match Frame: load a timeline clip's source in the Source Monitor and seek to the matching source timecode (uses selected clip or optional `clip_id`) |
| `set_clip_stabilization` | Enable/configure video stabilization (libvidstab) on a clip; applied during export |
| `set_clip_transform` | Set scale, position, and optional rotation/anamorphic offset for a clip. scale > 1.0 zooms in (crops), scale < 1.0 zooms out (letterbox). position_x/y shift the frame from -1.0 (full left/top) to 1.0 (full right/bottom). rotate is in degrees (-180 to 180 typical). anamorphic_desqueeze applies lens expansion (e.g. 1.33, 2.0). |
| `list_backups` | List available versioned backup files with timestamps and sizes |
| `set_clip_color` | Set brightness/contrast/saturation on a clip by id |
| `set_clip_opacity` | Set a clip opacity value (`0.0`–`1.0`) by id |
| `set_clip_eq` | Set 3-band parametric EQ on a clip (optional per-band `low_freq`/`low_gain`/`low_q`, `mid_freq`/`mid_gain`/`mid_q`, `high_freq`/`high_gain`/`high_q`; omitted fields keep current value) |
| `normalize_clip_audio` | Analyze clip loudness and normalize volume; `mode` (`peak`/`lufs`), `target_level` (dB); blocks during ffmpeg analysis (1–5 s) |
| `detect_scene_cuts` | Detect scene/shot changes in a clip using ffmpeg `scdet` and split at each cut point; `threshold` (1–50, default 10); blocks during analysis |
| `generate_music` | Generate music from a text prompt using MusicGen AI; `prompt` (required), `duration_secs` (1–30, default 10), optional `track_index`/`timeline_start_ns`; returns immediately, clip appears when generation completes |
| `record_voiceover` | Record audio from microphone for `duration_ns` at playhead position; places WAV clip on audio track; blocks during recording |
| `set_clip_keyframe` | Set/update a phase-1 keyframe (`scale`/`opacity`/`position_x`/`position_y`/`brightness`/`contrast`/`saturation`/`temperature`/`tint`/`volume`/`pan`/`rotate`/`crop_left`/`crop_right`/`crop_top`/`crop_bottom`/`eq_low_gain`/`eq_mid_gain`/`eq_high_gain`) at an absolute timeline position |
| `remove_clip_keyframe` | Remove a phase-1 keyframe for a property at an absolute timeline position |
| `set_clip_chroma_key` | Set chroma key (green/blue screen) params on a clip by id |
| `set_clip_mask` | Set shape mask on a clip (rectangle or ellipse) to restrict visible area |
| `set_project_title` | Rename the project |
| `save_fcpxml` | Write FCPXML 1.14 to a file path |
| `save_edl` | Export timeline to CMX 3600 EDL (.edl) file |
| `save_otio` | Export the current project to OpenTimelineIO (.otio) JSON file |
| `open_otio` | Load a project from an OpenTimelineIO (.otio) file, replacing the current project |
| `export_mp4` | Encode timeline to MP4/H.264+AAC via ffmpeg (blocks until done, up to 11 min timeout) |
| `list_export_presets` | List saved export presets from UI state |
| `save_export_preset` | Create or overwrite a named export preset |
| `delete_export_preset` | Delete a named export preset |
| `export_with_preset` | Export to a path using a named export preset |
| `list_library` | Items in the media library (not yet on timeline), including missing/offline status |
| `import_media` | Import a file into the library; probes duration via GStreamer Discoverer |
| `relink_media` | Recursively scan a root folder and remap missing media paths to matching files |
| `create_bin` | Create a media library bin (folder) with optional parent for nesting (max 2 levels) |
| `delete_bin` | Delete a media library bin; items and child bins move to parent or root |
| `rename_bin` | Rename a media library bin |
| `list_bins` | List all bins with hierarchy and item counts |
| `move_to_bin` | Move media items to a bin (or root if bin_id is null) |
| `reorder_track` | Move a track from one index to another (undoable) |
| `set_transition` | Set/clear clip-boundary transitions (e.g. `cross_dissolve`) by track/clip index |
| `create_project` | Discard the current project and start a new empty one (optional title) |
| `add_adjustment_layer` | Add an adjustment layer clip at a track index and timeline position; effects apply to composited result of all tracks below |
| `create_compound_clip` | Create a compound (nested timeline) clip from specified clip IDs; replaces selected clips with a single compound clip |
| `break_apart_compound_clip` | Break apart a compound clip, restoring its internal clips to the timeline |
| `create_multicam_clip` | Create a multicam clip from 2+ clip IDs synced by audio cross-correlation |
| `add_angle_switch` | Insert an angle switch at a position within a multicam clip |
| `list_multicam_angles` | List angles and switch points of a multicam clip |
| `set_multicam_angle_audio` | Set volume (0.0–2.0) and/or mute state for a multicam angle's audio; unmuted angles mix together |

For automation-heavy loops, MCP keeps a short-lived per-session read cache for repeated `get_project`, `list_tracks`, and `list_clips` calls. Both direct tool calls and `batch_call_tools` can reuse this cache, and it is invalidated when a mutating tool runs so subsequent reads observe the updated state.

### Example session

```jsonc
// → initialize
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"claude","version":"1"}}}

// ← response
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"ultimateslice","version":"0.1.0"}}}

// → list tools
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}

// → add a clip at 5 seconds
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"add_clip","arguments":{"source_path":"/home/user/footage.mp4","track_index":0,"timeline_start_ns":5000000000,"source_in_ns":0,"source_out_ns":10000000000}}}
```

### Adding a new MCP tool

1. Add a variant to `McpCommand` in `src/mcp/mod.rs`
2. Add a matching entry to the `tools_list()` function in `src/mcp/server.rs`
3. Add a dispatch arm in `call_tool()` in `src/mcp/server.rs`
4. Add a handler arm in `handle_mcp_command()` in `src/ui/window.rs`

Required system packages (Debian/Ubuntu):
```
build-essential cmake pkg-config
libgtk-4-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev
libgstreamer-plugins-bad1.0-dev gstreamer1.0-plugins-good
gstreamer1.0-plugins-bad gstreamer1.0-gl libglib2.0-dev
```

---

## See Also

- [`ROADMAP.md`](../ROADMAP.md) — implemented and planned features
