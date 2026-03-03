use crate::mcp::McpCommand;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const PROTOCOL_VERSION: &str = "2024-11-05";

/// Transport-agnostic MCP JSON-RPC loop.  Reads newline-delimited JSON-RPC
/// messages from `reader`, dispatches tool calls via `sender`, and writes
/// JSON-RPC responses to `writer`.
fn run_server(
    reader: impl BufRead,
    writer: &mut impl Write,
    sender: &std::sync::mpsc::Sender<McpCommand>,
) {
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let line = line.trim().to_owned();
        if line.is_empty() {
            continue;
        }

        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let r = err(Value::Null, -32700, &format!("Parse error: {e}"));
                let _ = writeln!(writer, "{r}");
                let _ = writer.flush();
                continue;
            }
        };

        // MCP notifications carry no "id" — do not respond.
        let id = match msg.get("id") {
            Some(id) => id.clone(),
            None => continue,
        };

        let method = msg["method"].as_str().unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));

        let response = match method {
            "initialize" => ok(&id, initialize_result()),
            "ping" => ok(&id, json!({})),
            "tools/list" => ok(&id, tools_list()),
            "resources/list" => ok(&id, json!({"resources": []})),
            "tools/call" => call_tool(&id, &params, sender),
            _ => err(id, -32601, "Method not found"),
        };

        let _ = writeln!(writer, "{response}");
        let _ = writer.flush();
    }
}

/// Stdio transport — wraps stdin/stdout around the generic handler.
pub fn run_stdio_server(sender: std::sync::mpsc::Sender<McpCommand>) {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    run_server(stdin.lock(), &mut stdout, &sender);
}

/// Return the path used for the MCP Unix domain socket.
pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    dir.join("ultimateslice-mcp.sock")
}

/// Unix domain socket transport.  Listens for one client at a time; rejects
/// additional connections while a session is active.  Exits when `stop` is set.
pub fn run_socket_server(sender: std::sync::mpsc::Sender<McpCommand>, stop: Arc<AtomicBool>) {
    use std::os::unix::net::UnixListener;

    let path = socket_path();
    let _ = std::fs::remove_file(&path); // remove stale socket

    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[MCP-socket] Failed to bind {}: {e}", path.display());
            return;
        }
    };
    listener.set_nonblocking(true).ok();
    eprintln!("[MCP-socket] Listening on {}", path.display());

    let client_active = Arc::new(AtomicBool::new(false));

    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                // Enforce single-client: reject if one is already connected.
                if client_active
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_err()
                {
                    let mut w = &stream;
                    let _ = writeln!(
                        w,
                        "{}",
                        json!({"jsonrpc":"2.0","id":null,"error":{"code":-32000,"message":"Another MCP client is already connected"}})
                    );
                    continue;
                }
                eprintln!("[MCP-socket] Client connected");
                let sender = sender.clone();
                let active = client_active.clone();
                std::thread::spawn(move || {
                    stream.set_nonblocking(false).ok();
                    let reader = BufReader::new(match stream.try_clone() {
                        Ok(s) => s,
                        Err(_) => {
                            active.store(false, Ordering::Relaxed);
                            return;
                        }
                    });
                    let mut writer = stream;
                    run_server(reader, &mut writer, &sender);
                    active.store(false, Ordering::Relaxed);
                    eprintln!("[MCP-socket] Client disconnected");
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => {
                eprintln!("[MCP-socket] Accept error: {e}");
                break;
            }
        }
    }

    let _ = std::fs::remove_file(&path);
    eprintln!("[MCP-socket] Server stopped");
}

// ── JSON-RPC helpers ─────────────────────────────────────────────────────────

