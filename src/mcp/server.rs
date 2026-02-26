/// MCP stdio transport — reads newline-delimited JSON-RPC from stdin,
/// dispatches tool calls to the GTK main thread via `sender`, and
/// writes JSON-RPC responses to stdout.
use std::io::{BufRead, Write};
use serde_json::{json, Value};
use crate::mcp::McpCommand;

const PROTOCOL_VERSION: &str = "2024-11-05";

pub fn run_stdio_server(sender: std::sync::mpsc::Sender<McpCommand>) {
    let stdin  = std::io::stdin();
    let mut out = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        let line = line.trim().to_owned();
        if line.is_empty() { continue; }

        let msg: Value = match serde_json::from_str(&line) {
            Ok(v)  => v,
            Err(e) => {
                let r = err(Value::Null, -32700, &format!("Parse error: {e}"));
                let _ = writeln!(out, "{r}");
                let _ = out.flush();
                continue;
            }
        };

        // MCP notifications carry no "id" — do not respond.
        let id = match msg.get("id") {
            Some(id) => id.clone(),
            None     => continue,
        };

        let method = msg["method"].as_str().unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));

        let response = match method {
            "initialize"     => ok(&id, initialize_result()),
            "ping"           => ok(&id, json!({})),
            "tools/list"     => ok(&id, tools_list()),
            "resources/list" => ok(&id, json!({"resources": []})),
            "tools/call"     => call_tool(&id, &params, &sender),
            _                => err(id, -32601, "Method not found"),
        };

        let _ = writeln!(out, "{response}");
        let _ = out.flush();
    }
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
                    "sharpness":  { "type": "number",  "description": "Sharpness: -1.0 (soften) to 1.0 (sharpen). Default 0.0." }
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
        "list_tracks" => McpCommand::ListTracks  { reply: tx },
        "list_clips"  => McpCommand::ListClips   { reply: tx },
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
            source_path:       args["source_path"].as_str().unwrap_or("").to_string(),
            track_index:       args["track_index"].as_u64().unwrap_or(0) as usize,
            timeline_start_ns: args["timeline_start_ns"].as_u64().unwrap_or(0),
            source_in_ns:      args["source_in_ns"].as_u64().unwrap_or(0),
            source_out_ns:     args["source_out_ns"].as_u64().unwrap_or(0),
            reply: tx,
        },

        "remove_clip" => McpCommand::RemoveClip {
            clip_id: args["clip_id"].as_str().unwrap_or("").to_string(),
            reply: tx,
        },

        "move_clip" => McpCommand::MoveClip {
            clip_id:      args["clip_id"].as_str().unwrap_or("").to_string(),
            new_start_ns: args["new_start_ns"].as_u64().unwrap_or(0),
            reply: tx,
        },

        "trim_clip" => McpCommand::TrimClip {
            clip_id:       args["clip_id"].as_str().unwrap_or("").to_string(),
            source_in_ns:  args["source_in_ns"].as_u64().unwrap_or(0),
            source_out_ns: args["source_out_ns"].as_u64().unwrap_or(0),
            reply: tx,
        },

        "set_clip_color" => McpCommand::SetClipColor {
            clip_id:    args["clip_id"].as_str().unwrap_or("").to_string(),
            brightness: args["brightness"].as_f64().unwrap_or(0.0),
            contrast:   args["contrast"].as_f64().unwrap_or(1.0),
            saturation: args["saturation"].as_f64().unwrap_or(1.0),
            denoise:    args["denoise"].as_f64().unwrap_or(0.0),
            sharpness:  args["sharpness"].as_f64().unwrap_or(0.0),
            reply: tx,
        },

        "set_project_title" => McpCommand::SetTitle {
            title: args["title"].as_str().unwrap_or("").to_string(),
            reply: tx,
        },

        "save_fcpxml" => McpCommand::SaveFcpxml {
            path:  args["path"].as_str().unwrap_or("").to_string(),
            reply: tx,
        },

        "open_fcpxml" => McpCommand::OpenFcpxml {
            path:  args["path"].as_str().unwrap_or("").to_string(),
            reply: tx,
        },

        "export_mp4" => McpCommand::ExportMp4 {
            path:  args["path"].as_str().unwrap_or("").to_string(),
            reply: tx,
        },

        "list_library" => McpCommand::ListLibrary { reply: tx },

        "import_media" => McpCommand::ImportMedia {
            path:  args["path"].as_str().unwrap_or("").to_string(),
            reply: tx,
        },

        "reorder_track" => McpCommand::ReorderTrack {
            from_index: args["from_index"].as_u64().unwrap_or(0) as usize,
            to_index:   args["to_index"].as_u64().unwrap_or(0) as usize,
            reply: tx,
        },
        "set_transition" => McpCommand::SetTransition {
            track_index: args["track_index"].as_u64().unwrap_or(0) as usize,
            clip_index:  args["clip_index"].as_u64().unwrap_or(0) as usize,
            kind:        args["kind"].as_str().unwrap_or("").to_string(),
            duration_ns: args["duration_ns"].as_u64().unwrap_or(0),
            reply: tx,
        },

        _ => return err(id.clone(), -32602, &format!("Unknown tool: '{name}'")),
    };

    if sender.send(cmd).is_err() {
        return err(id.clone(), -32603, "App main thread unavailable");
    }

    match rx.recv() {
        Ok(result) => ok(id, text_content(result)),
        Err(_)     => err(id.clone(), -32603, "No reply from app"),
    }
}
