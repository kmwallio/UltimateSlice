use crate::mcp::McpCommand;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const PROTOCOL_VERSION: &str = "2024-11-05";
const SESSION_READ_CACHE_TTL_MS: u64 = 150;

/// Transport-agnostic MCP JSON-RPC loop.  Reads newline-delimited JSON-RPC
/// messages from `reader`, dispatches tool calls via `sender`, and writes
/// JSON-RPC responses to `writer`.
fn run_server(
    reader: impl BufRead,
    writer: &mut impl Write,
    sender: &std::sync::mpsc::Sender<McpCommand>,
) {
    let mut session_read_cache: std::collections::HashMap<String, (Value, std::time::Instant)> =
        std::collections::HashMap::new();
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
            "tools/call" => call_tool(&id, &params, sender, &mut session_read_cache),
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
            "description": "List all tracks in the project with index/id/kind, clip count, muted/locked/soloed flags, and track height preset.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "compact": { "type": "boolean", "description": "When true, return only automation-critical fields (index/id/kind/clip_count)." }
                }
            }
        },
        {
            "name": "list_clips",
            "description": "List every clip on the timeline across all tracks, including color label and timing/effect metadata.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "compact": { "type": "boolean", "description": "When true, return only automation-critical timing/source fields." }
                }
            }
        },
        {
            "name": "get_timeline_settings",
            "description": "Return timeline behavior settings, including magnetic mode state.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "get_playhead_position",
            "description": "Return the current program playhead position in nanoseconds.",
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
            "name": "set_track_solo",
            "description": "Set solo state for a track by id. When any track is soloed, only soloed non-muted tracks are active in preview/export.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "track_id": { "type": "string", "description": "Target track id from list_tracks." },
                    "solo": { "type": "boolean", "description": "Whether the target track should be soloed." }
                },
                "required": ["track_id", "solo"]
            }
        },
        {
            "name": "set_track_height_preset",
            "description": "Set timeline display height preset for a track by id ('small', 'medium', or 'large').",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "track_id": { "type": "string", "description": "Target track id from list_tracks." },
                    "height_preset": { "type": "string", "enum": ["small", "medium", "large"], "description": "Track display height preset." }
                },
                "required": ["track_id", "height_preset"]
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
            "name": "set_source_playback_priority",
            "description": "Set source monitor playback priority ('smooth', 'balanced', or 'accurate').",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "priority": { "type": "string", "enum": ["smooth", "balanced", "accurate"], "description": "Source playback priority mode." }
                },
                "required": ["priority"]
            }
        },
        {
            "name": "set_crossfade_settings",
            "description": "Set automatic audio crossfade preferences (enabled flag, curve, and duration).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "enabled": { "type": "boolean", "description": "Whether automatic audio crossfades are enabled." },
                    "curve": { "type": "string", "enum": ["equal_power", "linear"], "description": "Crossfade curve shape." },
                    "duration_ns": { "type": "integer", "description": "Crossfade duration in nanoseconds (10_000_000 to 10_000_000_000)." }
                },
                "required": ["enabled", "curve", "duration_ns"]
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
            "name": "link_clips",
            "description": "Assign a shared link group to two or more timeline clips so selection/move/delete operations stay synchronized.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Two or more clip ids to link together."
                    }
                },
                "required": ["clip_ids"]
            }
        },
        {
            "name": "unlink_clips",
            "description": "Clear the shared link group for the provided clips and any peers already linked with them.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "One or more clip ids whose link groups should be cleared."
                    }
                },
                "required": ["clip_ids"]
            }
        },
        {
            "name": "align_grouped_clips_by_timecode",
            "description": "Align the grouped clips referenced by the provided clip ids using stored source timecode metadata.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "One or more clip ids whose clip groups should be aligned by timecode."
                    }
                },
                "required": ["clip_ids"]
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
                    "temperature":{ "type": "number",  "description": "Color temperature in Kelvin: 2000 (warm/amber) to 10000 (cool/blue). Default 6500." },
                    "tint":       { "type": "number",  "description": "Tint on green-magenta axis: -1.0 (green) to 1.0 (magenta). Default 0.0." },
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
            "name": "set_clip_color_label",
            "description": "Set semantic timeline color label for a clip by id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id (from list_clips)." },
                    "color_label": { "type": "string", "enum": ["none", "red", "orange", "yellow", "green", "teal", "blue", "purple", "magenta"], "description": "Clip color label." }
                },
                "required": ["clip_id", "color_label"]
            }
        },
        {
            "name": "set_clip_chroma_key",
            "description": "Set chroma key (green/blue screen) settings for a clip by id. Makes the keyed color transparent so lower tracks show through.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id":    { "type": "string",  "description": "Clip id (from list_clips)." },
                    "enabled":    { "type": "boolean", "description": "Enable/disable chroma key." },
                    "color":      { "type": "integer", "description": "Target color as 0xRRGGBB integer. Default 0x00FF00 (green). Use 0x0000FF for blue screen." },
                    "tolerance":  { "type": "number",  "description": "Key tolerance: 0.0 (tight) to 1.0 (wide). Default 0.3." },
                    "softness":   { "type": "number",  "description": "Edge softness: 0.0 (hard) to 1.0 (soft). Default 0.1." }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "set_clip_bg_removal",
            "description": "Set AI background removal settings for a clip by id. Uses offline ONNX segmentation (MODNet) to produce an alpha-channel version of the clip.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id":   { "type": "string",  "description": "Clip id (from list_clips)." },
                    "enabled":   { "type": "boolean", "description": "Enable/disable background removal." },
                    "threshold": { "type": "number",  "description": "Alpha threshold: 0.0 (aggressive) to 1.0 (conservative). Default 0.5." }
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
            "description": "Export the current project to a Final Cut Pro XML (.fcpxml) file using FCPXML 1.14.",
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
            "description": "Load a project from a Final Cut Pro XML (.fcpxml) file (supports versions up to 1.14), replacing the current project.",
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
            "name": "list_export_presets",
            "description": "List saved named export presets from local UI state.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "save_export_preset",
            "description": "Create or overwrite a named export preset.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Preset name." },
                    "video_codec": { "type": "string", "enum": ["h264", "h265", "vp9", "prores", "av1"] },
                    "container": { "type": "string", "enum": ["mp4", "mov", "webm", "mkv"] },
                    "output_width": { "type": "integer", "description": "Output width, or 0 to use project width." },
                    "output_height": { "type": "integer", "description": "Output height, or 0 to use project height." },
                    "crf": { "type": "integer", "description": "CRF quality value (0-51)." },
                    "audio_codec": { "type": "string", "enum": ["aac", "opus", "flac", "pcm"] },
                    "audio_bitrate_kbps": { "type": "integer", "description": "Audio bitrate in kbps." }
                },
                "required": ["name", "video_codec", "container", "output_width", "output_height", "crf", "audio_codec", "audio_bitrate_kbps"]
            }
        },
        {
            "name": "delete_export_preset",
            "description": "Delete a saved export preset by name.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Preset name." }
                },
                "required": ["name"]
            }
        },
        {
            "name": "export_with_preset",
            "description": "Export the current project to a path using a named saved export preset.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute output file path." },
                    "preset_name": { "type": "string", "description": "Name of the saved export preset." }
                },
                "required": ["path", "preset_name"]
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
            "description": "Set scale, position, and optional rotation offset for a clip. scale > 1.0 zooms in (crops), scale < 1.0 zooms out (letterbox). position_x/y shift the frame from -1.0 (full left/top) to 1.0 (full right/bottom). rotate is in degrees (-180 to 180 typical).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id":    { "type": "string", "description": "Clip id (from list_clips)." },
                    "scale":      { "type": "number", "description": "Zoom scale factor: 1.0 = normal, 2.0 = 2× zoom in, 0.5 = half size. Range 0.1–4.0." },
                    "position_x": { "type": "number", "description": "Horizontal offset: -1.0 (left) to 1.0 (right). Default 0.0 (center)." },
                    "position_y": { "type": "number", "description": "Vertical offset: -1.0 (top) to 1.0 (bottom). Default 0.0 (center)." },
                    "rotate":     { "type": "integer", "description": "Rotation in degrees. Optional; omit to keep existing value." }
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
            "name": "set_clip_keyframe",
            "description": "Create or update a phase-1 keyframe (position_x, position_y, scale, opacity, brightness, contrast, saturation, temperature, tint, volume, pan, speed, rotate, crop_left, crop_right, crop_top, crop_bottom) for a clip at a timeline position.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id (from list_clips)." },
                    "property": { "type": "string", "enum": ["position_x", "position_y", "scale", "opacity", "brightness", "contrast", "saturation", "temperature", "tint", "volume", "pan", "speed", "rotate", "crop_left", "crop_right", "crop_top", "crop_bottom"], "description": "Animated property to keyframe." },
                    "timeline_pos_ns": { "type": "integer", "description": "Absolute timeline position in nanoseconds. Optional; defaults to current playhead." },
                    "value": { "type": "number", "description": "Property value at this keyframe time." },
                    "interpolation": { "type": "string", "enum": ["linear", "ease_in", "ease_out", "ease_in_out"], "description": "Interpolation mode for the segment following this keyframe. Optional; defaults to linear." }
                },
                "required": ["clip_id", "property", "value"]
            }
        },
        {
            "name": "remove_clip_keyframe",
            "description": "Remove a phase-1 keyframe (position_x, position_y, scale, opacity, brightness, contrast, saturation, temperature, tint, volume, pan, speed, rotate, crop_left, crop_right, crop_top, crop_bottom) at a timeline position for a clip.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id (from list_clips)." },
                    "property": { "type": "string", "enum": ["position_x", "position_y", "scale", "opacity", "brightness", "contrast", "saturation", "temperature", "tint", "volume", "pan", "speed", "rotate", "crop_left", "crop_right", "crop_top", "crop_bottom"], "description": "Animated property keyframe lane." },
                    "timeline_pos_ns": { "type": "integer", "description": "Absolute timeline position in nanoseconds. Optional; defaults to current playhead." }
                },
                "required": ["clip_id", "property"]
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
            "name": "set_realtime_preview",
            "description": "Enable or disable real-time preview. When enabled, upcoming decoder slots are pre-built so clip transitions are near-instant. Uses more CPU and memory.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "enabled": { "type": "boolean", "description": "true to enable, false to disable." }
                },
                "required": ["enabled"]
            }
        },
        {
            "name": "set_experimental_preview_optimizations",
            "description": "Enable or disable experimental preview optimizations (audio-only decode for fully-occluded clips) during multi-track playback.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "enabled": { "type": "boolean", "description": "true to enable, false to disable." }
                },
                "required": ["enabled"]
            }
        },
        {
            "name": "set_background_prerender",
            "description": "Enable or disable background disk prerender for upcoming complex overlap playback sections.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "enabled": { "type": "boolean", "description": "true to enable, false to disable." }
                },
                "required": ["enabled"]
            }
        },
        {
            "name": "set_preview_luts",
            "description": "Enable or disable LUT-baked project-resolution preview media generation when Proxy mode is Off.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "enabled": { "type": "boolean", "description": "true to enable, false to disable." }
                },
                "required": ["enabled"]
            }
        },
        {
            "name": "insert_clip",
            "description": "Insert a source clip at a target timeline position (defaults to playhead), shifting all subsequent clips right to make room (3-point insert edit).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source_path": { "type": "string", "description": "Absolute path to the media file." },
                    "source_in_ns": { "type": "integer", "description": "Source in-point in nanoseconds." },
                    "source_out_ns": { "type": "integer", "description": "Source out-point in nanoseconds." },
                    "track_index": { "type": "integer", "description": "Optional target track index. Omit to use the active or first matching track." },
                    "timeline_pos_ns": { "type": "integer", "description": "Optional absolute timeline position in nanoseconds. If omitted, uses current playhead." }
                },
                "required": ["source_path", "source_in_ns", "source_out_ns"]
            }
        },
        {
            "name": "overwrite_clip",
            "description": "Overwrite timeline content at a target timeline position (defaults to playhead) with a source clip, replacing existing material in the time range (3-point overwrite edit).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source_path": { "type": "string", "description": "Absolute path to the media file." },
                    "source_in_ns": { "type": "integer", "description": "Source in-point in nanoseconds." },
                    "source_out_ns": { "type": "integer", "description": "Source out-point in nanoseconds." },
                    "track_index": { "type": "integer", "description": "Optional target track index. Omit to use the active or first matching track." },
                    "timeline_pos_ns": { "type": "integer", "description": "Optional absolute timeline position in nanoseconds. If omitted, uses current playhead." }
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
            "name": "export_timeline_snapshot",
            "description": "Render the timeline panel to a PNG image file. Useful for verifying timeline overlays like keyframe markers in headless environments.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute output file path (recommended .png extension)." },
                    "width": { "type": "integer", "description": "Output image width in pixels (default 1920)." },
                    "height": { "type": "integer", "description": "Output image height in pixels (default 1080)." }
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
        },
        {
            "name": "batch_call_tools",
            "description": "Execute multiple MCP tool calls in-order within one request. Returns per-call success/error records.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "calls": {
                        "type": "array",
                        "description": "Ordered list of tool calls: [{\"name\":\"tool_name\",\"arguments\":{...}}].",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string" },
                                "arguments": { "type": "object" }
                            },
                            "required": ["name"]
                        }
                    },
                    "stop_on_error": {
                        "type": "boolean",
                        "description": "When true, stop executing remaining calls after the first error (default: false)."
                    },
                    "include_timing": {
                        "type": "boolean",
                        "description": "When true, include elapsed_ms per call and total_elapsed_ms in the batch result (default: false)."
                    }
                },
                "required": ["calls"]
            }
        },
        {
            "name": "sync_clips_by_audio",
            "description": "Synchronize two or more timeline clips by audio cross-correlation. The first clip is used as the anchor; other clips are repositioned based on matching audio content. Returns offset and confidence for each non-anchor clip.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Two or more clip ids to sync. First clip is the anchor."
                    }
                },
                "required": ["clip_ids"]
            }
        }
    ]})
}