fn ok(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn err(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn text_content(v: Value) -> Value {
    json!({"content": [{"type": "text", "text": v.to_string()}]})
}

// ── MCP initialize ────────────────────────────────────────────────────────────

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name":    "ultimateslice",
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

// ── Tool list ─────────────────────────────────────────────────────────────────

fn tools_list() -> Value {
    json!({ "tools": [
        {
            "name": "get_project",
            "description": "Return the full project state (title, frame rate, all tracks and clips) as JSON.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "list_tracks",
            "description": "List all tracks in the project with their index, id, kind (Video/Audio), and clip count.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "list_clips",
            "description": "List every clip on the timeline across all tracks.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "get_timeline_settings",
            "description": "Return timeline behavior settings, including magnetic mode state.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "set_magnetic_mode",
            "description": "Enable or disable magnetic timeline mode (gap-free edits on the edited track).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "enabled": { "type": "boolean", "description": "Whether magnetic mode should be enabled." }
                },
                "required": ["enabled"]
            }
        },
        {
            "name": "close_source_preview",
            "description": "Hide the source preview and clear current source selection.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "get_preferences",
            "description": "Return current application preferences.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "set_hardware_acceleration",
            "description": "Set hardware acceleration preference and apply it to source preview playback.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "enabled": { "type": "boolean", "description": "Whether hardware acceleration preference is enabled." }
                },
                "required": ["enabled"]
            }
        },
        {
            "name": "set_playback_priority",
            "description": "Set program monitor playback priority ('smooth', 'balanced', or 'accurate').",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "priority": { "type": "string", "enum": ["smooth", "balanced", "accurate"], "description": "Playback priority mode." }
                },
                "required": ["priority"]
            }
        },
        {
            "name": "add_clip",
            "description": "Add a new clip to a track. Durations are in nanoseconds (1 s = 1_000_000_000 ns).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source_path":       { "type": "string",  "description": "Absolute path to the media file." },
                    "track_index":       { "type": "integer", "description": "0-based track index (default: 0)." },
                    "timeline_start_ns": { "type": "integer", "description": "Timeline position in nanoseconds." },
                    "source_in_ns":      { "type": "integer", "description": "Source in-point in nanoseconds." },
                    "source_out_ns":     { "type": "integer", "description": "Source out-point in nanoseconds." }
                },
                "required": ["source_path", "timeline_start_ns", "source_in_ns", "source_out_ns"]
            }
        },
        {
            "name": "remove_clip",
            "description": "Remove a clip from the timeline by its unique id (obtained from list_clips).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id to remove." }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "move_clip",
            "description": "Move a clip to a new timeline start position in nanoseconds.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id":      { "type": "string",  "description": "Clip id to move." },
                    "new_start_ns": { "type": "integer", "description": "New timeline start in nanoseconds." }
                },
                "required": ["clip_id", "new_start_ns"]
            }
        },
        {
            "name": "trim_clip",
            "description": "Adjust the source in- and out-point of a clip without changing its timeline position.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id":       { "type": "string",  "description": "Clip id to trim." },
                    "source_in_ns":  { "type": "integer", "description": "New source in-point in nanoseconds." },
                    "source_out_ns": { "type": "integer", "description": "New source out-point in nanoseconds." }
                },
                "required": ["clip_id", "source_in_ns", "source_out_ns"]
            }
        },
        {
            "name": "slip_clip",
            "description": "Slip a clip: shift its source window (source_in and source_out) by a delta without changing timeline position or duration.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id":  { "type": "string",  "description": "Clip id to slip." },
                    "delta_ns": { "type": "integer", "description": "Nanoseconds to shift the source window. Positive shifts forward (later source content), negative shifts backward." }
                },
                "required": ["clip_id", "delta_ns"]
            }
        },
        {
            "name": "slide_clip",
            "description": "Slide a clip: move its timeline position by a delta while adjusting neighboring clips to compensate.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id":  { "type": "string",  "description": "Clip id to slide." },
                    "delta_ns": { "type": "integer", "description": "Nanoseconds to slide on the timeline. Positive slides right, negative slides left." }
                },
                "required": ["clip_id", "delta_ns"]
            }
        },
        {
            "name": "set_clip_color",
            "description": "Set color correction and denoise/sharpness effects for a clip by id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id":    { "type": "string",  "description": "Clip id (from list_clips)." },
                    "brightness": { "type": "number",  "description": "Brightness adjustment: -1.0 (darkest) to 1.0 (brightest). Default 0.0." },
                    "contrast":   { "type": "number",  "description": "Contrast multiplier: 0.0 to 2.0. Default 1.0." },
                    "saturation": { "type": "number",  "description": "Saturation multiplier: 0.0 (greyscale) to 2.0 (vivid). Default 1.0." },
                    "denoise":    { "type": "number",  "description": "Denoise strength: 0.0 (off) to 1.0 (heavy). Default 0.0." },
                    "sharpness":  { "type": "number",  "description": "Sharpness: -1.0 (soften) to 1.0 (sharpen). Default 0.0." },
                    "shadows":    { "type": "number",  "description": "Shadow grading: -1.0 (crush) to 1.0 (lift). Default 0.0." },
                    "midtones":   { "type": "number",  "description": "Midtone grading: -1.0 (darken) to 1.0 (brighten). Default 0.0." },
                    "highlights": { "type": "number",  "description": "Highlight grading: -1.0 (pull down) to 1.0 (boost). Default 0.0." }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "set_project_title",
            "description": "Rename the project.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "New project title." }
                },
                "required": ["title"]
            }
        },
        {
            "name": "save_fcpxml",
            "description": "Export the current project to a Final Cut Pro XML (.fcpxml) file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path for the output .fcpxml file." }
                },
                "required": ["path"]
            }
        },
        {
            "name": "open_fcpxml",
            "description": "Load a project from a Final Cut Pro XML (.fcpxml) file, replacing the current project.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path to the .fcpxml file to open." }
                },
                "required": ["path"]
            }
        },
        {
            "name": "export_mp4",
            "description": "Export the current project to MP4/H.264 at the given absolute path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path for the output .mp4 file." }
                },
                "required": ["path"]
            }
        },
        {
            "name": "list_library",
            "description": "List all items currently in the media library (imported but not necessarily on the timeline).",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "import_media",
            "description": "Import a media file into the library by absolute path. Probes duration via GStreamer Discoverer.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path to the media file to import." }
                },
                "required": ["path"]
            }
        },
        {
            "name": "reorder_track",
            "description": "Move a track from one position to another (0-based indices).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from_index": { "type": "integer", "description": "Current 0-based track index." },
                    "to_index":   { "type": "integer", "description": "Target 0-based track index." }
                },
                "required": ["from_index", "to_index"]
            }
        },
        {
            "name": "set_transition",
            "description": "Set or clear a clip-boundary transition on a track. clip_index refers to the clip that transitions into the next clip on the same track.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "track_index": { "type": "integer", "description": "0-based track index." },
                    "clip_index":  { "type": "integer", "description": "0-based clip index within the track (must have a next clip)." },
                    "kind":        { "type": "string",  "description": "Transition kind. Use 'cross_dissolve' to set, or empty string to clear." },
                    "duration_ns": { "type": "integer", "description": "Transition duration in nanoseconds." }
                },
                "required": ["track_index", "clip_index", "kind", "duration_ns"]
            }
        },
        {
            "name": "set_proxy_mode",
            "description": "Set proxy preview mode ('off', 'half_res', or 'quarter_res'). When enabled, lightweight proxy files are generated for smoother preview playback. Export always uses original media.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "mode": { "type": "string", "enum": ["off", "half_res", "quarter_res"], "description": "Proxy preview mode." }
                },
                "required": ["mode"]
            }
        },
        {
            "name": "set_clip_lut",
            "description": "Assign or clear a 3D LUT (.cube) file for a clip. The LUT is applied on export via ffmpeg lut3d. Pass null or omit lut_path to clear.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id":  { "type": "string", "description": "Clip id (from list_clips)." },
                    "lut_path": { "type": ["string", "null"], "description": "Absolute path to a .cube LUT file, or null to clear." }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "set_clip_transform",
            "description": "Set scale and position offset for a clip. scale > 1.0 zooms in (crops), scale < 1.0 zooms out (letterbox). position_x/y shift the frame from -1.0 (full left/top) to 1.0 (full right/bottom).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id":    { "type": "string", "description": "Clip id (from list_clips)." },
                    "scale":      { "type": "number", "description": "Zoom scale factor: 1.0 = normal, 2.0 = 2× zoom in, 0.5 = half size. Range 0.1–4.0." },
                    "position_x": { "type": "number", "description": "Horizontal offset: -1.0 (left) to 1.0 (right). Default 0.0 (center)." },
                    "position_y": { "type": "number", "description": "Vertical offset: -1.0 (top) to 1.0 (bottom). Default 0.0 (center)." }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "set_clip_opacity",
            "description": "Set clip opacity for compositing. 1.0 is fully opaque; 0.0 is fully transparent.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id":  { "type": "string", "description": "Clip id (from list_clips)." },
                    "opacity":  { "type": "number", "description": "Opacity in range 0.0–1.0 (default 1.0)." }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "create_project",
            "description": "Create a new empty project, discarding the current one. Resets the timeline to a blank state.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "Title for the new project (default: 'Untitled')." }
                }
            }
        },
        {
            "name": "set_gsk_renderer",
            "description": "Set the GTK renderer backend. Use 'cairo' on devices with limited GPU memory to avoid Vulkan out-of-memory errors. Requires application restart to take effect.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "renderer": { "type": "string", "enum": ["auto", "cairo", "opengl", "vulkan"], "description": "GTK renderer backend." }
                },
                "required": ["renderer"]
            }
        },
        {
            "name": "set_preview_quality",
            "description": "Set compositor preview quality. Lower quality reduces memory and CPU usage for smoother playback on low-end hardware. Use 'auto' to adapt quality to current Program Monitor size. Export always uses full project resolution.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "quality": { "type": "string", "enum": ["auto", "full", "half", "quarter"], "description": "Preview quality level. 'auto' adapts to monitor size, 'half' halves both dimensions (4x less pixels), 'quarter' quarters them (16x less pixels)." }
                },
                "required": ["quality"]
            }
        },
        {
            "name": "insert_clip",
            "description": "Insert a source clip at the playhead position, shifting all subsequent clips right to make room (3-point insert edit).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source_path": { "type": "string", "description": "Absolute path to the media file." },
                    "source_in_ns": { "type": "integer", "description": "Source in-point in nanoseconds." },
                    "source_out_ns": { "type": "integer", "description": "Source out-point in nanoseconds." },
                    "track_index": { "type": "integer", "description": "Optional target track index. Omit to use the active or first matching track." }
                },
                "required": ["source_path", "source_in_ns", "source_out_ns"]
            }
        },
        {
            "name": "overwrite_clip",
            "description": "Overwrite timeline content at the playhead position with a source clip, replacing existing material in the time range (3-point overwrite edit).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source_path": { "type": "string", "description": "Absolute path to the media file." },
                    "source_in_ns": { "type": "integer", "description": "Source in-point in nanoseconds." },
                    "source_out_ns": { "type": "integer", "description": "Source out-point in nanoseconds." },
                    "track_index": { "type": "integer", "description": "Optional target track index. Omit to use the active or first matching track." }
                },
                "required": ["source_path", "source_in_ns", "source_out_ns"]
            }
        },
        {
            "name": "seek_playhead",
            "description": "Seek the timeline/program-monitor playhead to an absolute timeline position in nanoseconds.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "timeline_pos_ns": { "type": "integer", "description": "Absolute timeline position in nanoseconds." }
                },
                "required": ["timeline_pos_ns"]
            }
        },
        {
            "name": "export_displayed_frame",
            "description": "Export the currently displayed program-monitor frame to an image file (binary PPM/P6 format).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute output file path (recommended .ppm extension)." }
                },
                "required": ["path"]
            }
        },
        {
            "name": "play",
            "description": "Start program monitor playback from the current playhead position.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "pause",
            "description": "Pause program monitor playback, holding the current position.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "stop",
            "description": "Stop program monitor playback and return the playhead to the beginning.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "take_screenshot",
            "description": "Capture a PNG screenshot of the full application window using the GTK snapshot and renderer. The PNG is written to the current working directory with a timestamped filename and the path is returned.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "select_library_item",
            "description": "Select a media library item by its source path, loading it into the Source Monitor for preview. The item must already be in the library (use import_media first).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path of the library item to select." }
                },
                "required": ["path"]
            }
        },
        {
            "name": "source_play",
            "description": "Start playback in the Source Monitor (source preview player).",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "source_pause",
            "description": "Pause playback in the Source Monitor (source preview player).",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ]})
}

