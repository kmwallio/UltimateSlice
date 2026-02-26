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

Do this continuously as work is completed (not only at the end of large efforts).

---

## Project Layout

```
src/
  main.rs                   Entry point — initialises env_logger, calls app::run()
  app.rs                    GApplication setup, CSS loading
  style.css                 Dark theme CSS for all GTK widgets

  model/
    clip.rs                 Clip struct — source path, source_in/out (ns), timeline_start, label, ClipKind
    track.rs                Track struct — id, TrackKind, Vec<Clip>, muted, locked
    project.rs              Project struct — title, FrameRate, resolution, Vec<Track>, dirty flag
    media_library.rs        MediaItem (library entry) + SourceMarks (source in/out state)

  media/
    player.rs               GStreamer playbin wrapper (load/play/pause/stop/seek/position/duration)
    thumbnail.rs            Frame extraction via GStreamer AppSink pipeline (unused in UI yet)
    export.rs               MP4 export via ffmpeg subprocess: filter_complex concat (video) + adelay/amix (audio) → libx264 + aac

  fcpxml/
    parser.rs               FCPXML 1.10 → Project (quick-xml; parses assets, spine, asset-clip)
    writer.rs               Project → FCPXML 1.10

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
```

> **Single-instance enforcement for `--mcp`:** Only one MCP-enabled instance may
> run at a time. On startup with `--mcp`, the binary reads
> `/tmp/ultimateslice-mcp.pid`, sends SIGTERM (then SIGKILL after 3 s) to any
> prior instance, and writes its own PID to the file. The PID file is removed on
> normal exit. This lets agents or CI scripts safely restart the server by simply
> re-launching with `--mcp`.

---

## MCP Server

When started with `--mcp`, UltimateSlice exposes a Model Context Protocol server
over **stdio** (newline-delimited JSON-RPC 2.0, protocol version `2024-11-05`).
Agents connect by spawning the process and piping stdio.

### Architecture

```
Agent (stdin/stdout)
    ↓ JSON-RPC lines
MCP server thread  (src/mcp/server.rs)
    ↓ McpCommand + SyncSender<Value>  (std::sync::mpsc)
GTK main thread  (src/ui/window.rs — polled every 10 ms)
    ↓ mutates Project, calls on_project_changed()
    ↑ sends Value reply back via SyncSender
MCP server thread
    ↑ JSON-RPC response to stdout
```

Key design points:
- The MCP thread **blocks** waiting for each reply — requests are serialized.
- The main thread **never blocks** — it drains the channel via `try_recv()` in a timer.
- `McpCommand` variants carry a `std::sync::mpsc::SyncSender<serde_json::Value>`
  as a one-shot reply channel. All types are `Send`.
- `glib::Sender` / `MainContext::channel` are **not used** (API changed in glib 0.22).

### Available Tools

| Tool | Description |
|---|---|
| `get_project` | Full project JSON (title, tracks, clips) |
| `list_tracks` | Track index, id, kind, clip count |
| `list_clips` | All clips with id, path, track\_index, ns positions |
| `get_timeline_settings` | Timeline settings JSON (includes `magnetic_mode`) |
| `set_magnetic_mode` | Enable/disable magnetic (gap-free) timeline mode |
| `close_source_preview` | Deselect current source media and hide the source preview |
| `get_preferences` | Get persisted application preferences |
| `set_hardware_acceleration` | Set hardware-acceleration preference and apply to source preview playback |
| `add_clip` | Add clip at track\_index + timeline position |
| `remove_clip` | Remove clip by id |
| `move_clip` | Change a clip's `timeline_start_ns` |
| `trim_clip` | Change a clip's `source_in_ns` / `source_out_ns` |
| `set_clip_color` | Set brightness/contrast/saturation on a clip by id |
| `set_project_title` | Rename the project |
| `save_fcpxml` | Write FCPXML 1.10 to a file path |
| `export_mp4` | Encode timeline to MP4/H.264+AAC via ffmpeg (blocks until done, up to 11 min timeout) |
| `list_library` | Items in the media library (not yet on timeline) |
| `import_media` | Import a file into the library; probes duration via GStreamer Discoverer |
| `reorder_track` | Move a track from one index to another (undoable) |
| `set_transition` | Set/clear clip-boundary transitions (e.g. `cross_dissolve`) by track/clip index |

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
libgtk-4-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev
libgstreamer-plugins-bad1.0-dev gstreamer1.0-plugins-good
gstreamer1.0-plugins-bad gstreamer1.0-gl libglib2.0-dev
```

---

## See Also

- [`ROADMAP.md`](../ROADMAP.md) — implemented and planned features