// ── Tool dispatch ─────────────────────────────────────────────────────────────

fn tool_error_payload(code: i32, message: impl Into<String>) -> Value {
    json!({"code": code, "message": message.into()})
}

fn is_cacheable_read_tool(name: &str) -> bool {
    matches!(
        name,
        "get_project"
            | "list_tracks"
            | "list_clips"
            | "get_timeline_settings"
            | "get_playhead_position"
            | "get_preferences"
            | "list_export_presets"
            | "list_library"
    )
}

fn is_session_cacheable_read_tool(name: &str) -> bool {
    matches!(name, "get_project" | "list_tracks" | "list_clips")
}

fn cache_key(name: &str, args: &Value) -> String {
    let args_json = serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string());
    format!("{name}\u{1f}{args_json}")
}

fn dispatch_tool_payload(
    name: &str,
    args: &Value,
    sender: &std::sync::mpsc::Sender<McpCommand>,
) -> Result<Value, Value> {
    // Every tool call gets a dedicated one-shot reply channel.
    let (tx, rx) = std::sync::mpsc::sync_channel::<Value>(1);

    let cmd = match name {
        "get_project" => McpCommand::GetProject { reply: tx },
        "list_tracks" => McpCommand::ListTracks {
            compact: args["compact"].as_bool().unwrap_or(false),
            reply: tx,
        },
        "list_clips" => McpCommand::ListClips {
            compact: args["compact"].as_bool().unwrap_or(false),
            reply: tx,
        },
        "get_timeline_settings" => McpCommand::GetTimelineSettings { reply: tx },
        "get_playhead_position" => McpCommand::GetPlayheadPosition { reply: tx },
        "set_magnetic_mode" => McpCommand::SetMagneticMode {
            enabled: args["enabled"].as_bool().unwrap_or(false),
            reply: tx,
        },
        "set_track_solo" => McpCommand::SetTrackSolo {
            track_id: args["track_id"].as_str().unwrap_or("").to_string(),
            solo: args["solo"].as_bool().unwrap_or(false),
            reply: tx,
        },
        "set_track_height_preset" => McpCommand::SetTrackHeightPreset {
            track_id: args["track_id"].as_str().unwrap_or("").to_string(),
            height_preset: args["height_preset"]
                .as_str()
                .unwrap_or("medium")
                .to_string(),
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
        "set_source_playback_priority" => McpCommand::SetSourcePlaybackPriority {
            priority: args["priority"].as_str().unwrap_or("smooth").to_string(),
            reply: tx,
        },
        "set_crossfade_settings" => {
            let (enabled, curve, duration_ns) = match parse_crossfade_settings_args(&args) {
                Ok(parsed) => parsed,
                Err(message) => return Err(tool_error_payload(-32602, message)),
            };
            McpCommand::SetCrossfadeSettings {
                enabled,
                curve: curve.to_string(),
                duration_ns,
                reply: tx,
            }
        }

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
        "link_clips" => McpCommand::LinkClips {
            clip_ids: args["clip_ids"]
                .as_array()
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| id.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            reply: tx,
        },
        "unlink_clips" => McpCommand::UnlinkClips {
            clip_ids: args["clip_ids"]
                .as_array()
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| id.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            reply: tx,
        },
        "align_grouped_clips_by_timecode" => McpCommand::AlignGroupedClipsByTimecode {
            clip_ids: args["clip_ids"]
                .as_array()
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| id.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
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
            temperature: args["temperature"].as_f64().unwrap_or(6500.0),
            tint: args["tint"].as_f64().unwrap_or(0.0),
            denoise: args["denoise"].as_f64().unwrap_or(0.0),
            sharpness: args["sharpness"].as_f64().unwrap_or(0.0),
            shadows: args["shadows"].as_f64().unwrap_or(0.0),
            midtones: args["midtones"].as_f64().unwrap_or(0.0),
            highlights: args["highlights"].as_f64().unwrap_or(0.0),
            reply: tx,
        },
        "set_clip_color_label" => McpCommand::SetClipColorLabel {
            clip_id: args["clip_id"].as_str().unwrap_or("").to_string(),
            color_label: args["color_label"].as_str().unwrap_or("none").to_string(),
            reply: tx,
        },

        "set_clip_chroma_key" => McpCommand::SetClipChromaKey {
            clip_id: args["clip_id"].as_str().unwrap_or("").to_string(),
            enabled: args.get("enabled").and_then(|v| v.as_bool()),
            color: args.get("color").and_then(|v| v.as_u64()).map(|v| v as u32),
            tolerance: args.get("tolerance").and_then(|v| v.as_f64()),
            softness: args.get("softness").and_then(|v| v.as_f64()),
            reply: tx,
        },

        "set_clip_bg_removal" => McpCommand::SetClipBgRemoval {
            clip_id: args["clip_id"].as_str().unwrap_or("").to_string(),
            enabled: args.get("enabled").and_then(|v| v.as_bool()),
            threshold: args.get("threshold").and_then(|v| v.as_f64()),
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

        "list_export_presets" => McpCommand::ListExportPresets { reply: tx },

        "save_export_preset" => McpCommand::SaveExportPreset {
            name: args["name"].as_str().unwrap_or("").to_string(),
            video_codec: args["video_codec"].as_str().unwrap_or("h264").to_string(),
            container: args["container"].as_str().unwrap_or("mp4").to_string(),
            output_width: args["output_width"].as_u64().unwrap_or(0) as u32,
            output_height: args["output_height"].as_u64().unwrap_or(0) as u32,
            crf: args["crf"].as_u64().unwrap_or(23) as u32,
            audio_codec: args["audio_codec"].as_str().unwrap_or("aac").to_string(),
            audio_bitrate_kbps: args["audio_bitrate_kbps"].as_u64().unwrap_or(192) as u32,
            reply: tx,
        },

        "delete_export_preset" => McpCommand::DeleteExportPreset {
            name: args["name"].as_str().unwrap_or("").to_string(),
            reply: tx,
        },

        "export_with_preset" => McpCommand::ExportWithPreset {
            path: args["path"].as_str().unwrap_or("").to_string(),
            preset_name: args["preset_name"].as_str().unwrap_or("").to_string(),
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
            rotate: args["rotate"].as_i64().map(|v| v as i32),
            reply: tx,
        },
        "set_clip_opacity" => McpCommand::SetClipOpacity {
            clip_id: args["clip_id"].as_str().unwrap_or("").to_string(),
            opacity: args["opacity"].as_f64().unwrap_or(1.0),
            reply: tx,
        },
        "set_clip_keyframe" => McpCommand::SetClipKeyframe {
            clip_id: args["clip_id"].as_str().unwrap_or("").to_string(),
            property: args["property"].as_str().unwrap_or("").to_string(),
            timeline_pos_ns: args.get("timeline_pos_ns").and_then(|v| v.as_u64()),
            value: args["value"].as_f64().unwrap_or(0.0),
            interpolation: args
                .get("interpolation")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            reply: tx,
        },
        "remove_clip_keyframe" => McpCommand::RemoveClipKeyframe {
            clip_id: args["clip_id"].as_str().unwrap_or("").to_string(),
            property: args["property"].as_str().unwrap_or("").to_string(),
            timeline_pos_ns: args.get("timeline_pos_ns").and_then(|v| v.as_u64()),
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

        "set_realtime_preview" => McpCommand::SetRealtimePreview {
            enabled: args["enabled"].as_bool().unwrap_or(false),
            reply: tx,
        },

        "set_experimental_preview_optimizations" => {
            McpCommand::SetExperimentalPreviewOptimizations {
                enabled: args["enabled"].as_bool().unwrap_or(false),
                reply: tx,
            }
        }

        "set_background_prerender" => McpCommand::SetBackgroundPrerender {
            enabled: args["enabled"].as_bool().unwrap_or(false),
            reply: tx,
        },
        "set_preview_luts" => McpCommand::SetPreviewLuts {
            enabled: args["enabled"].as_bool().unwrap_or(false),
            reply: tx,
        },

        "insert_clip" => McpCommand::InsertClip {
            source_path: args["source_path"].as_str().unwrap_or("").to_string(),
            source_in_ns: args["source_in_ns"].as_u64().unwrap_or(0),
            source_out_ns: args["source_out_ns"].as_u64().unwrap_or(0),
            track_index: args["track_index"].as_u64().map(|v| v as usize),
            timeline_pos_ns: args["timeline_pos_ns"].as_u64(),
            reply: tx,
        },

        "overwrite_clip" => McpCommand::OverwriteClip {
            source_path: args["source_path"].as_str().unwrap_or("").to_string(),
            source_in_ns: args["source_in_ns"].as_u64().unwrap_or(0),
            source_out_ns: args["source_out_ns"].as_u64().unwrap_or(0),
            track_index: args["track_index"].as_u64().map(|v| v as usize),
            timeline_pos_ns: args["timeline_pos_ns"].as_u64(),
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
        "export_timeline_snapshot" => McpCommand::ExportTimelineSnapshot {
            path: args["path"].as_str().unwrap_or("").to_string(),
            width: args["width"].as_u64().unwrap_or(1920) as u32,
            height: args["height"].as_u64().unwrap_or(1080) as u32,
            reply: tx,
        },
        "take_screenshot" => McpCommand::TakeScreenshot { reply: tx },
        "select_library_item" => McpCommand::SelectLibraryItem {
            path: args["path"].as_str().unwrap_or("").to_string(),
            reply: tx,
        },
        "source_play" => McpCommand::SourcePlay { reply: tx },
        "source_pause" => McpCommand::SourcePause { reply: tx },
        "sync_clips_by_audio" => McpCommand::SyncClipsByAudio {
            clip_ids: args["clip_ids"]
                .as_array()
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| id.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            reply: tx,
        },

        _ => {
            return Err(tool_error_payload(
                -32602,
                format!("Unknown tool: '{name}'"),
            ))
        }
    };

    if sender.send(cmd).is_err() {
        return Err(tool_error_payload(-32603, "App main thread unavailable"));
    }

    match rx.recv() {
        Ok(result) => Ok(result),
        Err(_) => Err(tool_error_payload(-32603, "No reply from app")),
    }
}

fn call_tool(
    id: &Value,
    params: &Value,
    sender: &std::sync::mpsc::Sender<McpCommand>,
    session_read_cache: &mut std::collections::HashMap<String, (Value, std::time::Instant)>,
) -> Value {
    let name = params["name"].as_str().unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    if name == "batch_call_tools" {
        let Some(calls) = args.get("calls").and_then(Value::as_array) else {
            return err(id.clone(), -32602, "calls must be an array");
        };
        let stop_on_error = args["stop_on_error"].as_bool().unwrap_or(false);
        let include_timing = args["include_timing"].as_bool().unwrap_or(false);
        let batch_started = std::time::Instant::now();
        let mut results = Vec::with_capacity(calls.len());
        let mut stopped_on_error = false;
        let mut read_cache: std::collections::HashMap<String, Result<Value, Value>> =
            std::collections::HashMap::new();
        for (index, call) in calls.iter().enumerate() {
            let call_started = std::time::Instant::now();
            let tool_name = call.get("name").and_then(Value::as_str).unwrap_or("");
            if tool_name.is_empty() {
                let mut entry = json!({
                    "index": index,
                    "success": false,
                    "error": {"code": -32602, "message": "call.name must be a non-empty string"}
                });
                if include_timing {
                    if let Some(obj) = entry.as_object_mut() {
                        obj.insert(
                            "elapsed_ms".to_string(),
                            json!(call_started.elapsed().as_secs_f64() * 1000.0),
                        );
                    }
                }
                results.push(entry);
                if stop_on_error {
                    stopped_on_error = true;
                    break;
                }
                continue;
            }
            if tool_name == "batch_call_tools" {
                let mut entry = json!({
                    "index": index,
                    "name": tool_name,
                    "success": false,
                    "error": {"code": -32602, "message": "Nested batch_call_tools is not supported"}
                });
                if include_timing {
                    if let Some(obj) = entry.as_object_mut() {
                        obj.insert(
                            "elapsed_ms".to_string(),
                            json!(call_started.elapsed().as_secs_f64() * 1000.0),
                        );
                    }
                }
                results.push(entry);
                if stop_on_error {
                    stopped_on_error = true;
                    break;
                }
                continue;
            }
            let tool_args = call.get("arguments").cloned().unwrap_or(json!({}));
            let tool_is_read = is_cacheable_read_tool(tool_name);
            let tool_is_session_cacheable = is_session_cacheable_read_tool(tool_name);
            if !tool_is_read {
                read_cache.clear();
                session_read_cache.clear();
            }
            let dispatch_result = if tool_is_read {
                let key = cache_key(tool_name, &tool_args);
                if let Some(cached) = read_cache.get(&key) {
                    cached.clone()
                } else if tool_is_session_cacheable {
                    if let Some((cached_payload, cached_at)) = session_read_cache.get(&key) {
                        if call_started.duration_since(*cached_at)
                            <= std::time::Duration::from_millis(SESSION_READ_CACHE_TTL_MS)
                        {
                            let cached_result = Ok(cached_payload.clone());
                            read_cache.insert(key, cached_result.clone());
                            cached_result
                        } else {
                            let dispatched = dispatch_tool_payload(tool_name, &tool_args, sender);
                            if let Ok(result_payload) = &dispatched {
                                session_read_cache
                                    .insert(key.clone(), (result_payload.clone(), call_started));
                            }
                            read_cache.insert(key, dispatched.clone());
                            dispatched
                        }
                    } else {
                        let dispatched = dispatch_tool_payload(tool_name, &tool_args, sender);
                        if let Ok(result_payload) = &dispatched {
                            session_read_cache
                                .insert(key.clone(), (result_payload.clone(), call_started));
                        }
                        read_cache.insert(key, dispatched.clone());
                        dispatched
                    }
                } else {
                    let dispatched = dispatch_tool_payload(tool_name, &tool_args, sender);
                    read_cache.insert(key, dispatched.clone());
                    dispatched
                }
            } else {
                dispatch_tool_payload(tool_name, &tool_args, sender)
            };
            match dispatch_result {
                Ok(result_payload) => {
                    let mut entry = json!({
                        "index": index,
                        "name": tool_name,
                        "success": true,
                        "result": result_payload
                    });
                    if include_timing {
                        if let Some(obj) = entry.as_object_mut() {
                            obj.insert(
                                "elapsed_ms".to_string(),
                                json!(call_started.elapsed().as_secs_f64() * 1000.0),
                            );
                        }
                    }
                    results.push(entry);
                }
                Err(error_payload) => {
                    let mut entry = json!({
                        "index": index,
                        "name": tool_name,
                        "success": false,
                        "error": error_payload
                    });
                    if include_timing {
                        if let Some(obj) = entry.as_object_mut() {
                            obj.insert(
                                "elapsed_ms".to_string(),
                                json!(call_started.elapsed().as_secs_f64() * 1000.0),
                            );
                        }
                    }
                    results.push(entry);
                }
            }
            if stop_on_error
                && results
                    .last()
                    .is_some_and(|entry| !entry["success"].as_bool().unwrap_or(false))
            {
                stopped_on_error = true;
                break;
            }
        }
        let mut payload = json!({
            "results": results,
            "stopped_on_error": stopped_on_error
        });
        if include_timing {
            if let Some(obj) = payload.as_object_mut() {
                obj.insert(
                    "total_elapsed_ms".to_string(),
                    json!(batch_started.elapsed().as_secs_f64() * 1000.0),
                );
            }
        }
        return ok(id, text_content(payload));
    }

    let now = std::time::Instant::now();
    let session_cacheable = is_session_cacheable_read_tool(name);
    let session_key = cache_key(name, &args);
    if session_cacheable {
        if let Some((cached, cached_at)) = session_read_cache.get(&session_key) {
            if now.duration_since(*cached_at)
                <= std::time::Duration::from_millis(SESSION_READ_CACHE_TTL_MS)
            {
                return ok(id, text_content(cached.clone()));
            }
        }
    } else {
        session_read_cache.clear();
    }

    match dispatch_tool_payload(name, &args, sender) {
        Ok(result_payload) => {
            if session_cacheable {
                session_read_cache.insert(session_key, (result_payload.clone(), now));
            }
            ok(id, text_content(result_payload))
        }
        Err(error_payload) => {
            let code = error_payload
                .get("code")
                .and_then(Value::as_i64)
                .unwrap_or(-32603) as i32;
            let message = error_payload
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Tool call failed");
            err(id.clone(), code, message)
        }
    }
}

fn parse_crossfade_settings_args(args: &Value) -> Result<(bool, &'static str, u64), &'static str> {
    let enabled = match args.get("enabled").and_then(Value::as_bool) {
        Some(enabled) => enabled,
        None => return Err("enabled must be a boolean"),
    };
    let curve = match args.get("curve").and_then(Value::as_str) {
        Some("equal_power") => "equal_power",
        Some("linear") => "linear",
        Some(_) => return Err("curve must be one of: equal_power, linear"),
        None => return Err("curve must be a string"),
    };
    let duration_ns = match args.get("duration_ns").and_then(Value::as_u64) {
        Some(duration_ns) => duration_ns,
        None => return Err("duration_ns must be an integer"),
    };
    if !(10_000_000..=10_000_000_000).contains(&duration_ns) {
        return Err("duration_ns must be between 10_000_000 and 10_000_000_000");
    }
    Ok((enabled, curve, duration_ns))
}

#[cfg(test)]
mod tests {
    use super::{call_tool, parse_crossfade_settings_args};
    use crate::mcp::McpCommand;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn parse_crossfade_settings_accepts_valid_bounds() {
        let min = json!({"enabled": true, "curve": "linear", "duration_ns": 10_000_000u64});
        let max =
            json!({"enabled": false, "curve": "equal_power", "duration_ns": 10_000_000_000u64});
        assert_eq!(
            parse_crossfade_settings_args(&min),
            Ok((true, "linear", 10_000_000))
        );
        assert_eq!(
            parse_crossfade_settings_args(&max),
            Ok((false, "equal_power", 10_000_000_000))
        );
    }

    #[test]
    fn parse_crossfade_settings_rejects_invalid_curve() {
        let args = json!({"enabled": true, "curve": "log", "duration_ns": 200_000_000u64});
        assert_eq!(
            parse_crossfade_settings_args(&args),
            Err("curve must be one of: equal_power, linear")
        );
    }

    #[test]
    fn parse_crossfade_settings_rejects_duration_out_of_bounds() {
        let too_small = json!({"enabled": true, "curve": "linear", "duration_ns": 9_999_999u64});
        let too_large =
            json!({"enabled": true, "curve": "linear", "duration_ns": 10_000_000_001u64});
        assert_eq!(
            parse_crossfade_settings_args(&too_small),
            Err("duration_ns must be between 10_000_000 and 10_000_000_000")
        );
        assert_eq!(
            parse_crossfade_settings_args(&too_large),
            Err("duration_ns must be between 10_000_000 and 10_000_000_000")
        );
    }

    #[test]
    fn parse_crossfade_settings_rejects_non_integer_duration() {
        let args = json!({"enabled": true, "curve": "linear", "duration_ns": 2.5});
        assert_eq!(
            parse_crossfade_settings_args(&args),
            Err("duration_ns must be an integer")
        );
    }

    #[test]
    fn call_tool_dispatches_set_track_solo() {
        let (sender, receiver) = std::sync::mpsc::channel::<McpCommand>();
        std::thread::spawn(move || {
            let cmd = receiver.recv().expect("expected command");
            match cmd {
                McpCommand::SetTrackSolo {
                    track_id,
                    solo,
                    reply,
                } => {
                    assert_eq!(track_id, "track-1");
                    assert!(solo);
                    reply
                        .send(json!({"success": true, "track_id": track_id, "soloed": solo}))
                        .ok();
                }
                _ => panic!("unexpected MCP command"),
            }
        });
        let id = json!(7);
        let params = json!({
            "name": "set_track_solo",
            "arguments": { "track_id": "track-1", "solo": true }
        });
        let mut cache = std::collections::HashMap::new();
        let response = call_tool(&id, &params, &sender, &mut cache);
        assert_eq!(response["id"], id);
        assert_eq!(response["error"], serde_json::Value::Null);
    }

    #[test]
    fn batch_call_tools_returns_per_call_results() {
        let (sender, receiver) = std::sync::mpsc::channel::<McpCommand>();
        std::thread::spawn(move || {
            let first = receiver.recv().expect("expected first command");
            match first {
                McpCommand::GetTimelineSettings { reply } => {
                    reply.send(json!({"magnetic_mode": false})).ok();
                }
                _ => panic!("unexpected first MCP command"),
            }
            let second = receiver.recv().expect("expected second command");
            match second {
                McpCommand::GetPlayheadPosition { reply } => {
                    reply.send(json!({"timeline_pos_ns": 1234u64})).ok();
                }
                _ => panic!("unexpected second MCP command"),
            }
        });
        let id = json!(88);
        let params = json!({
            "name": "batch_call_tools",
            "arguments": {
                "include_timing": true,
                "calls": [
                    {"name": "get_timeline_settings", "arguments": {}},
                    {"name": "get_playhead_position", "arguments": {}}
                ]
            }
        });
        let mut cache = std::collections::HashMap::new();
        let response = call_tool(&id, &params, &sender, &mut cache);
        assert_eq!(response["error"], serde_json::Value::Null);
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("batch content text");
        let payload: serde_json::Value = serde_json::from_str(text).expect("parse batch payload");
        let results = payload["results"].as_array().expect("batch results array");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["success"], true);
        assert_eq!(results[1]["success"], true);
        assert!(results[0]["elapsed_ms"].as_f64().is_some());
        assert!(payload["total_elapsed_ms"].as_f64().is_some());
    }

    #[test]
    fn batch_call_tools_stop_on_error_halts_remaining_calls() {
        let (sender, receiver) = std::sync::mpsc::channel::<McpCommand>();
        std::thread::spawn(move || {
            let first = receiver.recv().expect("expected first command");
            match first {
                McpCommand::GetTimelineSettings { reply } => {
                    reply.send(json!({"magnetic_mode": true})).ok();
                }
                _ => panic!("unexpected first MCP command"),
            }
            // No second recv expected: unknown tool should stop execution when stop_on_error=true.
        });
        let id = json!(99);
        let params = json!({
            "name": "batch_call_tools",
            "arguments": {
                "stop_on_error": true,
                "calls": [
                    {"name": "get_timeline_settings", "arguments": {}},
                    {"name": "not_a_real_tool", "arguments": {}},
                    {"name": "get_playhead_position", "arguments": {}}
                ]
            }
        });
        let mut cache = std::collections::HashMap::new();
        let response = call_tool(&id, &params, &sender, &mut cache);
        assert_eq!(response["error"], serde_json::Value::Null);
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("batch content text");
        let payload: serde_json::Value = serde_json::from_str(text).expect("parse batch payload");
        let results = payload["results"].as_array().expect("batch results array");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["success"], true);
        assert_eq!(results[1]["success"], false);
        assert_eq!(payload["stopped_on_error"], true);
    }

    #[test]
    fn batch_call_tools_reuses_cached_read_results_until_mutation() {
        let (sender, receiver) = std::sync::mpsc::channel::<McpCommand>();
        let command_count = Arc::new(AtomicUsize::new(0));
        let command_count_thread = command_count.clone();
        let worker = std::thread::spawn(move || {
            while let Ok(cmd) = receiver.recv_timeout(std::time::Duration::from_millis(200)) {
                command_count_thread.fetch_add(1, Ordering::Relaxed);
                match cmd {
                    McpCommand::GetTimelineSettings { reply } => {
                        reply.send(json!({"magnetic_mode": false})).ok();
                    }
                    McpCommand::SetMagneticMode { enabled, reply } => {
                        reply
                            .send(json!({"success": true, "magnetic_mode": enabled}))
                            .ok();
                    }
                    _ => panic!("unexpected MCP command"),
                }
            }
        });
        let id = json!(123);
        let params = json!({
            "name": "batch_call_tools",
            "arguments": {
                "calls": [
                    {"name": "get_timeline_settings", "arguments": {}},
                    {"name": "get_timeline_settings", "arguments": {}},
                    {"name": "set_magnetic_mode", "arguments": {"enabled": true}},
                    {"name": "get_timeline_settings", "arguments": {}}
                ]
            }
        });
        let mut cache = std::collections::HashMap::new();
        let response = call_tool(&id, &params, &sender, &mut cache);
        assert_eq!(response["error"], serde_json::Value::Null);
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("batch content text");
        let payload: serde_json::Value = serde_json::from_str(text).expect("parse batch payload");
        let results = payload["results"].as_array().expect("batch results array");
        assert_eq!(results.len(), 4);
        assert_eq!(results[0]["success"], true);
        assert_eq!(results[1]["success"], true);
        assert_eq!(results[2]["success"], true);
        assert_eq!(results[3]["success"], true);
        drop(sender);
        worker.join().expect("worker join");
        assert_eq!(command_count.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn top_level_read_cache_hits_and_invalidates_on_mutation() {
        let (sender, receiver) = std::sync::mpsc::channel::<McpCommand>();
        let command_count = Arc::new(AtomicUsize::new(0));
        let command_count_thread = command_count.clone();
        let worker = std::thread::spawn(move || {
            while let Ok(cmd) = receiver.recv_timeout(std::time::Duration::from_millis(200)) {
                command_count_thread.fetch_add(1, Ordering::Relaxed);
                match cmd {
                    McpCommand::ListTracks { compact, reply } => {
                        reply.send(json!([{"index": 0, "compact": compact}])).ok();
                    }
                    McpCommand::SetMagneticMode { enabled, reply } => {
                        reply
                            .send(json!({"success": true, "magnetic_mode": enabled}))
                            .ok();
                    }
                    _ => panic!("unexpected MCP command"),
                }
            }
        });

        let mut cache = std::collections::HashMap::new();
        let read_id_1 = json!(201);
        let read_params = json!({"name":"list_tracks","arguments":{"compact":true}});
        let _ = call_tool(&read_id_1, &read_params, &sender, &mut cache);
        // Second identical read should hit cache and avoid dispatch.
        let read_id_2 = json!(202);
        let _ = call_tool(&read_id_2, &read_params, &sender, &mut cache);
        // Mutation clears top-level cache.
        let mut_id = json!(203);
        let mut_params = json!({"name":"set_magnetic_mode","arguments":{"enabled":true}});
        let _ = call_tool(&mut_id, &mut_params, &sender, &mut cache);
        // Read after mutation should dispatch again.
        let read_id_3 = json!(204);
        let _ = call_tool(&read_id_3, &read_params, &sender, &mut cache);

        drop(sender);
        worker.join().expect("worker join");
        assert_eq!(command_count.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn batch_call_tools_reuses_session_cache_then_invalidates_on_mutation() {
        let (sender, receiver) = std::sync::mpsc::channel::<McpCommand>();
        let command_count = Arc::new(AtomicUsize::new(0));
        let command_count_thread = command_count.clone();
        let worker = std::thread::spawn(move || {
            while let Ok(cmd) = receiver.recv_timeout(std::time::Duration::from_millis(200)) {
                command_count_thread.fetch_add(1, Ordering::Relaxed);
                match cmd {
                    McpCommand::ListTracks { compact, reply } => {
                        reply.send(json!([{"index": 0, "compact": compact}])).ok();
                    }
                    McpCommand::SetMagneticMode { enabled, reply } => {
                        reply
                            .send(json!({"success": true, "magnetic_mode": enabled}))
                            .ok();
                    }
                    _ => panic!("unexpected MCP command"),
                }
            }
        });

        let mut cache = std::collections::HashMap::new();
        let single_read_id = json!(301);
        let single_read_params = json!({"name":"list_tracks","arguments":{"compact":true}});
        let _ = call_tool(&single_read_id, &single_read_params, &sender, &mut cache);

        let batch_id = json!(302);
        let batch_params = json!({
            "name": "batch_call_tools",
            "arguments": {
                "calls": [
                    {"name":"list_tracks","arguments":{"compact":true}},
                    {"name":"set_magnetic_mode","arguments":{"enabled":true}},
                    {"name":"list_tracks","arguments":{"compact":true}}
                ]
            }
        });
        let response = call_tool(&batch_id, &batch_params, &sender, &mut cache);
        assert_eq!(response["error"], serde_json::Value::Null);
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("batch content text");
        let payload: serde_json::Value = serde_json::from_str(text).expect("parse batch payload");
        let results = payload["results"].as_array().expect("batch results array");
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|entry| entry["success"] == true));

        drop(sender);
        worker.join().expect("worker join");
        // One dispatch for initial read, one for mutation, one for read after mutation.
        assert_eq!(command_count.load(Ordering::Relaxed), 3);
    }
}