// ── Tool dispatch ─────────────────────────────────────────────────────────────

fn call_tool(id: &Value, params: &Value, sender: &std::sync::mpsc::Sender<McpCommand>) -> Value {
    let name = params["name"].as_str().unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    // Every tool call gets a dedicated one-shot reply channel.
    let (tx, rx) = std::sync::mpsc::sync_channel::<Value>(1);

    let cmd = match name {
        "get_project" => McpCommand::GetProject { reply: tx },
        "list_tracks" => McpCommand::ListTracks { reply: tx },
        "list_clips" => McpCommand::ListClips { reply: tx },
        "get_timeline_settings" => McpCommand::GetTimelineSettings { reply: tx },
        "set_magnetic_mode" => McpCommand::SetMagneticMode {
            enabled: args["enabled"].as_bool().unwrap_or(false),
            reply: tx,
        },
        "close_source_preview" => McpCommand::CloseSourcePreview { reply: tx },
        "get_preferences" => McpCommand::GetPreferences { reply: tx },
        "set_hardware_acceleration" => McpCommand::SetHardwareAcceleration {
            enabled: args["enabled"].as_bool().unwrap_or(false),
            reply: tx,
        },
        "set_playback_priority" => McpCommand::SetPlaybackPriority {
            priority: args["priority"].as_str().unwrap_or("smooth").to_string(),
            reply: tx,
        },

        "add_clip" => McpCommand::AddClip {
            source_path: args["source_path"].as_str().unwrap_or("").to_string(),
            track_index: args["track_index"].as_u64().unwrap_or(0) as usize,
            timeline_start_ns: args["timeline_start_ns"].as_u64().unwrap_or(0),
            source_in_ns: args["source_in_ns"].as_u64().unwrap_or(0),
            source_out_ns: args["source_out_ns"].as_u64().unwrap_or(0),
            reply: tx,
        },

        "remove_clip" => McpCommand::RemoveClip {
            clip_id: args["clip_id"].as_str().unwrap_or("").to_string(),
            reply: tx,
        },

        "move_clip" => McpCommand::MoveClip {
            clip_id: args["clip_id"].as_str().unwrap_or("").to_string(),
            new_start_ns: args["new_start_ns"].as_u64().unwrap_or(0),
            reply: tx,
        },

        "trim_clip" => McpCommand::TrimClip {
            clip_id: args["clip_id"].as_str().unwrap_or("").to_string(),
            source_in_ns: args["source_in_ns"].as_u64().unwrap_or(0),
            source_out_ns: args["source_out_ns"].as_u64().unwrap_or(0),
            reply: tx,
        },

        "slip_clip" => McpCommand::SlipClip {
            clip_id: args["clip_id"].as_str().unwrap_or("").to_string(),
            delta_ns: args["delta_ns"].as_i64().unwrap_or(0),
            reply: tx,
        },

        "slide_clip" => McpCommand::SlideClip {
            clip_id: args["clip_id"].as_str().unwrap_or("").to_string(),
            delta_ns: args["delta_ns"].as_i64().unwrap_or(0),
            reply: tx,
        },

        "set_clip_color" => McpCommand::SetClipColor {
            clip_id: args["clip_id"].as_str().unwrap_or("").to_string(),
            brightness: args["brightness"].as_f64().unwrap_or(0.0),
            contrast: args["contrast"].as_f64().unwrap_or(1.0),
            saturation: args["saturation"].as_f64().unwrap_or(1.0),
            denoise: args["denoise"].as_f64().unwrap_or(0.0),
            sharpness: args["sharpness"].as_f64().unwrap_or(0.0),
            shadows: args["shadows"].as_f64().unwrap_or(0.0),
            midtones: args["midtones"].as_f64().unwrap_or(0.0),
            highlights: args["highlights"].as_f64().unwrap_or(0.0),
            reply: tx,
        },

        "set_project_title" => McpCommand::SetTitle {
            title: args["title"].as_str().unwrap_or("").to_string(),
            reply: tx,
        },

        "save_fcpxml" => McpCommand::SaveFcpxml {
            path: args["path"].as_str().unwrap_or("").to_string(),
            reply: tx,
        },

        "open_fcpxml" => McpCommand::OpenFcpxml {
            path: args["path"].as_str().unwrap_or("").to_string(),
            reply: tx,
        },

        "export_mp4" => McpCommand::ExportMp4 {
            path: args["path"].as_str().unwrap_or("").to_string(),
            reply: tx,
        },

        "list_library" => McpCommand::ListLibrary { reply: tx },

        "import_media" => McpCommand::ImportMedia {
            path: args["path"].as_str().unwrap_or("").to_string(),
            reply: tx,
        },

        "reorder_track" => McpCommand::ReorderTrack {
            from_index: args["from_index"].as_u64().unwrap_or(0) as usize,
            to_index: args["to_index"].as_u64().unwrap_or(0) as usize,
            reply: tx,
        },
        "set_transition" => McpCommand::SetTransition {
            track_index: args["track_index"].as_u64().unwrap_or(0) as usize,
            clip_index: args["clip_index"].as_u64().unwrap_or(0) as usize,
            kind: args["kind"].as_str().unwrap_or("").to_string(),
            duration_ns: args["duration_ns"].as_u64().unwrap_or(0),
            reply: tx,
        },
        "set_proxy_mode" => McpCommand::SetProxyMode {
            mode: args["mode"].as_str().unwrap_or("off").to_string(),
            reply: tx,
        },
        "set_clip_lut" => McpCommand::SetClipLut {
            clip_id: args["clip_id"].as_str().unwrap_or("").to_string(),
            lut_path: match &args["lut_path"] {
                Value::String(s) => Some(s.clone()),
                Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::Array(_)
                | Value::Object(_) => None,
            },
            reply: tx,
        },
        "set_clip_transform" => McpCommand::SetClipTransform {
            clip_id: args["clip_id"].as_str().unwrap_or("").to_string(),
            scale: args["scale"].as_f64().unwrap_or(1.0),
            position_x: args["position_x"].as_f64().unwrap_or(0.0),
            position_y: args["position_y"].as_f64().unwrap_or(0.0),
            reply: tx,
        },
        "set_clip_opacity" => McpCommand::SetClipOpacity {
            clip_id: args["clip_id"].as_str().unwrap_or("").to_string(),
            opacity: args["opacity"].as_f64().unwrap_or(1.0),
            reply: tx,
        },

        "create_project" => McpCommand::CreateProject {
            title: args["title"].as_str().unwrap_or("Untitled").to_string(),
            reply: tx,
        },

        "set_gsk_renderer" => McpCommand::SetGskRenderer {
            renderer: args["renderer"].as_str().unwrap_or("auto").to_string(),
            reply: tx,
        },

        "set_preview_quality" => McpCommand::SetPreviewQuality {
            quality: args["quality"].as_str().unwrap_or("full").to_string(),
            reply: tx,
        },

        "insert_clip" => McpCommand::InsertClip {
            source_path: args["source_path"].as_str().unwrap_or("").to_string(),
            source_in_ns: args["source_in_ns"].as_u64().unwrap_or(0),
            source_out_ns: args["source_out_ns"].as_u64().unwrap_or(0),
            track_index: args["track_index"].as_u64().map(|v| v as usize),
            reply: tx,
        },

        "overwrite_clip" => McpCommand::OverwriteClip {
            source_path: args["source_path"].as_str().unwrap_or("").to_string(),
            source_in_ns: args["source_in_ns"].as_u64().unwrap_or(0),
            source_out_ns: args["source_out_ns"].as_u64().unwrap_or(0),
            track_index: args["track_index"].as_u64().map(|v| v as usize),
            reply: tx,
        },

        "play" => McpCommand::Play { reply: tx },
        "pause" => McpCommand::Pause { reply: tx },
        "stop" => McpCommand::Stop { reply: tx },
        "seek_playhead" => McpCommand::SeekPlayhead {
            timeline_pos_ns: args["timeline_pos_ns"].as_u64().unwrap_or(0),
            reply: tx,
        },
        "export_displayed_frame" => McpCommand::ExportDisplayedFrame {
            path: args["path"].as_str().unwrap_or("").to_string(),
            reply: tx,
        },
        "take_screenshot" => McpCommand::TakeScreenshot { reply: tx },
        "select_library_item" => McpCommand::SelectLibraryItem {
            path: args["path"].as_str().unwrap_or("").to_string(),
            reply: tx,
        },
        "source_play" => McpCommand::SourcePlay { reply: tx },
        "source_pause" => McpCommand::SourcePause { reply: tx },

        _ => return err(id.clone(), -32602, &format!("Unknown tool: '{name}'")),
    };

    if sender.send(cmd).is_err() {
        return err(id.clone(), -32603, "App main thread unavailable");
    }

    match rx.recv() {
        Ok(result) => ok(id, text_content(result)),
        Err(_) => err(id.clone(), -32603, "No reply from app"),
    }
}
