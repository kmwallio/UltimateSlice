use crate::mcp::McpCommand;
use crate::model::project::FrameRate;
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
            log::error!("Failed to bind MCP socket {}: {e}", path.display());
            return;
        }
    };
    listener.set_nonblocking(true).ok();
    log::info!("MCP socket listening on {}", path.display());

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
                log::info!("MCP socket client connected");
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
                    log::info!("MCP socket client disconnected");
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => {
                log::error!("MCP socket accept error: {e}");
                break;
            }
        }
    }

    let _ = std::fs::remove_file(&path);
    log::info!("MCP socket server stopped");
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
            "name": "get_performance_snapshot",
            "description": "Return compact Program Monitor performance counters for tuning: prerender queue state, rebuild telemetry, and transition prerender hit/miss metrics.",
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
            "name": "list_ladspa_plugins",
            "description": "List all available LADSPA audio effect plugins with their parameters.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "add_clip_ladspa_effect",
            "description": "Add a LADSPA audio effect to a clip by plugin name.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id." },
                    "plugin_name": { "type": "string", "description": "LADSPA plugin short name from list_ladspa_plugins." }
                },
                "required": ["clip_id", "plugin_name"]
            }
        },
        {
            "name": "remove_clip_ladspa_effect",
            "description": "Remove a LADSPA audio effect from a clip by effect id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id." },
                    "effect_id": { "type": "string", "description": "Effect instance id." }
                },
                "required": ["clip_id", "effect_id"]
            }
        },
        {
            "name": "set_clip_ladspa_effect_params",
            "description": "Set parameters on a LADSPA audio effect instance.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id." },
                    "effect_id": { "type": "string", "description": "Effect instance id." },
                    "params": { "type": "object", "description": "Parameter name → value pairs." }
                },
                "required": ["clip_id", "effect_id", "params"]
            }
        },
        {
            "name": "set_track_role",
            "description": "Set audio role for a track. Roles categorize audio for submix routing and FCPXML metadata.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "track_id": { "type": "string", "description": "Target track id from list_tracks." },
                    "role": { "type": "string", "enum": ["none", "dialogue", "effects", "music"], "description": "Audio role for the track." }
                },
                "required": ["track_id", "role"]
            }
        },
        {
            "name": "set_track_duck",
            "description": "Enable or disable automatic ducking on a track. When enabled, the track's volume is reduced when dialogue (video-embedded audio or non-ducked audio tracks) is present.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "track_id": { "type": "string", "description": "Target track id from list_tracks." },
                    "duck": { "type": "boolean", "description": "Whether the track should be ducked when dialogue is present." }
                },
                "required": ["track_id", "duck"]
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
            "name": "convert_ltc_audio_to_timecode",
            "description": "Decode LTC from a clip's audio and store the result as source timecode metadata. When LTC lives on one stereo side, the opposite side is routed to both speakers; mono LTC clips are muted.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": {
                        "type": "string",
                        "description": "Clip id whose source audio should be decoded for LTC."
                    },
                    "ltc_channel": {
                        "type": "string",
                        "description": "Which audio channel carries LTC: auto, left, right, or mono_mix.",
                        "enum": ["auto", "left", "right", "mono_mix"]
                    },
                    "frame_rate": {
                        "type": "string",
                        "description": "Optional LTC frame rate override: 23.976, 24, 25, 29.97, 30, or a fraction like 24000/1001."
                    }
                },
                "required": ["clip_id"]
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
            "name": "set_clip_speed",
            "description": "Set per-clip playback speed and (optionally) the slow-motion frame interpolation mode. When slow_motion_interp is 'ai' the AI frame-interpolation cache (RIFE) precomputes a higher-fps sidecar in the background; preview and export consume the same sidecar once it is ready, guaranteeing they match.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id." },
                    "speed":   { "type": "number", "description": "Playback multiplier. <1 = slow-motion, >1 = fast-forward, 1.0 = normal. Range 0.05–16.0." },
                    "slow_motion_interp": {
                        "type": "string",
                        "enum": ["off", "blend", "optical-flow", "ai"],
                        "description": "Interpolation mode for slow-motion clips. 'off' disables interpolation; 'blend' uses ffmpeg minterpolate blend; 'optical-flow' uses ffmpeg motion-compensation; 'ai' precomputes a learned RIFE sidecar."
                    }
                },
                "required": ["clip_id", "speed"]
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
            "description": "Set color correction and denoise/sharpness/blur effects for a clip by id.",
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
                    "blur":       { "type": "number",  "description": "Creative blur strength: 0.0 (off) to 1.0 (heavy). Default 0.0." },
                    "shadows":    { "type": "number",  "description": "Shadow grading: -1.0 (crush) to 1.0 (lift). Default 0.0." },
                    "midtones":   { "type": "number",  "description": "Midtone grading: -1.0 (darken) to 1.0 (brighten). Default 0.0." },
                    "highlights": { "type": "number",  "description": "Highlight grading: -1.0 (pull down) to 1.0 (boost). Default 0.0." },
                    "exposure":   { "type": "number",  "description": "Exposure adjustment: -1.0 to 1.0. Default 0.0." },
                    "black_point":{ "type": "number",  "description": "Black point adjustment: -1.0 to 1.0. Default 0.0." },
                    "highlights_warmth": { "type": "number", "description": "Highlights warmth (orange-blue): -1.0 to 1.0. Default 0.0." },
                    "highlights_tint":   { "type": "number", "description": "Highlights tint (green-magenta): -1.0 to 1.0. Default 0.0." },
                    "midtones_warmth":   { "type": "number", "description": "Midtones warmth (orange-blue): -1.0 to 1.0. Default 0.0." },
                    "midtones_tint":     { "type": "number", "description": "Midtones tint (green-magenta): -1.0 to 1.0. Default 0.0." },
                    "shadows_warmth":    { "type": "number", "description": "Shadows warmth (orange-blue): -1.0 to 1.0. Default 0.0." },
                    "shadows_tint":      { "type": "number", "description": "Shadows tint (green-magenta): -1.0 to 1.0. Default 0.0." }
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
            "name": "set_clip_hsl_qualifier",
            "description": "Set (or clear) an HSL Qualifier on a clip — secondary color correction that isolates pixels by hue/saturation/luminance range and applies a follow-up brightness/contrast/saturation grade only inside the matched region. Pass 'clear: true' to remove the qualifier entirely.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id":      { "type": "string",  "description": "Clip id (from list_clips)." },
                    "clear":        { "type": "boolean", "description": "When true, clear the qualifier entirely. Default false." },
                    "enabled":      { "type": "boolean", "description": "Whether the qualifier is active. Default true when first set." },
                    "hue_min":      { "type": "number",  "description": "Hue range minimum in degrees (0..360). Default 0." },
                    "hue_max":      { "type": "number",  "description": "Hue range maximum in degrees (0..360). When min > max, the range wraps around 360 (selects reds). Default 360." },
                    "hue_softness": { "type": "number",  "description": "Hue feather band in degrees (0..60). Default 0." },
                    "sat_min":      { "type": "number",  "description": "Saturation range minimum (0..1). Default 0." },
                    "sat_max":      { "type": "number",  "description": "Saturation range maximum (0..1). Default 1." },
                    "sat_softness": { "type": "number",  "description": "Saturation feather band (0..0.5). Default 0." },
                    "lum_min":      { "type": "number",  "description": "Luminance range minimum (0..1). Default 0." },
                    "lum_max":      { "type": "number",  "description": "Luminance range maximum (0..1). Default 1." },
                    "lum_softness": { "type": "number",  "description": "Luminance feather band (0..0.5). Default 0." },
                    "invert":       { "type": "boolean", "description": "Invert the matte. Default false." },
                    "brightness":   { "type": "number",  "description": "Secondary brightness delta applied inside the matte (-1..1). Default 0." },
                    "contrast":     { "type": "number",  "description": "Secondary contrast multiplier inside the matte (0..2). Default 1." },
                    "saturation":   { "type": "number",  "description": "Secondary saturation multiplier inside the matte (0..2). Default 1." }
                },
                "required": ["clip_id"]
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
            "name": "set_clip_motion_blur",
            "description": "Enable motion blur for a clip's keyframed transforms or fast-speed motion. Rendered at export only via FFmpeg minterpolate+tmix; auto-skipped on static (non-keyframed, speed≈1) clips.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id":       { "type": "string",  "description": "Clip id (from list_clips)." },
                    "enabled":       { "type": "boolean", "description": "Enable/disable motion blur." },
                    "shutter_angle": { "type": "number",  "description": "Shutter angle in degrees, 0..720. 180 = cinematic (default); 360 = full natural blur (cheap path)." }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "set_clip_mask",
            "description": "Set shape mask on a clip (rectangle, ellipse, or bezier path) to restrict visible area. Creates mask if absent.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id":   { "type": "string",  "description": "Clip id" },
                    "enabled":   { "type": "boolean", "description": "Enable/disable mask" },
                    "shape":     { "type": "string",  "enum": ["rectangle", "ellipse", "path"], "description": "Mask shape type" },
                    "center_x":  { "type": "number",  "description": "Mask center X (0.0-1.0, default 0.5)" },
                    "center_y":  { "type": "number",  "description": "Mask center Y (0.0-1.0, default 0.5)" },
                    "width":     { "type": "number",  "description": "Mask half-width (0.01-0.5, default 0.25)" },
                    "height":    { "type": "number",  "description": "Mask half-height (0.01-0.5, default 0.25)" },
                    "rotation":  { "type": "number",  "description": "Mask rotation in degrees (-180 to 180)" },
                    "feather":   { "type": "number",  "description": "Edge feather (0.0-0.5, default 0.0)" },
                    "expansion": { "type": "number",  "description": "Expand/contract mask (-0.5 to 0.5)" },
                    "invert":    { "type": "boolean", "description": "Invert mask (show outside, hide inside)" },
                    "path":      { "type": "array",   "description": "Bezier path points (for shape='path'). Each: {x, y, handle_in_x, handle_in_y, handle_out_x, handle_out_y}", "items": {"type": "object"} }
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
            "name": "save_edl",
            "description": "Export timeline to CMX 3600 EDL (.edl) file for color grading handoff and broadcast",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path for output .edl file" }
                },
                "required": ["path"]
            }
        },
        {
            "name": "save_otio",
            "description": "Export the current project to an OpenTimelineIO (.otio) JSON file for interchange with DaVinci Resolve, Premiere, Nuke, etc.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path for the output .otio file." },
                    "path_mode": {
                        "type": "string",
                        "enum": ["absolute", "relative"],
                        "description": "How media references are written inside the OTIO file. Defaults to absolute. Relative paths are resolved against the exported .otio file location."
                    }
                },
                "required": ["path"]
            }
        },
        {
            "name": "save_project_with_media",
            "description": "Export a packaged project: write .uspxml plus copy all timeline-used media into a sibling ProjectName.Library directory, with XML media paths rewritten to the packaged copies.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path for the packaged output .uspxml/.fcpxml file." }
                },
                "required": ["path"]
            }
        },
        {
            "name": "collect_project_files",
            "description": "Copy referenced project media into a destination directory for archival or transfer, without writing project XML. Supports timeline-used-only or entire-library collection modes and also copies clip LUT files that exist on disk.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "directory_path": { "type": "string", "description": "Absolute path of the destination directory for collected files." },
                    "mode": {
                        "type": "string",
                        "enum": ["timeline_used", "entire_library"],
                        "description": "Collection scope. Defaults to timeline_used."
                    },
                    "use_collected_locations_on_next_save": {
                        "type": "boolean",
                        "description": "When true, update the in-memory project to point at the collected media and LUT files so the next project save/export writes those collected paths."
                    }
                },
                "required": ["directory_path"]
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
            "name": "open_otio",
            "description": "Load a project from an OpenTimelineIO (.otio) JSON file, replacing the current project.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path to the .otio file to open." }
                },
                "required": ["path"]
            }
        },
        {
            "name": "export_mp4",
            "description": "Export the current project to MP4/H.264 at the given absolute path. Optional advanced audio mode picks a surround channel layout.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path for the output .mp4 file." },
                    "audio_channel_layout": {
                        "type": "string",
                        "enum": ["stereo", "surround_5_1", "surround_7_1"],
                        "description": "Output audio channel layout. Defaults to stereo. Surround uses role-based auto-routing with an LFE bass tap."
                    }
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
                    "audio_bitrate_kbps": { "type": "integer", "description": "Audio bitrate in kbps." },
                    "audio_channel_layout": {
                        "type": "string",
                        "enum": ["stereo", "surround_5_1", "surround_7_1"],
                        "description": "Output audio channel layout. Optional; defaults to stereo."
                    }
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
            "name": "list_workspace_layouts",
            "description": "List saved workspace layouts plus the current arrangement state from local UI state.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "save_workspace_layout",
            "description": "Create or overwrite a named workspace layout using the current window arrangement.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Workspace layout name." }
                },
                "required": ["name"]
            }
        },
        {
            "name": "apply_workspace_layout",
            "description": "Apply a saved named workspace layout to the current window.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Saved workspace layout name." }
                },
                "required": ["name"]
            }
        },
        {
            "name": "rename_workspace_layout",
            "description": "Rename a saved workspace layout.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "old_name": { "type": "string", "description": "Existing workspace layout name." },
                    "new_name": { "type": "string", "description": "New workspace layout name." }
                },
                "required": ["old_name", "new_name"]
            }
        },
        {
            "name": "delete_workspace_layout",
            "description": "Delete a saved workspace layout by name.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Saved workspace layout name." }
                },
                "required": ["name"]
            }
        },
        {
            "name": "reset_workspace_layout",
            "description": "Restore the built-in default workspace arrangement in the current window.",
            "inputSchema": { "type": "object", "properties": {} }
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
            "description": "List all items currently in the media library, including stable library keys plus resolved browser metadata such as duration, codec, resolution, frame rate, file size, rating, keyword ranges, and non-file clip kind/title text when available.",
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
            "name": "relink_media",
            "description": "Attempt to relink missing/offline media by recursively scanning a root folder and remapping missing source paths to matching files.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root_path": { "type": "string", "description": "Absolute folder path to scan for replacement media files." }
                },
                "required": ["root_path"]
            }
        },
        {
            "name": "create_bin",
            "description": "Create a media library bin (folder) for organizing media items",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Name for the new bin" },
                    "parent_id": { "type": "string", "description": "Parent bin ID for nesting (omit for root-level bin)" }
                },
                "required": ["name"]
            }
        },
        {
            "name": "delete_bin",
            "description": "Delete a media library bin; items and child bins are moved to the parent (or root)",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "bin_id": { "type": "string", "description": "ID of the bin to delete" }
                },
                "required": ["bin_id"]
            }
        },
        {
            "name": "rename_bin",
            "description": "Rename a media library bin",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "bin_id": { "type": "string", "description": "ID of the bin to rename" },
                    "name": { "type": "string", "description": "New name for the bin" }
                },
                "required": ["bin_id", "name"]
            }
        },
        {
            "name": "list_bins",
            "description": "List all media library bins with hierarchy and item counts",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "move_to_bin",
            "description": "Move media items to a bin (or root if bin_id is null/omitted)",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source_paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Source paths of media items to move"
                    },
                    "bin_id": { "type": "string", "description": "Target bin ID, or omit/null to move to root" }
                },
                "required": ["source_paths"]
            }
        },
        {
            "name": "list_collections",
            "description": "List saved smart collections and their metadata filter criteria.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "create_collection",
            "description": "Create a project-wide smart collection from saved media-browser filters.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Collection name." },
                    "search_text": { "type": "string", "description": "Optional text match against clip name, title text, path, or codec." },
                    "kind": { "type": "string", "enum": ["all", "video", "audio", "image", "offline"], "description": "Optional media kind filter." },
                    "resolution": { "type": "string", "enum": ["all", "sd", "hd", "fhd", "uhd"], "description": "Optional resolution bucket." },
                    "frame_rate": { "type": "string", "enum": ["all", "fps24", "fps25_30", "fps31_59", "fps60"], "description": "Optional frame-rate bucket." },
                    "rating": { "type": "string", "enum": ["all", "favorite", "reject", "unrated"], "description": "Optional rating filter." }
                },
                "required": ["name"]
            }
        },
        {
            "name": "update_collection",
            "description": "Rename a smart collection or replace any of its saved filter criteria.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "collection_id": { "type": "string", "description": "Collection id from list_collections." },
                    "name": { "type": "string", "description": "Optional new collection name." },
                    "search_text": { "type": "string", "description": "Optional replacement search text." },
                    "kind": { "type": "string", "enum": ["all", "video", "audio", "image", "offline"], "description": "Optional replacement media kind filter." },
                    "resolution": { "type": "string", "enum": ["all", "sd", "hd", "fhd", "uhd"], "description": "Optional replacement resolution bucket." },
                    "frame_rate": { "type": "string", "enum": ["all", "fps24", "fps25_30", "fps31_59", "fps60"], "description": "Optional replacement frame-rate bucket." },
                    "rating": { "type": "string", "enum": ["all", "favorite", "reject", "unrated"], "description": "Optional replacement rating filter." }
                },
                "required": ["collection_id"]
            }
        },
        {
            "name": "delete_collection",
            "description": "Delete a smart collection by id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "collection_id": { "type": "string", "description": "Collection id from list_collections." }
                },
                "required": ["collection_id"]
            }
        },
        {
            "name": "set_media_rating",
            "description": "Set a media-browser rating on a library item identified by library_key.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "library_key": { "type": "string", "description": "Stable media library key from list_library." },
                    "rating": { "type": "string", "enum": ["none", "favorite", "reject"], "description": "Rating to apply." }
                },
                "required": ["library_key", "rating"]
            }
        },
        {
            "name": "add_media_keyword_range",
            "description": "Add a named keyword range to a library item using source-relative nanosecond positions.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "library_key": { "type": "string", "description": "Stable media library key from list_library." },
                    "label": { "type": "string", "description": "Keyword label." },
                    "start_ns": { "type": "integer", "description": "Range start in nanoseconds." },
                    "end_ns": { "type": "integer", "description": "Range end in nanoseconds." }
                },
                "required": ["library_key", "label", "start_ns", "end_ns"]
            }
        },
        {
            "name": "update_media_keyword_range",
            "description": "Replace an existing keyword range's label and bounds.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "library_key": { "type": "string", "description": "Stable media library key from list_library." },
                    "range_id": { "type": "string", "description": "Keyword range id from list_library." },
                    "label": { "type": "string", "description": "Updated keyword label." },
                    "start_ns": { "type": "integer", "description": "Updated range start in nanoseconds." },
                    "end_ns": { "type": "integer", "description": "Updated range end in nanoseconds." }
                },
                "required": ["library_key", "range_id", "label", "start_ns", "end_ns"]
            }
        },
        {
            "name": "delete_media_keyword_range",
            "description": "Delete a keyword range from a library item.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "library_key": { "type": "string", "description": "Stable media library key from list_library." },
                    "range_id": { "type": "string", "description": "Keyword range id from list_library." }
                },
                "required": ["library_key", "range_id"]
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
                    "kind":        { "type": "string",  "description": "Transition kind. Use any supported transition id from the Inspector/Transitions pane (for example 'cross_dissolve', 'circle_open', or 'slide_left'), or empty string to clear." },
                    "duration_ns": { "type": "integer", "description": "Transition duration in nanoseconds." },
                    "alignment":   { "type": "string",  "description": "Overlap placement: 'end_on_cut' (default), 'center_on_cut', or 'start_on_cut'." }
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
            "name": "set_proxy_sidecar_persistence",
            "description": "Control whether proxy files are mirrored into UltimateSlice.cache directories beside original media for reuse after reopen.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "enabled": { "type": "boolean", "description": "true to enable, false to disable." }
                },
                "required": ["enabled"]
            }
        },
        {
            "name": "set_clip_lut",
            "description": "Set the 3D LUT (.cube) stack for a clip. LUTs are applied sequentially on export via ffmpeg lut3d. Pass an array of paths to set (empty array or null to clear). A single string path is accepted for backward compatibility (sets a single-element stack).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id":  { "type": "string", "description": "Clip id (from list_clips)." },
                    "lut_paths": { "type": ["array", "string", "null"], "description": "Array of absolute .cube LUT file paths (applied in order), a single path string, or null to clear." }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "set_clip_transform",
            "description": "Set scale, position, and optional rotation/anamorphic offset for a clip. scale > 1.0 zooms in (crops), scale < 1.0 zooms out (letterbox). position_x/y shift the frame from -1.0 (full left/top) to 1.0 (full right/bottom). rotate is in degrees (-180 to 180 typical). anamorphic_desqueeze applies lens expansion (e.g. 1.33, 2.0).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id":    { "type": "string", "description": "Clip id (from list_clips)." },
                    "scale":      { "type": "number", "description": "Zoom scale factor: 1.0 = normal, 2.0 = 2× zoom in, 0.5 = half size. Range 0.1–4.0." },
                    "position_x": { "type": "number", "description": "Horizontal offset: -1.0 (left) to 1.0 (right). Default 0.0 (center)." },
                    "position_y": { "type": "number", "description": "Vertical offset: -1.0 (top) to 1.0 (bottom). Default 0.0 (center)." },
                    "rotate":     { "type": "integer", "description": "Rotation in degrees. Optional; omit to keep existing value." },
                    "anamorphic_desqueeze": { "type": "number", "description": "Anamorphic desqueeze factor (1.0 = none, 1.33, 1.5, 1.8, 2.0). Optional." }
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
            "name": "set_clip_voice_isolation",
            "description": "Set voice isolation (smart noise gating). 0.0 is off, 1.0 is full gating. Requires either generated subtitles (default source) or analyzed silence intervals (set source via set_voice_isolation_source).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id" },
                    "voice_isolation": { "type": "number", "description": "Isolation amount 0.0 to 1.0" }
                },
                "required": ["clip_id", "voice_isolation"]
            }
        },
        {
            "name": "set_clip_voice_enhance",
            "description": "Toggle the per-clip 'Enhance Voice' chain (high-pass, FFT denoise, mud cut, presence boost, gentle compressor). Applied before voice isolation in the audio chain. When enabled, a background ffmpeg prerender produces a sidecar mp4 that the Program Monitor swaps in for byte-identical preview/export. Strength scales every stage from subtle (0.0) to broadcast (1.0).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id (from list_clips)." },
                    "enabled": { "type": "boolean", "description": "Whether the voice enhance chain is on for this clip." },
                    "strength": { "type": "number", "description": "Optional strength 0.0–1.0. Omit to keep the current value (defaults to 0.5 on first enable)." }
                },
                "required": ["clip_id", "enabled"]
            }
        },
        {
            "name": "set_clip_subtitle_visible",
            "description": "Toggle whether a clip's subtitles are rendered. When false, the clip's subtitles are hidden from the Program Monitor overlay, the export burn-in (ASS filter), and the SRT sidecar export — but the underlying segment data is preserved so the transcript editor and voice isolation (Subtitles source) keep working. Defaults to true.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id (from list_clips)." },
                    "visible": { "type": "boolean", "description": "Whether subtitles are rendered for this clip." }
                },
                "required": ["clip_id", "visible"]
            }
        },
        {
            "name": "set_voice_isolation_source",
            "description": "Choose the source of voice-isolation gate intervals. 'subtitles' uses Whisper word timings (default, requires generated subtitles). 'silence' uses ffmpeg silencedetect intervals (requires analyze_voice_isolation_silence first).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id" },
                    "source": { "type": "string", "enum": ["subtitles", "silence"], "description": "Interval source" }
                },
                "required": ["clip_id", "source"]
            }
        },
        {
            "name": "set_voice_isolation_silence_params",
            "description": "Set the silence-detect parameters used by analyze_voice_isolation_silence. Threshold is in dB (more negative = stricter). Min gap is in milliseconds. Either parameter is optional; the unset one keeps its current value. Changing either invalidates any cached analysis.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id" },
                    "threshold_db": { "type": "number", "description": "Silence threshold (-60 to -10 dB)" },
                    "min_ms": { "type": "integer", "description": "Minimum silence duration in milliseconds (50 to 2000)" }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "suggest_voice_isolation_threshold",
            "description": "Analyze the clip's noise floor with ffmpeg astats and return a suggested silence-detect threshold (dB). Uses 5th percentile of windowed RMS + 6 dB headroom. Does not mutate the clip — caller can pass the result to set_voice_isolation_silence_params.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id" }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "analyze_voice_isolation_silence",
            "description": "Run ffmpeg silencedetect on the clip's source audio with the clip's current threshold and min-gap parameters, and store the inverted speech intervals on the clip. Required before silence-mode voice isolation can take effect.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id" }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "set_clip_eq",
            "description": "Set 3-band parametric EQ on a clip. Each band has freq (Hz), gain (dB), and Q (bandwidth). All parameters optional — omitted fields keep their current value.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id":   { "type": "string", "description": "Clip id (from list_clips)." },
                    "low_freq":  { "type": "number", "description": "Low band center frequency (20–20000 Hz, default 200)." },
                    "low_gain":  { "type": "number", "description": "Low band gain in dB (−24 to +24, default 0)." },
                    "low_q":     { "type": "number", "description": "Low band Q factor (0.1–10.0, default 1.0)." },
                    "mid_freq":  { "type": "number", "description": "Mid band center frequency (20–20000 Hz, default 1000)." },
                    "mid_gain":  { "type": "number", "description": "Mid band gain in dB (−24 to +24, default 0)." },
                    "mid_q":     { "type": "number", "description": "Mid band Q factor (0.1–10.0, default 1.0)." },
                    "high_freq": { "type": "number", "description": "High band center frequency (20–20000 Hz, default 5000)." },
                    "high_gain": { "type": "number", "description": "High band gain in dB (−24 to +24, default 0)." },
                    "high_q":    { "type": "number", "description": "High band Q factor (0.1–10.0, default 1.0)." }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "normalize_clip_audio",
            "description": "Analyze clip loudness and normalize volume. Measures integrated loudness (LUFS) or peak amplitude via ffmpeg, then adjusts clip volume to reach target level. Blocks while ffmpeg analyzes audio (typically 1–5 seconds).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id":      { "type": "string", "description": "Clip id (from list_clips)." },
                    "mode":         { "type": "string", "enum": ["peak", "lufs"], "description": "Normalization mode: 'peak' for peak amplitude, 'lufs' for EBU R128 integrated loudness. Default 'lufs'." },
                    "target_level": { "type": "number", "description": "Target level in dB. For 'lufs': -14.0 (YouTube), -23.0 (broadcast). For 'peak': 0.0 or -1.0. Default -14.0." }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "analyze_project_loudness",
            "description": "Render the full timeline mixdown (all tracks, effects, crossfades, ducking, per-role submixes) to a temp file and run EBU R128 analysis on it. Returns the complete loudness report: integrated LUFS, short-term max, momentary max, LRA, true peak. Use this before `set_project_master_gain_db` to compute the delta for a broadcast-standard normalize. Blocks while ffmpeg renders + analyzes (typically 5–30 seconds depending on timeline length).",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        },
        {
            "name": "set_project_master_gain_db",
            "description": "Set the project-level master audio gain in dB. Applied post-mixdown in both the Program Monitor preview and the final export. Use this to normalize the entire timeline to a broadcast-standard loudness target (−23 LUFS EBU R128, −24 ATSC A/85, −27 Netflix, −16 Apple Podcasts, −14 Spotify/YouTube). Clamped to ±24 dB. Undoable. Pass 0.0 to reset.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "master_gain_db": {
                        "type": "number",
                        "description": "Gain in dB. Clamped to ±24 dB. Positive values boost, negative values attenuate. 0.0 = no-op (default)."
                    }
                },
                "required": ["master_gain_db"]
            }
        },
        {
            "name": "match_clip_audio",
            "description": "Match a source clip's audio tone toward a reference clip using integrated loudness plus the built-in 3-band EQ AND a higher-resolution 7-band match EQ for fine mic-matching (e.g., making a lav mic sound more like a shotgun mic). The matcher derives adaptive band frequency/gain/Q targets from speech-focused spectral differences, preferring subtitle/STT dialogue regions when available and otherwise weighting voice-active frames. Channel-aware analysis defaults to auto-detecting dominant one-sided audio while respecting existing clip routing; optional source/reference channel overrides and start/end ranges let you target a specific side or phrase. The 7-band match EQ is independent of the user 3-band EQ — both are applied in series during export. The operation is undoable.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source_clip_id": { "type": "string", "description": "Clip id to adjust (the clip that will be modified)." },
                    "source_start_ns": { "type": "integer", "description": "Optional source subrange start in nanoseconds, relative to the clip's current in-point." },
                    "source_end_ns": { "type": "integer", "description": "Optional source subrange end in nanoseconds, relative to the clip's current in-point." },
                    "source_channel_mode": { "type": "string", "description": "Optional source channel analysis mode: `auto`, `mono_mix`, `left`, or `right`." },
                    "reference_clip_id": { "type": "string", "description": "Clip id whose tonal balance and loudness should be matched." },
                    "reference_start_ns": { "type": "integer", "description": "Optional reference subrange start in nanoseconds, relative to the clip's current in-point." },
                    "reference_end_ns": { "type": "integer", "description": "Optional reference subrange end in nanoseconds, relative to the clip's current in-point." },
                    "reference_channel_mode": { "type": "string", "description": "Optional reference channel analysis mode: `auto`, `mono_mix`, `left`, or `right`." }
                },
                "required": ["source_clip_id", "reference_clip_id"]
            }
        },
        {
            "name": "clear_match_eq",
            "description": "Clear the 7-band match EQ on a clip (the result of a prior match_clip_audio call). Leaves the user 3-band EQ untouched. The operation is undoable.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id whose match EQ should be cleared." }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "detect_scene_cuts",
            "description": "Detect scene/shot changes in a clip using ffmpeg scdet and split the clip at each detected cut point. Blocks while ffmpeg analyzes the video (duration depends on clip length).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id":   { "type": "string", "description": "Clip id (from list_clips)." },
                    "track_id":  { "type": "string", "description": "Track id containing the clip (from list_tracks)." },
                    "threshold": { "type": "number", "description": "Scene change sensitivity (1-50, default 10). Lower values detect more cuts." }
                },
                "required": ["clip_id", "track_id"]
            }
        },
        {
            "name": "generate_music",
            "description": "Generate music from a text prompt using MusicGen AI. Places the generated WAV clip on an audio track. Requires MusicGen ONNX models to be installed. Returns immediately with a job_id; poll list_clips to see the clip when generation completes. When `reference_audio_path` is provided, UltimateSlice analyzes the reference clip locally (BPM, key/mode, brightness, dynamics) and appends the derived natural-language style hints to the prompt before queuing the job; analysis failures degrade gracefully and the original prompt is used.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "prompt":           { "type": "string", "description": "Text description of the music to generate (e.g. 'upbeat jazz piano')." },
                    "duration_secs":    { "type": "number", "description": "Duration of generated audio in seconds (1-30, default 10)." },
                    "track_index":      { "type": "integer", "description": "Audio track index to place the clip (default: first audio track)." },
                    "timeline_start_ns": { "type": "integer", "description": "Timeline position in nanoseconds (default: current playhead)." },
                    "reference_audio_path": { "type": "string", "description": "Optional path to a reference audio (or video with audio) file. UltimateSlice will analyze BPM, key/mode, brightness, and dynamics from the reference and append the derived natural-language style hints to the prompt." }
                },
                "required": ["prompt"]
            }
        },
        {
            "name": "record_voiceover",
            "description": "Record audio from the default microphone for a fixed duration and place it as a clip on an audio track at the current playhead position. Blocks while recording.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "duration_ns": { "type": "integer", "description": "Recording duration in nanoseconds." },
                    "track_index": { "type": "integer", "description": "Target audio track index (default: first audio track)." }
                },
                "required": ["duration_ns"]
            }
        },
        {
            "name": "set_clip_blend_mode",
            "description": "Set compositing blend mode for a clip by id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id":     { "type": "string", "description": "Clip id (from list_clips)." },
                    "blend_mode":  { "type": "string", "enum": ["normal", "multiply", "screen", "overlay", "add", "difference", "soft_light"], "description": "Blend mode for compositing." }
                },
                "required": ["clip_id", "blend_mode"]
            }
        },
        {
            "name": "set_clip_keyframe",
            "description": "Create or update a phase-1 keyframe (position_x, position_y, scale, opacity, brightness, contrast, saturation, temperature, tint, volume, pan, speed, rotate, crop_left, crop_right, crop_top, crop_bottom, blur) for a clip at a timeline position.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id (from list_clips)." },
                    "property": { "type": "string", "enum": ["position_x", "position_y", "scale", "opacity", "brightness", "contrast", "saturation", "temperature", "tint", "volume", "pan", "speed", "rotate", "crop_left", "crop_right", "crop_top", "crop_bottom", "blur"], "description": "Animated property to keyframe." },
                    "timeline_pos_ns": { "type": "integer", "description": "Absolute timeline position in nanoseconds. Optional; defaults to current playhead." },
                    "value": { "type": "number", "description": "Property value at this keyframe time." },
                    "interpolation": { "type": "string", "enum": ["linear", "ease_in", "ease_out", "ease_in_out"], "description": "Interpolation mode for the segment following this keyframe. Optional; defaults to linear." },
                    "bezier_controls": {
                        "type": "object",
                        "description": "Optional custom cubic-bezier controls for the outgoing segment from this keyframe. Values are normalized 0.0..1.0.",
                        "properties": {
                            "x1": { "type": "number" },
                            "y1": { "type": "number" },
                            "x2": { "type": "number" },
                            "y2": { "type": "number" }
                        }
                    }
                },
                "required": ["clip_id", "property", "value"]
            }
        },
        {
            "name": "remove_clip_keyframe",
            "description": "Remove a phase-1 keyframe (position_x, position_y, scale, opacity, brightness, contrast, saturation, temperature, tint, volume, pan, speed, rotate, crop_left, crop_right, crop_top, crop_bottom, blur) at a timeline position for a clip.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id (from list_clips)." },
                    "property": { "type": "string", "enum": ["position_x", "position_y", "scale", "opacity", "brightness", "contrast", "saturation", "temperature", "tint", "volume", "pan", "speed", "rotate", "crop_left", "crop_right", "crop_top", "crop_bottom", "blur"], "description": "Animated property keyframe lane." },
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
            "name": "set_prerender_quality",
            "description": "Set the x264 preset and CRF used for background prerendered overlap segments. Lower CRF improves fidelity but increases cache size and render time; slower presets improve compression efficiency at higher CPU cost.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "preset": {
                        "type": "string",
                        "enum": ["ultrafast", "superfast", "veryfast", "faster", "fast", "medium"],
                        "description": "x264 preset used for background prerender video segments."
                    },
                    "crf": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 51,
                        "description": "x264 CRF used for background prerender video segments. Lower values improve quality and increase size. Default is 20."
                    }
                },
                "required": ["preset", "crf"]
            }
        },
        {
            "name": "set_prerender_project_persistence",
            "description": "Control whether saved projects keep reusable prerender cache files beside the project file instead of using only the temporary cache root.",
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
            "name": "match_frame",
            "description": "Match Frame: find a timeline clip's source in the media library, load it in the Source Monitor, and seek to the matching source timecode. Uses the selected clip or the specified clip_id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Optional clip ID to match. If omitted, uses the currently selected clip." }
                }
            }
        },
        {
            "name": "list_backups",
            "description": "List available versioned backup files with timestamps and sizes.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "list_project_snapshots",
            "description": "List named snapshots for the current project, newest first.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "create_project_snapshot",
            "description": "Create a named snapshot of the current project without changing its primary save path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Human-readable snapshot name such as 'Before color pass'." }
                },
                "required": ["name"]
            }
        },
        {
            "name": "restore_project_snapshot",
            "description": "Restore a named snapshot into the current project while preserving the current primary save path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "snapshot_id": { "type": "string", "description": "Snapshot id from list_project_snapshots." }
                },
                "required": ["snapshot_id"]
            }
        },
        {
            "name": "delete_project_snapshot",
            "description": "Delete a named project snapshot by id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "snapshot_id": { "type": "string", "description": "Snapshot id from list_project_snapshots." }
                },
                "required": ["snapshot_id"]
            }
        },
        {
            "name": "set_clip_stabilization",
            "description": "Enable or configure video stabilization (libvidstab) on a clip. Stabilization is applied during export (two-pass analysis + transform).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip ID to stabilize." },
                    "enabled": { "type": "boolean", "description": "Enable or disable stabilization." },
                    "smoothing": { "type": "number", "description": "Smoothing strength: 0.0 (minimal) to 1.0 (maximum). Default 0.5." }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "set_clip_auto_crop_track",
            "description": "Auto-crop and track: create a motion tracker for the given clip-local region, reframe the clip so the tracked region stays centered at the project's aspect ratio (including cross-aspect 16:9 -> 9:16 reframing), and enqueue a background tracking job that will keep the region centered over time. Reuses any existing motion tracker on the clip that matches the region; otherwise creates a new tracker.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip ID to auto-crop." },
                    "center_x": { "type": "number", "description": "Region center X in normalized clip coordinates (0..1, where 0.5 = center)." },
                    "center_y": { "type": "number", "description": "Region center Y in normalized clip coordinates (0..1, where 0.5 = center)." },
                    "width": { "type": "number", "description": "Region HALF-width in normalized clip coordinates (0..0.5). Full region width = 2 * width." },
                    "height": { "type": "number", "description": "Region HALF-height in normalized clip coordinates (0..0.5). Full region height = 2 * height." },
                    "padding": { "type": "number", "description": "Optional extra headroom around the region as a fraction (e.g. 0.1 = 10% margin). Clamped to [0, 0.5]. Default 0.1." }
                },
                "required": ["clip_id", "center_x", "center_y", "width", "height"]
            }
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
            "description": "Synchronize two or more timeline clips by audio cross-correlation. The first clip is used as the anchor; other clips are repositioned based on matching audio content. When replace_audio is true (default false), the anchor's embedded audio is muted and all clips are linked so the external audio replaces the camera audio.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Two or more clip ids to sync. First clip is the anchor (typically the camera clip)."
                    },
                    "replace_audio": {
                        "type": "boolean",
                        "description": "When true, link all synced clips and mute the anchor clip's embedded audio so external audio replaces it. Default false."
                    }
                },
                "required": ["clip_ids"]
            }
        },
        {
            "name": "copy_clip_color_grade",
            "description": "Copy color grading values from a clip into an internal clipboard. The copied grade can then be pasted onto other clips with paste_clip_color_grade. Copies static values only (brightness, contrast, saturation, temperature, tint, exposure, black_point, shadows, midtones, highlights, warmth/tint per tonal region, denoise, sharpness, lut_paths).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Source clip id to copy color grade from" }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "paste_clip_color_grade",
            "description": "Paste the previously copied color grading values onto a target clip. Requires a prior copy_clip_color_grade call. Applies static color values only (no keyframes).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Target clip id to paste color grade onto" }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "match_clip_colors",
            "description": "Automatically grade a source clip to match the color appearance of a reference clip. Samples frames from both clips, analyses color statistics in CIE L*a*b* space, and computes slider adjustments (brightness, contrast, saturation, temperature, tint). Optionally generates a .cube 3D LUT for finer non-linear matching. The operation is undoable.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source_clip_id": { "type": "string", "description": "Clip id to adjust (the one that will be modified)" },
                    "reference_clip_id": { "type": "string", "description": "Clip id to match (the target look)" },
                    "generate_lut": { "type": "boolean", "description": "When true, also generate and assign a .cube 3D LUT for finer matching (default false)" },
                    "sample_count": { "type": "integer", "description": "Number of frames to sample from each clip (1–20, default 8)" }
                },
                "required": ["source_clip_id", "reference_clip_id"]
            }
        },
        {
            "name": "list_frei0r_plugins",
            "description": "List all available frei0r filter plugins discovered from the GStreamer registry. Returns plugin names, display names, categories, and parameter info.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "list_clip_frei0r_effects",
            "description": "List frei0r effects currently applied to a clip, in order.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id" }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "add_clip_frei0r_effect",
            "description": "Add a frei0r filter effect to a clip. The effect is appended to the end of the effect chain. Returns the generated effect id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id" },
                    "plugin_name": { "type": "string", "description": "Frei0r plugin name (e.g. 'cartoon', 'glow')" },
                    "params": { "type": "object", "description": "Optional numeric parameter overrides as {param_name: value} (frei0r doubles 0.0-1.0)" },
                    "string_params": { "type": "object", "description": "Optional string parameter overrides as {param_name: value} (e.g. blend-mode, pattern)" }
                },
                "required": ["clip_id", "plugin_name"]
            }
        },
        {
            "name": "remove_clip_frei0r_effect",
            "description": "Remove a frei0r effect from a clip by its effect instance id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id" },
                    "effect_id": { "type": "string", "description": "Effect instance id (from list_clip_frei0r_effects)" }
                },
                "required": ["clip_id", "effect_id"]
            }
        },
        {
            "name": "set_clip_frei0r_effect_params",
            "description": "Update parameters on a frei0r effect applied to a clip.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id" },
                    "effect_id": { "type": "string", "description": "Effect instance id" },
                    "params": { "type": "object", "description": "Numeric parameter values as {param_name: value}" },
                    "string_params": { "type": "object", "description": "Optional string parameter values as {param_name: value}" }
                },
                "required": ["clip_id", "effect_id", "params"]
            }
        },
        {
            "name": "reorder_clip_frei0r_effects",
            "description": "Reorder frei0r effects on a clip. Provide the complete list of effect ids in the desired order.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id" },
                    "effect_ids": { "type": "array", "items": { "type": "string" }, "description": "All effect ids in desired order" }
                },
                "required": ["clip_id", "effect_ids"]
            }
        }
        ,{
            "name": "add_title_clip",
            "description": "Add a standalone title clip to the timeline from a built-in template. Templates: lower_third_banner, lower_third_clean, centered_title, subtitle, full_screen, chapter_heading, cinematic, end_credits, callout.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "template_id": { "type": "string", "description": "Template id (e.g. 'centered_title')" },
                    "track_index": { "type": "integer", "description": "Video track index (default: first video track)" },
                    "timeline_start_ns": { "type": "integer", "description": "Timeline start position in nanoseconds (default: playhead)" },
                    "duration_ns": { "type": "integer", "description": "Clip duration in nanoseconds (default: 5 seconds)" },
                    "title_text": { "type": "string", "description": "Override title text (default: template name)" }
                },
                "required": ["template_id"]
            }
        },
        {
            "name": "add_adjustment_layer",
            "description": "Add an adjustment layer clip at a track index and timeline position. Adjustment layer effects (color grading, LUTs, frei0r) apply to the composited result of all tracks below.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "track_index": { "type": "integer", "description": "Video track index" },
                    "timeline_start_ns": { "type": "integer", "description": "Timeline start position in nanoseconds" },
                    "duration_ns": { "type": "integer", "description": "Duration in nanoseconds" }
                },
                "required": ["track_index", "timeline_start_ns", "duration_ns"]
            }
        },
        {
            "name": "set_clip_title_style",
            "description": "Set title/text overlay styling properties on a clip. Includes font, color, position, outline, shadow, background box.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "Clip id" },
                    "title_text": { "type": "string" },
                    "title_font": { "type": "string", "description": "Pango font description (e.g. 'Sans Bold 36')" },
                    "title_color": { "type": "integer", "description": "Text color as 0xRRGGBBAA" },
                    "title_x": { "type": "number", "description": "Horizontal position 0.0-1.0" },
                    "title_y": { "type": "number", "description": "Vertical position 0.0-1.0" },
                    "title_outline_width": { "type": "number", "description": "Outline width in pts (0=none)" },
                    "title_outline_color": { "type": "integer", "description": "Outline color as 0xRRGGBBAA" },
                    "title_shadow": { "type": "boolean", "description": "Enable drop shadow" },
                    "title_shadow_color": { "type": "integer" },
                    "title_shadow_offset_x": { "type": "number" },
                    "title_shadow_offset_y": { "type": "number" },
                    "title_bg_box": { "type": "boolean", "description": "Enable background box" },
                    "title_bg_box_color": { "type": "integer" },
                    "title_bg_box_padding": { "type": "number" },
                    "title_clip_bg_color": { "type": "integer", "description": "Title clip background color (0=transparent)" },
                    "title_secondary_text": { "type": "string" }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "add_to_export_queue",
            "description": "Add an export job to the batch queue. Optionally load settings from a named preset.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "output_path": { "type": "string", "description": "Full output file path (e.g. /home/user/output.gif)" },
                    "preset_name": { "type": "string", "description": "Name of a saved export preset to use. If omitted, uses last-used preset." }
                },
                "required": ["output_path"]
            }
        },
        {
            "name": "list_export_queue",
            "description": "List all jobs in the batch export queue with their current status.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "clear_export_queue",
            "description": "Remove jobs from the batch export queue. Use status_filter to only remove done or error jobs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "status_filter": { "type": "string", "description": "Which jobs to remove: 'all' (default), 'done', or 'error'" }
                }
            }
        },
        {
            "name": "run_export_queue",
            "description": "Run all pending jobs in the batch export queue sequentially. Blocks until complete.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "create_compound_clip",
            "description": "Create a compound (nested timeline) clip from the specified clip IDs. The selected clips are replaced by a single compound clip that contains them as an internal sub-timeline.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Array of clip IDs to nest into a compound clip (minimum 2)"
                    }
                },
                "required": ["clip_ids"]
            }
        },
        {
            "name": "break_apart_compound_clip",
            "description": "Break apart a compound clip, restoring its internal clips to the timeline.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "ID of the compound clip to break apart" }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "create_multicam_clip",
            "description": "Create a multicam clip from 2+ video clip IDs. Clips are synced by audio cross-correlation and combined into a single multicam clip with per-angle source data.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Array of clip IDs to combine into a multicam clip (minimum 2)"
                    }
                },
                "required": ["clip_ids"]
            }
        },
        {
            "name": "add_angle_switch",
            "description": "Insert an angle switch at a position within a multicam clip. If a switch already exists at that position, its angle is updated.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "ID of the multicam clip" },
                    "position_ns": { "type": "integer", "description": "Position within the clip (nanoseconds from clip start)" },
                    "angle_index": { "type": "integer", "description": "Zero-based index of the angle to switch to" }
                },
                "required": ["clip_id", "position_ns", "angle_index"]
            }
        },
        {
            "name": "list_multicam_angles",
            "description": "List the angles and switch points of a multicam clip.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "ID of the multicam clip" }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "set_multicam_angle_audio",
            "description": "Set volume and/or mute state for a multicam angle's audio. Unmuted angles with volume > 0 are mixed together in the audio output.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "ID of the multicam clip" },
                    "angle_index": { "type": "integer", "description": "0-based angle index" },
                    "volume": { "type": "number", "description": "Volume level 0.0 (silent) to 1.0 (full); omit to keep current" },
                    "muted": { "type": "boolean", "description": "Whether to mute this angle's audio; omit to keep current" }
                },
                "required": ["clip_id", "angle_index"]
            }
        },
        {
            "name": "set_multicam_angle_color",
            "description": "Set per-angle color grade and/or LUT for a multicam angle. When set, these override the clip-level values for this angle. Omit a field to keep its current value. Pass an empty lut_paths array to clear the per-angle LUT (falls back to clip-level).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "ID of the multicam clip" },
                    "angle_index": { "type": "integer", "description": "0-based angle index" },
                    "brightness": { "type": "number", "description": "Brightness (−1.0 to 1.0, neutral 0.0); omit to keep current" },
                    "contrast": { "type": "number", "description": "Contrast (0.0 to 2.0, neutral 1.0); omit to keep current" },
                    "saturation": { "type": "number", "description": "Saturation (0.0 to 2.0, neutral 1.0); omit to keep current" },
                    "temperature": { "type": "number", "description": "Temperature in Kelvin (2000 to 10000, neutral 6500); omit to keep current" },
                    "tint": { "type": "number", "description": "Tint (−1.0 to 1.0, neutral 0.0); omit to keep current" },
                    "lut_paths": { "type": "array", "items": { "type": "string" }, "description": "Per-angle .cube LUT file paths; overrides clip-level LUT. Empty array clears." }
                },
                "required": ["clip_id", "angle_index"]
            }
        },
        // ── Audition / clip-versions tools ────────────────────────────────
        {
            "name": "create_audition_clip",
            "description": "Group 2+ clips on the same track into a single audition clip. The clip at active_index becomes the active take that drives playback and export; the others are kept as nondestructive alternates.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_ids": { "type": "array", "items": { "type": "string" }, "description": "Array of clip IDs to combine into the audition (minimum 2)" },
                    "active_index": { "type": "integer", "description": "0-based index into clip_ids of the take to make active. Default 0." }
                },
                "required": ["clip_ids"]
            }
        },
        {
            "name": "add_audition_take",
            "description": "Append a new take to an existing audition clip. The new take is added at the end and is NOT made active.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "audition_clip_id": { "type": "string", "description": "ID of the audition clip" },
                    "source_path": { "type": "string", "description": "Path to the source media file for the new take" },
                    "source_in_ns": { "type": "integer", "description": "In point of the take in the source file (nanoseconds)" },
                    "source_out_ns": { "type": "integer", "description": "Out point of the take in the source file (nanoseconds)" },
                    "label": { "type": "string", "description": "Optional human-readable label for the take" }
                },
                "required": ["audition_clip_id", "source_path", "source_in_ns", "source_out_ns"]
            }
        },
        {
            "name": "remove_audition_take",
            "description": "Remove a take from an audition clip by index. Refuses to remove the currently active take — switch active first if needed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "audition_clip_id": { "type": "string", "description": "ID of the audition clip" },
                    "take_index": { "type": "integer", "description": "0-based index of the take to remove" }
                },
                "required": ["audition_clip_id", "take_index"]
            }
        },
        {
            "name": "set_active_audition_take",
            "description": "Switch the active take of an audition clip. The audition's host fields (source_path/source_in/source_out) are updated to the chosen take, so playback and export immediately reflect the new selection. Any field tweaks made while the previous take was active are snapshotted into the takes list before the swap.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "audition_clip_id": { "type": "string", "description": "ID of the audition clip" },
                    "take_index": { "type": "integer", "description": "0-based index of the take to make active" }
                },
                "required": ["audition_clip_id", "take_index"]
            }
        },
        {
            "name": "list_audition_takes",
            "description": "List all takes in an audition clip, with the active take index.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "audition_clip_id": { "type": "string", "description": "ID of the audition clip" }
                },
                "required": ["audition_clip_id"]
            }
        },
        {
            "name": "finalize_audition",
            "description": "Collapse an audition clip to a normal clip referencing only its currently active take. Discards alternate takes. Undoable.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "audition_clip_id": { "type": "string", "description": "ID of the audition clip to finalize" }
                },
                "required": ["audition_clip_id"]
            }
        },
        // ── Subtitle / STT tools ──────────────────────────────────────────
        {
            "name": "generate_subtitles",
            "description": "Run speech-to-text on a clip to generate subtitle segments. Returns immediately; poll get_clip_subtitles for results.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "ID of the clip to transcribe" },
                    "language": { "type": "string", "description": "Language code (en, es, fr, de, ja, zh, auto). Default: auto" }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "get_clip_subtitles",
            "description": "Get all subtitle segments for a clip, including word-level timestamps.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "ID of the clip" }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "edit_subtitle_text",
            "description": "Edit the text of a subtitle segment.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "ID of the clip" },
                    "segment_id": { "type": "string", "description": "ID of the subtitle segment" },
                    "text": { "type": "string", "description": "New text for the segment" }
                },
                "required": ["clip_id", "segment_id", "text"]
            }
        },
        {
            "name": "edit_subtitle_timing",
            "description": "Edit the start/end timing of a subtitle segment.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "ID of the clip" },
                    "segment_id": { "type": "string", "description": "ID of the subtitle segment" },
                    "start_ns": { "type": "integer", "description": "New start time in nanoseconds (source-relative)" },
                    "end_ns": { "type": "integer", "description": "New end time in nanoseconds (source-relative)" }
                },
                "required": ["clip_id", "segment_id", "start_ns", "end_ns"]
            }
        },
        {
            "name": "clear_subtitles",
            "description": "Remove all subtitle segments from a clip.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "ID of the clip" }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "delete_transcript_range",
            "description": "Delete a contiguous range of words from a clip's transcript and ripple-shift downstream clips. Splits the clip at the selected word boundaries and removes the middle slice as a single undo entry. Word indices reference the clip's flattened word list (segment 0 word 0, segment 0 word 1, segment 1 word 0, ...). Use list_clips or get_clip_subtitles to discover available words.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "ID of the clip whose transcript range should be deleted" },
                    "start_word_index": { "type": "integer", "minimum": 0, "description": "Index of the first word to delete (inclusive)" },
                    "end_word_index": { "type": "integer", "minimum": 1, "description": "Index one past the last word to delete (exclusive)" }
                },
                "required": ["clip_id", "start_word_index", "end_word_index"]
            }
        },
        {
            "name": "set_subtitle_style",
            "description": "Set subtitle display style for a clip (font, colors, base styles, highlight flags).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string", "description": "ID of the clip" },
                    "font": { "type": "string", "description": "Font descriptor e.g. 'Sans Bold 24'" },
                    "color": { "type": "integer", "description": "Text color as 0xRRGGBBAA" },
                    "outline_color": { "type": "integer", "description": "Outline color as 0xRRGGBBAA" },
                    "outline_width": { "type": "number", "description": "Outline width in pts" },
                    "bg_box": { "type": "boolean", "description": "Enable background box" },
                    "bg_box_color": { "type": "integer", "description": "Background box color as 0xRRGGBBAA" },
                    "highlight_mode": { "type": "string", "enum": ["none", "bold", "color", "underline", "stroke"], "description": "Legacy word highlight mode (prefer highlight flags)" },
                    "highlight_color": { "type": "integer", "description": "Highlight (text-fill) color as 0xRRGGBBAA, applied when highlight_color flag is on" },
                    "highlight_stroke_color": { "type": "integer", "description": "Highlight stroke color as 0xRRGGBBAA, applied when highlight_stroke flag is on. Independent from highlight_color so the karaoke stroke can differ from the karaoke text fill (e.g. yellow text + black stroke)." },
                    "bold": { "type": "boolean", "description": "Base style: bold for all subtitle text" },
                    "italic": { "type": "boolean", "description": "Base style: italic for all subtitle text" },
                    "underline": { "type": "boolean", "description": "Base style: underline for all subtitle text" },
                    "shadow": { "type": "boolean", "description": "Base style: shadow for all subtitle text" },
                    "highlight_bold": { "type": "boolean", "description": "Highlight flag: bold on active word" },
                    "highlight_color_flag": { "type": "boolean", "description": "Highlight flag: color on active word" },
                    "highlight_underline": { "type": "boolean", "description": "Highlight flag: underline on active word" },
                    "highlight_stroke": { "type": "boolean", "description": "Highlight flag: stroke on active word" },
                    "highlight_italic": { "type": "boolean", "description": "Highlight flag: italic on active word" },
                    "highlight_background": { "type": "boolean", "description": "Highlight flag: background on active word" },
                    "highlight_shadow": { "type": "boolean", "description": "Highlight flag: shadow on active word" },
                    "bg_highlight_color": { "type": "integer", "description": "Background highlight color as 0xRRGGBBAA" }
                },
                "required": ["clip_id"]
            }
        },
        {
            "name": "export_srt",
            "description": "Export all subtitles in the project as an SRT file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Output file path for the .srt file" }
                },
                "required": ["path"]
            }
        }
    ]})
}

// ── Tool dispatch ─────────────────────────────────────────────────────────────

fn tool_error_payload(code: i32, message: impl Into<String>) -> Value {
    json!({"code": code, "message": message.into()})
}

// -----------------------------------------------------------------------
// MCP argument extraction macros
// -----------------------------------------------------------------------
//
// `dispatch_tool_payload` and the per-tool match arms below historically
// repeated `arg_str!(args, "key")` and friends
// hundreds of times. These macros compress that boilerplate. Each macro has
// two forms: one with no default (uses the type's empty / falsy value) and
// one with an explicit default. Returning `String` for `arg_str!` matches
// the dominant call style — every existing site immediately called
// `.to_string()` on the borrowed `&str`.

/// Extract a JSON string argument as `String`. Default: empty string.
macro_rules! arg_str {
    ($args:expr, $key:expr) => {
        $args[$key].as_str().unwrap_or("").to_string()
    };
    ($args:expr, $key:expr, $default:expr) => {
        $args[$key].as_str().unwrap_or($default).to_string()
    };
}

/// Extract a JSON bool argument. Default: `false`.
macro_rules! arg_bool {
    ($args:expr, $key:expr) => {
        $args[$key].as_bool().unwrap_or(false)
    };
    ($args:expr, $key:expr, $default:expr) => {
        $args[$key].as_bool().unwrap_or($default)
    };
}

/// Extract a JSON `f64` argument. Caller-supplied default.
macro_rules! arg_f64 {
    ($args:expr, $key:expr, $default:expr) => {
        $args[$key].as_f64().unwrap_or($default)
    };
}

/// Extract a JSON `u64` argument. Caller-supplied default.
macro_rules! arg_u64 {
    ($args:expr, $key:expr, $default:expr) => {
        $args[$key].as_u64().unwrap_or($default)
    };
}

/// Extract a JSON `i64` argument. Caller-supplied default.
macro_rules! arg_i64 {
    ($args:expr, $key:expr, $default:expr) => {
        $args[$key].as_i64().unwrap_or($default)
    };
}

fn is_cacheable_read_tool(name: &str) -> bool {
    matches!(
        name,
        "get_project"
            | "list_tracks"
            | "list_clips"
            | "get_timeline_settings"
            | "get_playhead_position"
            | "get_performance_snapshot"
            | "get_preferences"
            | "list_export_presets"
            | "list_workspace_layouts"
            | "list_library"
            | "list_collections"
            | "list_project_snapshots"
            | "list_frei0r_plugins"
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
            compact: arg_bool!(args, "compact"),
            reply: tx,
        },
        "list_clips" => McpCommand::ListClips {
            compact: arg_bool!(args, "compact"),
            reply: tx,
        },
        "get_timeline_settings" => McpCommand::GetTimelineSettings { reply: tx },
        "get_playhead_position" => McpCommand::GetPlayheadPosition { reply: tx },
        "get_performance_snapshot" => McpCommand::GetPerformanceSnapshot { reply: tx },
        "set_magnetic_mode" => McpCommand::SetMagneticMode {
            enabled: arg_bool!(args, "enabled"),
            reply: tx,
        },
        "set_track_solo" => McpCommand::SetTrackSolo {
            track_id: arg_str!(args, "track_id"),
            solo: arg_bool!(args, "solo"),
            reply: tx,
        },
        "list_ladspa_plugins" => McpCommand::ListLadspaPlugins { reply: tx },
        "add_clip_ladspa_effect" => McpCommand::AddClipLadspaEffect {
            clip_id: arg_str!(args, "clip_id"),
            plugin_name: arg_str!(args, "plugin_name"),
            reply: tx,
        },
        "remove_clip_ladspa_effect" => McpCommand::RemoveClipLadspaEffect {
            clip_id: arg_str!(args, "clip_id"),
            effect_id: arg_str!(args, "effect_id"),
            reply: tx,
        },
        "set_clip_ladspa_effect_params" => McpCommand::SetClipLadspaEffectParams {
            clip_id: arg_str!(args, "clip_id"),
            effect_id: arg_str!(args, "effect_id"),
            params: args
                .get("params")
                .and_then(|v| v.as_object())
                .map(|obj| {
                    obj.iter()
                        .filter_map(|(k, v)| v.as_f64().map(|val| (k.clone(), val)))
                        .collect()
                })
                .unwrap_or_default(),
            reply: tx,
        },
        "set_track_role" => McpCommand::SetTrackRole {
            track_id: arg_str!(args, "track_id"),
            role: arg_str!(args, "role", "none"),
            reply: tx,
        },
        "set_track_duck" => McpCommand::SetTrackDuck {
            track_id: arg_str!(args, "track_id"),
            duck: arg_bool!(args, "duck"),
            reply: tx,
        },
        "set_track_height_preset" => McpCommand::SetTrackHeightPreset {
            track_id: arg_str!(args, "track_id"),
            height_preset: args["height_preset"]
                .as_str()
                .unwrap_or("medium")
                .to_string(),
            reply: tx,
        },
        "close_source_preview" => McpCommand::CloseSourcePreview { reply: tx },
        "get_preferences" => McpCommand::GetPreferences { reply: tx },
        "set_hardware_acceleration" => McpCommand::SetHardwareAcceleration {
            enabled: arg_bool!(args, "enabled"),
            reply: tx,
        },
        "set_playback_priority" => McpCommand::SetPlaybackPriority {
            priority: arg_str!(args, "priority", "smooth"),
            reply: tx,
        },
        "set_source_playback_priority" => McpCommand::SetSourcePlaybackPriority {
            priority: arg_str!(args, "priority", "smooth"),
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
        "set_prerender_quality" => {
            let (preset, crf) = match parse_prerender_quality_args(&args) {
                Ok(parsed) => parsed,
                Err(message) => return Err(tool_error_payload(-32602, message)),
            };
            McpCommand::SetPrerenderQuality {
                preset: preset.to_string(),
                crf,
                reply: tx,
            }
        }

        "add_clip" => McpCommand::AddClip {
            source_path: arg_str!(args, "source_path"),
            track_index: arg_u64!(args, "track_index", 0) as usize,
            timeline_start_ns: arg_u64!(args, "timeline_start_ns", 0),
            source_in_ns: arg_u64!(args, "source_in_ns", 0),
            source_out_ns: arg_u64!(args, "source_out_ns", 0),
            reply: tx,
        },

        "remove_clip" => McpCommand::RemoveClip {
            clip_id: arg_str!(args, "clip_id"),
            reply: tx,
        },

        "move_clip" => McpCommand::MoveClip {
            clip_id: arg_str!(args, "clip_id"),
            new_start_ns: arg_u64!(args, "new_start_ns", 0),
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
        "convert_ltc_audio_to_timecode" => {
            let ltc_channel = match parse_ltc_channel_arg(args.get("ltc_channel")) {
                Ok(channel) => channel,
                Err(message) => return Err(tool_error_payload(-32602, message)),
            };
            let frame_rate = match parse_ltc_frame_rate_arg(args.get("frame_rate")) {
                Ok(frame_rate) => frame_rate,
                Err(message) => return Err(tool_error_payload(-32602, message)),
            };
            McpCommand::ConvertLtcAudioToTimecode {
                clip_id: arg_str!(args, "clip_id"),
                ltc_channel,
                frame_rate,
                reply: tx,
            }
        }

        "trim_clip" => McpCommand::TrimClip {
            clip_id: arg_str!(args, "clip_id"),
            source_in_ns: arg_u64!(args, "source_in_ns", 0),
            source_out_ns: arg_u64!(args, "source_out_ns", 0),
            reply: tx,
        },

        "set_clip_speed" => McpCommand::SetClipSpeed {
            clip_id: arg_str!(args, "clip_id"),
            speed: arg_f64!(args, "speed", 1.0),
            slow_motion_interp: args["slow_motion_interp"].as_str().map(|s| s.to_string()),
            reply: tx,
        },

        "slip_clip" => McpCommand::SlipClip {
            clip_id: arg_str!(args, "clip_id"),
            delta_ns: arg_i64!(args, "delta_ns", 0),
            reply: tx,
        },

        "slide_clip" => McpCommand::SlideClip {
            clip_id: arg_str!(args, "clip_id"),
            delta_ns: arg_i64!(args, "delta_ns", 0),
            reply: tx,
        },

        "set_clip_color" => McpCommand::SetClipColor {
            clip_id: arg_str!(args, "clip_id"),
            brightness: arg_f64!(args, "brightness", 0.0),
            contrast: arg_f64!(args, "contrast", 1.0),
            saturation: arg_f64!(args, "saturation", 1.0),
            temperature: arg_f64!(args, "temperature", 6500.0),
            tint: arg_f64!(args, "tint", 0.0),
            denoise: arg_f64!(args, "denoise", 0.0),
            sharpness: arg_f64!(args, "sharpness", 0.0),
            blur: arg_f64!(args, "blur", 0.0),
            shadows: arg_f64!(args, "shadows", 0.0),
            midtones: arg_f64!(args, "midtones", 0.0),
            highlights: arg_f64!(args, "highlights", 0.0),
            exposure: arg_f64!(args, "exposure", 0.0),
            black_point: arg_f64!(args, "black_point", 0.0),
            highlights_warmth: arg_f64!(args, "highlights_warmth", 0.0),
            highlights_tint: arg_f64!(args, "highlights_tint", 0.0),
            midtones_warmth: arg_f64!(args, "midtones_warmth", 0.0),
            midtones_tint: arg_f64!(args, "midtones_tint", 0.0),
            shadows_warmth: arg_f64!(args, "shadows_warmth", 0.0),
            shadows_tint: arg_f64!(args, "shadows_tint", 0.0),
            reply: tx,
        },
        "set_clip_color_label" => McpCommand::SetClipColorLabel {
            clip_id: arg_str!(args, "clip_id"),
            color_label: arg_str!(args, "color_label", "none"),
            reply: tx,
        },

        "set_clip_hsl_qualifier" => {
            let clip_id = arg_str!(args, "clip_id");
            let clear = args
                .get("clear")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let qualifier = if clear {
                None
            } else {
                let mut q = crate::model::clip::HslQualifier::default();
                q.enabled = args
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                if let Some(v) = args.get("hue_min").and_then(|v| v.as_f64()) {
                    q.hue_min = v;
                }
                if let Some(v) = args.get("hue_max").and_then(|v| v.as_f64()) {
                    q.hue_max = v;
                }
                if let Some(v) = args.get("hue_softness").and_then(|v| v.as_f64()) {
                    q.hue_softness = v;
                }
                if let Some(v) = args.get("sat_min").and_then(|v| v.as_f64()) {
                    q.sat_min = v;
                }
                if let Some(v) = args.get("sat_max").and_then(|v| v.as_f64()) {
                    q.sat_max = v;
                }
                if let Some(v) = args.get("sat_softness").and_then(|v| v.as_f64()) {
                    q.sat_softness = v;
                }
                if let Some(v) = args.get("lum_min").and_then(|v| v.as_f64()) {
                    q.lum_min = v;
                }
                if let Some(v) = args.get("lum_max").and_then(|v| v.as_f64()) {
                    q.lum_max = v;
                }
                if let Some(v) = args.get("lum_softness").and_then(|v| v.as_f64()) {
                    q.lum_softness = v;
                }
                if let Some(v) = args.get("invert").and_then(|v| v.as_bool()) {
                    q.invert = v;
                }
                if let Some(v) = args.get("brightness").and_then(|v| v.as_f64()) {
                    q.brightness = v;
                }
                if let Some(v) = args.get("contrast").and_then(|v| v.as_f64()) {
                    q.contrast = v;
                }
                if let Some(v) = args.get("saturation").and_then(|v| v.as_f64()) {
                    q.saturation = v;
                }
                Some(q)
            };
            McpCommand::SetClipHslQualifier {
                clip_id,
                qualifier,
                reply: tx,
            }
        }

        "set_clip_chroma_key" => McpCommand::SetClipChromaKey {
            clip_id: arg_str!(args, "clip_id"),
            enabled: args.get("enabled").and_then(|v| v.as_bool()),
            color: args.get("color").and_then(|v| v.as_u64()).map(|v| v as u32),
            tolerance: args.get("tolerance").and_then(|v| v.as_f64()),
            softness: args.get("softness").and_then(|v| v.as_f64()),
            reply: tx,
        },

        "set_clip_bg_removal" => McpCommand::SetClipBgRemoval {
            clip_id: arg_str!(args, "clip_id"),
            enabled: args.get("enabled").and_then(|v| v.as_bool()),
            threshold: args.get("threshold").and_then(|v| v.as_f64()),
            reply: tx,
        },

        "set_clip_motion_blur" => McpCommand::SetClipMotionBlur {
            clip_id: arg_str!(args, "clip_id"),
            enabled: args.get("enabled").and_then(|v| v.as_bool()),
            shutter_angle: args.get("shutter_angle").and_then(|v| v.as_f64()),
            reply: tx,
        },

        "set_clip_mask" => McpCommand::SetClipMask {
            clip_id: arg_str!(args, "clip_id"),
            enabled: args.get("enabled").and_then(|v| v.as_bool()),
            shape: args
                .get("shape")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            center_x: args.get("center_x").and_then(|v| v.as_f64()),
            center_y: args.get("center_y").and_then(|v| v.as_f64()),
            width: args.get("width").and_then(|v| v.as_f64()),
            height: args.get("height").and_then(|v| v.as_f64()),
            rotation: args.get("rotation").and_then(|v| v.as_f64()),
            feather: args.get("feather").and_then(|v| v.as_f64()),
            expansion: args.get("expansion").and_then(|v| v.as_f64()),
            invert: args.get("invert").and_then(|v| v.as_bool()),
            path: args.get("path").cloned(),
            reply: tx,
        },

        "set_project_title" => McpCommand::SetTitle {
            title: arg_str!(args, "title"),
            reply: tx,
        },

        "save_fcpxml" => McpCommand::SaveFcpxml {
            path: arg_str!(args, "path"),
            reply: tx,
        },

        "save_edl" => McpCommand::SaveEdl {
            path: arg_str!(args, "path"),
            reply: tx,
        },

        "save_otio" => McpCommand::SaveOtio {
            path: arg_str!(args, "path"),
            path_mode: args
                .get("path_mode")
                .and_then(|v| v.as_str())
                .unwrap_or("absolute")
                .to_string(),
            reply: tx,
        },

        "save_project_with_media" => McpCommand::SaveProjectWithMedia {
            path: arg_str!(args, "path"),
            reply: tx,
        },

        "collect_project_files" => {
            let directory_path = arg_str!(args, "directory_path");
            if directory_path.is_empty() {
                return Err(tool_error_payload(-32602, "directory_path is required"));
            }
            let mode = match args.get("mode").and_then(|v| v.as_str()) {
                None => crate::fcpxml::writer::CollectFilesMode::TimelineUsedOnly,
                Some(value) => match crate::fcpxml::writer::CollectFilesMode::from_str(value) {
                    Some(mode) => mode,
                    None => {
                        return Err(tool_error_payload(
                            -32602,
                            "mode must be 'timeline_used' or 'entire_library'",
                        ));
                    }
                },
            };
            let use_collected_locations_on_next_save = args
                .get("use_collected_locations_on_next_save")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            McpCommand::CollectProjectFiles {
                directory_path,
                mode,
                use_collected_locations_on_next_save,
                reply: tx,
            }
        }

        "open_fcpxml" => McpCommand::OpenFcpxml {
            path: arg_str!(args, "path"),
            reply: tx,
        },

        "open_otio" => McpCommand::OpenOtio {
            path: arg_str!(args, "path"),
            reply: tx,
        },

        "export_mp4" => McpCommand::ExportMp4 {
            path: arg_str!(args, "path"),
            audio_channel_layout: arg_str!(args, "audio_channel_layout", "stereo"),
            reply: tx,
        },

        "list_export_presets" => McpCommand::ListExportPresets { reply: tx },

        "save_export_preset" => McpCommand::SaveExportPreset {
            name: arg_str!(args, "name"),
            video_codec: arg_str!(args, "video_codec", "h264"),
            container: arg_str!(args, "container", "mp4"),
            output_width: arg_u64!(args, "output_width", 0) as u32,
            output_height: arg_u64!(args, "output_height", 0) as u32,
            crf: arg_u64!(args, "crf", 23) as u32,
            audio_codec: arg_str!(args, "audio_codec", "aac"),
            audio_bitrate_kbps: arg_u64!(args, "audio_bitrate_kbps", 192) as u32,
            audio_channel_layout: arg_str!(args, "audio_channel_layout", "stereo"),
            reply: tx,
        },

        "delete_export_preset" => McpCommand::DeleteExportPreset {
            name: arg_str!(args, "name"),
            reply: tx,
        },

        "list_workspace_layouts" => McpCommand::ListWorkspaceLayouts { reply: tx },

        "save_workspace_layout" => McpCommand::SaveWorkspaceLayout {
            name: arg_str!(args, "name"),
            reply: tx,
        },

        "apply_workspace_layout" => McpCommand::ApplyWorkspaceLayout {
            name: arg_str!(args, "name"),
            reply: tx,
        },

        "rename_workspace_layout" => McpCommand::RenameWorkspaceLayout {
            old_name: arg_str!(args, "old_name"),
            new_name: arg_str!(args, "new_name"),
            reply: tx,
        },

        "delete_workspace_layout" => McpCommand::DeleteWorkspaceLayout {
            name: arg_str!(args, "name"),
            reply: tx,
        },

        "reset_workspace_layout" => McpCommand::ResetWorkspaceLayout { reply: tx },

        "export_with_preset" => McpCommand::ExportWithPreset {
            path: arg_str!(args, "path"),
            preset_name: arg_str!(args, "preset_name"),
            reply: tx,
        },

        "list_library" => McpCommand::ListLibrary { reply: tx },

        "import_media" => McpCommand::ImportMedia {
            path: arg_str!(args, "path"),
            reply: tx,
        },
        "relink_media" => McpCommand::RelinkMedia {
            root_path: arg_str!(args, "root_path"),
            reply: tx,
        },

        "create_bin" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let parent_id = args
                .get("parent_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            if name.is_empty() {
                return Err(tool_error_payload(-32602, "name is required"));
            }
            McpCommand::CreateBin {
                name,
                parent_id,
                reply: tx,
            }
        }
        "delete_bin" => {
            let bin_id = args
                .get("bin_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if bin_id.is_empty() {
                return Err(tool_error_payload(-32602, "bin_id is required"));
            }
            McpCommand::DeleteBin { bin_id, reply: tx }
        }
        "rename_bin" => {
            let bin_id = args
                .get("bin_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if bin_id.is_empty() || name.is_empty() {
                return Err(tool_error_payload(-32602, "bin_id and name are required"));
            }
            McpCommand::RenameBin {
                bin_id,
                name,
                reply: tx,
            }
        }
        "list_bins" => McpCommand::ListBins { reply: tx },
        "move_to_bin" => {
            let source_paths: Vec<String> = args
                .get("source_paths")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            if source_paths.is_empty() {
                return Err(tool_error_payload(-32602, "source_paths is required"));
            }
            let bin_id = args
                .get("bin_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            McpCommand::MoveToBin {
                source_paths,
                bin_id,
                reply: tx,
            }
        }
        "list_collections" => McpCommand::ListCollections { reply: tx },
        "create_collection" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if name.is_empty() {
                return Err(tool_error_payload(-32602, "name is required"));
            }
            McpCommand::CreateCollection {
                name,
                search_text: args
                    .get("search_text")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                kind: args
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                resolution: args
                    .get("resolution")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                frame_rate: args
                    .get("frame_rate")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                rating: args
                    .get("rating")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                reply: tx,
            }
        }
        "update_collection" => {
            let collection_id = args
                .get("collection_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if collection_id.is_empty() {
                return Err(tool_error_payload(-32602, "collection_id is required"));
            }
            McpCommand::UpdateCollection {
                collection_id,
                name: args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                search_text: args
                    .get("search_text")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                kind: args
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                resolution: args
                    .get("resolution")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                frame_rate: args
                    .get("frame_rate")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                rating: args
                    .get("rating")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                reply: tx,
            }
        }
        "delete_collection" => {
            let collection_id = args
                .get("collection_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if collection_id.is_empty() {
                return Err(tool_error_payload(-32602, "collection_id is required"));
            }
            McpCommand::DeleteCollection {
                collection_id,
                reply: tx,
            }
        }
        "set_media_rating" => {
            let library_key = args
                .get("library_key")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let rating = args
                .get("rating")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if library_key.is_empty() || rating.is_empty() {
                return Err(tool_error_payload(
                    -32602,
                    "library_key and rating are required",
                ));
            }
            McpCommand::SetMediaRating {
                library_key,
                rating,
                reply: tx,
            }
        }
        "add_media_keyword_range" => {
            let library_key = args
                .get("library_key")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let label = args
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if library_key.is_empty() || label.is_empty() {
                return Err(tool_error_payload(
                    -32602,
                    "library_key and label are required",
                ));
            }
            McpCommand::AddMediaKeywordRange {
                library_key,
                label,
                start_ns: args.get("start_ns").and_then(|v| v.as_u64()).unwrap_or(0),
                end_ns: args.get("end_ns").and_then(|v| v.as_u64()).unwrap_or(0),
                reply: tx,
            }
        }
        "update_media_keyword_range" => {
            let library_key = args
                .get("library_key")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let range_id = args
                .get("range_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let label = args
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if library_key.is_empty() || range_id.is_empty() || label.is_empty() {
                return Err(tool_error_payload(
                    -32602,
                    "library_key, range_id, and label are required",
                ));
            }
            McpCommand::UpdateMediaKeywordRange {
                library_key,
                range_id,
                label,
                start_ns: args.get("start_ns").and_then(|v| v.as_u64()).unwrap_or(0),
                end_ns: args.get("end_ns").and_then(|v| v.as_u64()).unwrap_or(0),
                reply: tx,
            }
        }
        "delete_media_keyword_range" => {
            let library_key = args
                .get("library_key")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let range_id = args
                .get("range_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if library_key.is_empty() || range_id.is_empty() {
                return Err(tool_error_payload(
                    -32602,
                    "library_key and range_id are required",
                ));
            }
            McpCommand::DeleteMediaKeywordRange {
                library_key,
                range_id,
                reply: tx,
            }
        }

        "reorder_track" => McpCommand::ReorderTrack {
            from_index: arg_u64!(args, "from_index", 0) as usize,
            to_index: arg_u64!(args, "to_index", 0) as usize,
            reply: tx,
        },
        "set_transition" => McpCommand::SetTransition {
            track_index: arg_u64!(args, "track_index", 0) as usize,
            clip_index: arg_u64!(args, "clip_index", 0) as usize,
            kind: arg_str!(args, "kind"),
            duration_ns: arg_u64!(args, "duration_ns", 0),
            alignment: args["alignment"]
                .as_str()
                .unwrap_or("end_on_cut")
                .to_string(),
            reply: tx,
        },
        "set_proxy_mode" => McpCommand::SetProxyMode {
            mode: arg_str!(args, "mode", "off"),
            reply: tx,
        },
        "set_proxy_sidecar_persistence" => McpCommand::SetProxySidecarPersistence {
            enabled: arg_bool!(args, "enabled"),
            reply: tx,
        },
        "set_clip_lut" => McpCommand::SetClipLut {
            clip_id: arg_str!(args, "clip_id"),
            lut_paths: {
                // Accept: array of strings, single string, "lut_paths" key, or legacy "lut_path" key
                let raw = if !args["lut_paths"].is_null() {
                    &args["lut_paths"]
                } else {
                    &args["lut_path"]
                };
                match raw {
                    Value::Array(arr) => arr
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect(),
                    Value::String(s) => vec![s.clone()],
                    _ => Vec::new(),
                }
            },
            reply: tx,
        },
        "set_clip_transform" => McpCommand::SetClipTransform {
            clip_id: arg_str!(args, "clip_id"),
            scale: arg_f64!(args, "scale", 1.0),
            position_x: arg_f64!(args, "position_x", 0.0),
            position_y: arg_f64!(args, "position_y", 0.0),
            rotate: args["rotate"].as_i64().map(|v| v as i32),
            anamorphic_desqueeze: args["anamorphic_desqueeze"].as_f64(),
            reply: tx,
        },
        "set_clip_opacity" => McpCommand::SetClipOpacity {
            clip_id: arg_str!(args, "clip_id"),
            opacity: arg_f64!(args, "opacity", 1.0),
            reply: tx,
        },
        "set_clip_voice_isolation" => McpCommand::SetClipVoiceIsolation {
            clip_id: arg_str!(args, "clip_id"),
            voice_isolation: arg_f64!(args, "voice_isolation", 0.0),
            reply: tx,
        },
        "set_clip_voice_enhance" => McpCommand::SetClipVoiceEnhance {
            clip_id: arg_str!(args, "clip_id"),
            enabled: args
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            strength: args.get("strength").and_then(|v| v.as_f64()),
            reply: tx,
        },
        "set_clip_subtitle_visible" => McpCommand::SetClipSubtitleVisible {
            clip_id: arg_str!(args, "clip_id"),
            visible: args
                .get("visible")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            reply: tx,
        },
        "set_voice_isolation_source" => McpCommand::SetVoiceIsolationSource {
            clip_id: arg_str!(args, "clip_id"),
            source: arg_str!(args, "source", "subtitles"),
            reply: tx,
        },
        "set_voice_isolation_silence_params" => McpCommand::SetVoiceIsolationSilenceParams {
            clip_id: arg_str!(args, "clip_id"),
            threshold_db: args.get("threshold_db").and_then(|v| v.as_f64()),
            min_ms: args
                .get("min_ms")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            reply: tx,
        },
        "suggest_voice_isolation_threshold" => McpCommand::SuggestVoiceIsolationThreshold {
            clip_id: arg_str!(args, "clip_id"),
            reply: tx,
        },
        "analyze_voice_isolation_silence" => McpCommand::AnalyzeVoiceIsolationSilence {
            clip_id: arg_str!(args, "clip_id"),
            reply: tx,
        },
        "set_clip_eq" => McpCommand::SetClipEq {
            clip_id: arg_str!(args, "clip_id"),
            low_freq: args.get("low_freq").and_then(|v| v.as_f64()),
            low_gain: args.get("low_gain").and_then(|v| v.as_f64()),
            low_q: args.get("low_q").and_then(|v| v.as_f64()),
            mid_freq: args.get("mid_freq").and_then(|v| v.as_f64()),
            mid_gain: args.get("mid_gain").and_then(|v| v.as_f64()),
            mid_q: args.get("mid_q").and_then(|v| v.as_f64()),
            high_freq: args.get("high_freq").and_then(|v| v.as_f64()),
            high_gain: args.get("high_gain").and_then(|v| v.as_f64()),
            high_q: args.get("high_q").and_then(|v| v.as_f64()),
            reply: tx,
        },
        "normalize_clip_audio" => McpCommand::NormalizeClipAudio {
            clip_id: arg_str!(args, "clip_id"),
            mode: args
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("lufs")
                .to_string(),
            target_level: args
                .get("target_level")
                .and_then(|v| v.as_f64())
                .unwrap_or(-14.0),
            reply: tx,
        },
        "analyze_project_loudness" => McpCommand::AnalyzeProjectLoudness { reply: tx },
        "set_project_master_gain_db" => McpCommand::SetProjectMasterGainDb {
            master_gain_db: args
                .get("master_gain_db")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
            reply: tx,
        },
        "match_clip_audio" => McpCommand::MatchClipAudio {
            source_clip_id: arg_str!(args, "source_clip_id"),
            source_start_ns: args.get("source_start_ns").and_then(|v| v.as_u64()),
            source_end_ns: args.get("source_end_ns").and_then(|v| v.as_u64()),
            source_channel_mode: crate::media::audio_match::AudioMatchChannelMode::from_str(
                args.get("source_channel_mode")
                    .and_then(|v| v.as_str())
                    .unwrap_or("auto"),
            ),
            reference_clip_id: arg_str!(args, "reference_clip_id"),
            reference_start_ns: args.get("reference_start_ns").and_then(|v| v.as_u64()),
            reference_end_ns: args.get("reference_end_ns").and_then(|v| v.as_u64()),
            reference_channel_mode: crate::media::audio_match::AudioMatchChannelMode::from_str(
                args.get("reference_channel_mode")
                    .and_then(|v| v.as_str())
                    .unwrap_or("auto"),
            ),
            reply: tx,
        },
        "clear_match_eq" => McpCommand::ClearMatchEq {
            clip_id: arg_str!(args, "clip_id"),
            reply: tx,
        },
        "detect_scene_cuts" => McpCommand::DetectSceneCuts {
            clip_id: arg_str!(args, "clip_id"),
            track_id: arg_str!(args, "track_id"),
            threshold: args
                .get("threshold")
                .and_then(|v| v.as_f64())
                .unwrap_or(10.0),
            reply: tx,
        },
        "generate_music" => McpCommand::GenerateMusic {
            prompt: arg_str!(args, "prompt"),
            duration_secs: args
                .get("duration_secs")
                .and_then(|v| v.as_f64())
                .unwrap_or(10.0)
                .clamp(1.0, 30.0),
            track_index: args
                .get("track_index")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize),
            timeline_start_ns: args.get("timeline_start_ns").and_then(|v| v.as_u64()),
            reference_audio_path: args
                .get("reference_audio_path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            reply: tx,
        },
        "record_voiceover" => McpCommand::RecordVoiceover {
            duration_ns: arg_u64!(args, "duration_ns", 0),
            track_index: args
                .get("track_index")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize),
            reply: tx,
        },
        "set_clip_blend_mode" => McpCommand::SetClipBlendMode {
            clip_id: arg_str!(args, "clip_id"),
            blend_mode: arg_str!(args, "blend_mode", "normal"),
            reply: tx,
        },
        "set_clip_keyframe" => McpCommand::SetClipKeyframe {
            clip_id: arg_str!(args, "clip_id"),
            property: arg_str!(args, "property"),
            timeline_pos_ns: args.get("timeline_pos_ns").and_then(|v| v.as_u64()),
            value: arg_f64!(args, "value", 0.0),
            interpolation: args
                .get("interpolation")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            bezier_controls: args.get("bezier_controls").and_then(|v| {
                let obj = v.as_object()?;
                let x1 = obj.get("x1")?.as_f64()?;
                let y1 = obj.get("y1")?.as_f64()?;
                let x2 = obj.get("x2")?.as_f64()?;
                let y2 = obj.get("y2")?.as_f64()?;
                Some((x1, y1, x2, y2))
            }),
            reply: tx,
        },
        "remove_clip_keyframe" => McpCommand::RemoveClipKeyframe {
            clip_id: arg_str!(args, "clip_id"),
            property: arg_str!(args, "property"),
            timeline_pos_ns: args.get("timeline_pos_ns").and_then(|v| v.as_u64()),
            reply: tx,
        },

        "create_project" => McpCommand::CreateProject {
            title: arg_str!(args, "title", "Untitled"),
            reply: tx,
        },

        "set_gsk_renderer" => McpCommand::SetGskRenderer {
            renderer: arg_str!(args, "renderer", "auto"),
            reply: tx,
        },

        "set_preview_quality" => McpCommand::SetPreviewQuality {
            quality: arg_str!(args, "quality", "full"),
            reply: tx,
        },

        "set_realtime_preview" => McpCommand::SetRealtimePreview {
            enabled: arg_bool!(args, "enabled"),
            reply: tx,
        },

        "set_experimental_preview_optimizations" => {
            McpCommand::SetExperimentalPreviewOptimizations {
                enabled: arg_bool!(args, "enabled"),
                reply: tx,
            }
        }

        "set_background_prerender" => McpCommand::SetBackgroundPrerender {
            enabled: arg_bool!(args, "enabled"),
            reply: tx,
        },
        "set_prerender_quality" => {
            let (preset, crf) = match parse_prerender_quality_args(&args) {
                Ok(parsed) => parsed,
                Err(message) => return Err(tool_error_payload(-32602, message)),
            };
            McpCommand::SetPrerenderQuality {
                preset: preset.to_string(),
                crf,
                reply: tx,
            }
        }
        "set_prerender_project_persistence" => McpCommand::SetPrerenderProjectPersistence {
            enabled: arg_bool!(args, "enabled"),
            reply: tx,
        },
        "set_preview_luts" => McpCommand::SetPreviewLuts {
            enabled: arg_bool!(args, "enabled"),
            reply: tx,
        },

        "insert_clip" => McpCommand::InsertClip {
            source_path: arg_str!(args, "source_path"),
            source_in_ns: arg_u64!(args, "source_in_ns", 0),
            source_out_ns: arg_u64!(args, "source_out_ns", 0),
            track_index: args["track_index"].as_u64().map(|v| v as usize),
            timeline_pos_ns: args["timeline_pos_ns"].as_u64(),
            reply: tx,
        },

        "overwrite_clip" => McpCommand::OverwriteClip {
            source_path: arg_str!(args, "source_path"),
            source_in_ns: arg_u64!(args, "source_in_ns", 0),
            source_out_ns: arg_u64!(args, "source_out_ns", 0),
            track_index: args["track_index"].as_u64().map(|v| v as usize),
            timeline_pos_ns: args["timeline_pos_ns"].as_u64(),
            reply: tx,
        },

        "play" => McpCommand::Play { reply: tx },
        "pause" => McpCommand::Pause { reply: tx },
        "stop" => McpCommand::Stop { reply: tx },
        "seek_playhead" => McpCommand::SeekPlayhead {
            timeline_pos_ns: arg_u64!(args, "timeline_pos_ns", 0),
            reply: tx,
        },
        "export_displayed_frame" => McpCommand::ExportDisplayedFrame {
            path: arg_str!(args, "path"),
            reply: tx,
        },
        "export_timeline_snapshot" => McpCommand::ExportTimelineSnapshot {
            path: arg_str!(args, "path"),
            width: arg_u64!(args, "width", 1920) as u32,
            height: arg_u64!(args, "height", 1080) as u32,
            reply: tx,
        },
        "take_screenshot" => McpCommand::TakeScreenshot { reply: tx },
        "select_library_item" => McpCommand::SelectLibraryItem {
            path: arg_str!(args, "path"),
            reply: tx,
        },
        "source_play" => McpCommand::SourcePlay { reply: tx },
        "source_pause" => McpCommand::SourcePause { reply: tx },
        "match_frame" => McpCommand::MatchFrame {
            clip_id: args
                .get("clip_id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            reply: tx,
        },
        "list_backups" => McpCommand::ListBackups { reply: tx },
        "list_project_snapshots" => McpCommand::ListProjectSnapshots { reply: tx },
        "create_project_snapshot" => McpCommand::CreateProjectSnapshot {
            name: arg_str!(args, "name"),
            reply: tx,
        },
        "restore_project_snapshot" => McpCommand::RestoreProjectSnapshot {
            snapshot_id: arg_str!(args, "snapshot_id"),
            reply: tx,
        },
        "delete_project_snapshot" => McpCommand::DeleteProjectSnapshot {
            snapshot_id: arg_str!(args, "snapshot_id"),
            reply: tx,
        },
        "set_clip_stabilization" => McpCommand::SetClipStabilization {
            clip_id: arg_str!(args, "clip_id"),
            enabled: args
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            smoothing: args
                .get("smoothing")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.5),
            reply: tx,
        },
        "set_clip_auto_crop_track" => McpCommand::SetClipAutoCropTrack {
            clip_id: arg_str!(args, "clip_id"),
            center_x: args
                .get("center_x")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.5),
            center_y: args
                .get("center_y")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.5),
            width: args
                .get("width")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.25),
            height: args
                .get("height")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.25),
            padding: args.get("padding").and_then(|v| v.as_f64()),
            reply: tx,
        },
        "sync_clips_by_audio" => McpCommand::SyncClipsByAudio {
            clip_ids: args["clip_ids"]
                .as_array()
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| id.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            replace_audio: args
                .get("replace_audio")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            reply: tx,
        },
        "copy_clip_color_grade" => McpCommand::CopyClipColorGrade {
            clip_id: arg_str!(args, "clip_id"),
            reply: tx,
        },
        "paste_clip_color_grade" => McpCommand::PasteClipColorGrade {
            clip_id: arg_str!(args, "clip_id"),
            reply: tx,
        },
        "match_clip_colors" => McpCommand::MatchClipColors {
            source_clip_id: arg_str!(args, "source_clip_id"),
            reference_clip_id: arg_str!(args, "reference_clip_id"),
            generate_lut: args
                .get("generate_lut")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            reply: tx,
        },
        "list_frei0r_plugins" => McpCommand::ListFrei0rPlugins { reply: tx },
        "list_clip_frei0r_effects" => McpCommand::ListClipFrei0rEffects {
            clip_id: arg_str!(args, "clip_id"),
            reply: tx,
        },
        "add_clip_frei0r_effect" => McpCommand::AddClipFrei0rEffect {
            clip_id: arg_str!(args, "clip_id"),
            plugin_name: arg_str!(args, "plugin_name"),
            params: args.get("params").and_then(|v| v.as_object()).map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f)))
                    .collect()
            }),
            string_params: args
                .get("string_params")
                .and_then(|v| v.as_object())
                .map(|obj| {
                    obj.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                }),
            reply: tx,
        },
        "remove_clip_frei0r_effect" => McpCommand::RemoveClipFrei0rEffect {
            clip_id: arg_str!(args, "clip_id"),
            effect_id: arg_str!(args, "effect_id"),
            reply: tx,
        },
        "set_clip_frei0r_effect_params" => McpCommand::SetClipFrei0rEffectParams {
            clip_id: arg_str!(args, "clip_id"),
            effect_id: arg_str!(args, "effect_id"),
            params: args
                .get("params")
                .and_then(|v| v.as_object())
                .map(|obj| {
                    obj.iter()
                        .filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f)))
                        .collect()
                })
                .unwrap_or_default(),
            string_params: args
                .get("string_params")
                .and_then(|v| v.as_object())
                .map(|obj| {
                    obj.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                }),
            reply: tx,
        },
        "reorder_clip_frei0r_effects" => McpCommand::ReorderClipFrei0rEffects {
            clip_id: arg_str!(args, "clip_id"),
            effect_ids: args["effect_ids"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            reply: tx,
        },
        "add_title_clip" => McpCommand::AddTitleClip {
            template_id: arg_str!(args, "template_id"),
            track_index: args["track_index"].as_u64().map(|v| v as usize),
            timeline_start_ns: args["timeline_start_ns"].as_u64(),
            duration_ns: args["duration_ns"].as_u64(),
            title_text: args["title_text"].as_str().map(String::from),
            reply: tx,
        },
        "add_adjustment_layer" => McpCommand::AddAdjustmentLayer {
            track_index: arg_u64!(args, "track_index", 0) as usize,
            timeline_start_ns: arg_u64!(args, "timeline_start_ns", 0),
            duration_ns: arg_u64!(args, "duration_ns", 5_000_000_000),
            reply: tx,
        },
        "set_clip_title_style" => McpCommand::SetClipTitleStyle {
            clip_id: arg_str!(args, "clip_id"),
            title_text: args["title_text"].as_str().map(String::from),
            title_font: args["title_font"].as_str().map(String::from),
            title_color: args["title_color"].as_u64().map(|v| v as u32),
            title_x: args["title_x"].as_f64(),
            title_y: args["title_y"].as_f64(),
            title_outline_width: args["title_outline_width"].as_f64(),
            title_outline_color: args["title_outline_color"].as_u64().map(|v| v as u32),
            title_shadow: args["title_shadow"].as_bool(),
            title_shadow_color: args["title_shadow_color"].as_u64().map(|v| v as u32),
            title_shadow_offset_x: args["title_shadow_offset_x"].as_f64(),
            title_shadow_offset_y: args["title_shadow_offset_y"].as_f64(),
            title_bg_box: args["title_bg_box"].as_bool(),
            title_bg_box_color: args["title_bg_box_color"].as_u64().map(|v| v as u32),
            title_bg_box_padding: args["title_bg_box_padding"].as_f64(),
            title_clip_bg_color: args["title_clip_bg_color"].as_u64().map(|v| v as u32),
            title_secondary_text: args["title_secondary_text"].as_str().map(String::from),
            reply: tx,
        },
        "add_to_export_queue" => McpCommand::AddToExportQueue {
            output_path: arg_str!(args, "output_path"),
            preset_name: args["preset_name"].as_str().map(String::from),
            reply: tx,
        },
        "list_export_queue" => McpCommand::ListExportQueue { reply: tx },
        "clear_export_queue" => McpCommand::ClearExportQueue {
            status_filter: args["status_filter"].as_str().map(String::from),
            reply: tx,
        },
        "run_export_queue" => McpCommand::RunExportQueue { reply: tx },
        "create_compound_clip" => {
            let clip_ids: Vec<String> = args["clip_ids"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            McpCommand::CreateCompoundClip {
                clip_ids,
                reply: tx,
            }
        }
        "break_apart_compound_clip" => McpCommand::BreakApartCompoundClip {
            clip_id: arg_str!(args, "clip_id"),
            reply: tx,
        },
        "create_multicam_clip" => {
            let clip_ids: Vec<String> = args["clip_ids"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            McpCommand::CreateMulticamClip {
                clip_ids,
                reply: tx,
            }
        }
        "add_angle_switch" => McpCommand::AddAngleSwitch {
            clip_id: arg_str!(args, "clip_id"),
            position_ns: arg_u64!(args, "position_ns", 0),
            angle_index: arg_u64!(args, "angle_index", 0) as usize,
            reply: tx,
        },
        "list_multicam_angles" => McpCommand::ListMulticamAngles {
            clip_id: arg_str!(args, "clip_id"),
            reply: tx,
        },
        "set_multicam_angle_audio" => McpCommand::SetMulticamAngleAudio {
            clip_id: arg_str!(args, "clip_id"),
            angle_index: arg_u64!(args, "angle_index", 0) as usize,
            volume: args["volume"].as_f64().map(|v| v as f32),
            muted: args["muted"].as_bool(),
            reply: tx,
        },
        "set_multicam_angle_color" => McpCommand::SetMulticamAngleColor {
            clip_id: arg_str!(args, "clip_id"),
            angle_index: arg_u64!(args, "angle_index", 0) as usize,
            brightness: args["brightness"].as_f64().map(|v| v as f32),
            contrast: args["contrast"].as_f64().map(|v| v as f32),
            saturation: args["saturation"].as_f64().map(|v| v as f32),
            temperature: args["temperature"].as_f64().map(|v| v as f32),
            tint: args["tint"].as_f64().map(|v| v as f32),
            lut_paths: args["lut_paths"].as_array().map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            }),
            reply: tx,
        },
        // ── Audition / clip-versions tools ────────────────────────────────
        "create_audition_clip" => {
            let clip_ids: Vec<String> = args["clip_ids"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            McpCommand::CreateAuditionClip {
                clip_ids,
                active_index: arg_u64!(args, "active_index", 0) as usize,
                reply: tx,
            }
        }
        "add_audition_take" => McpCommand::AddAuditionTake {
            audition_clip_id: arg_str!(args, "audition_clip_id"),
            source_path: arg_str!(args, "source_path"),
            source_in_ns: arg_u64!(args, "source_in_ns", 0),
            source_out_ns: arg_u64!(args, "source_out_ns", 0),
            label: args["label"].as_str().map(String::from),
            reply: tx,
        },
        "remove_audition_take" => McpCommand::RemoveAuditionTake {
            audition_clip_id: arg_str!(args, "audition_clip_id"),
            take_index: arg_u64!(args, "take_index", 0) as usize,
            reply: tx,
        },
        "set_active_audition_take" => McpCommand::SetActiveAuditionTake {
            audition_clip_id: arg_str!(args, "audition_clip_id"),
            take_index: arg_u64!(args, "take_index", 0) as usize,
            reply: tx,
        },
        "list_audition_takes" => McpCommand::ListAuditionTakes {
            audition_clip_id: arg_str!(args, "audition_clip_id"),
            reply: tx,
        },
        "finalize_audition" => McpCommand::FinalizeAudition {
            audition_clip_id: arg_str!(args, "audition_clip_id"),
            reply: tx,
        },
        // ── Subtitle / STT tools ──────────────────────────────────────────
        "generate_subtitles" => McpCommand::GenerateSubtitles {
            clip_id: arg_str!(args, "clip_id"),
            language: arg_str!(args, "language", "auto"),
            reply: tx,
        },
        "get_clip_subtitles" => McpCommand::GetClipSubtitles {
            clip_id: arg_str!(args, "clip_id"),
            reply: tx,
        },
        "edit_subtitle_text" => McpCommand::EditSubtitleText {
            clip_id: arg_str!(args, "clip_id"),
            segment_id: arg_str!(args, "segment_id"),
            text: arg_str!(args, "text"),
            reply: tx,
        },
        "edit_subtitle_timing" => McpCommand::EditSubtitleTiming {
            clip_id: arg_str!(args, "clip_id"),
            segment_id: arg_str!(args, "segment_id"),
            start_ns: arg_u64!(args, "start_ns", 0),
            end_ns: arg_u64!(args, "end_ns", 0),
            reply: tx,
        },
        "clear_subtitles" => McpCommand::ClearSubtitles {
            clip_id: arg_str!(args, "clip_id"),
            reply: tx,
        },
        "delete_transcript_range" => McpCommand::DeleteTranscriptRange {
            clip_id: arg_str!(args, "clip_id"),
            start_word_index: args["start_word_index"].as_u64().unwrap_or(0) as u32,
            end_word_index: args["end_word_index"].as_u64().unwrap_or(0) as u32,
            reply: tx,
        },
        "set_subtitle_style" => McpCommand::SetSubtitleStyle {
            clip_id: arg_str!(args, "clip_id"),
            font: args["font"].as_str().map(String::from),
            color: args["color"].as_u64().map(|v| v as u32),
            outline_color: args["outline_color"].as_u64().map(|v| v as u32),
            outline_width: args["outline_width"].as_f64(),
            bg_box: args["bg_box"].as_bool(),
            bg_box_color: args["bg_box_color"].as_u64().map(|v| v as u32),
            highlight_mode: args["highlight_mode"].as_str().map(String::from),
            highlight_color: args["highlight_color"].as_u64().map(|v| v as u32),
            bold: args["bold"].as_bool(),
            italic: args["italic"].as_bool(),
            underline: args["underline"].as_bool(),
            shadow: args["shadow"].as_bool(),
            highlight_bold: args["highlight_bold"].as_bool(),
            highlight_color_flag: args["highlight_color_flag"].as_bool(),
            highlight_underline: args["highlight_underline"].as_bool(),
            highlight_stroke: args["highlight_stroke"].as_bool(),
            highlight_italic: args["highlight_italic"].as_bool(),
            highlight_background: args["highlight_background"].as_bool(),
            highlight_shadow: args["highlight_shadow"].as_bool(),
            bg_highlight_color: args["bg_highlight_color"].as_u64().map(|v| v as u32),
            highlight_stroke_color: args["highlight_stroke_color"]
                .as_u64()
                .map(|v| v as u32),
            reply: tx,
        },
        "export_srt" => McpCommand::ExportSrt {
            path: arg_str!(args, "path"),
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
        let stop_on_error = arg_bool!(args, "stop_on_error");
        let include_timing = arg_bool!(args, "include_timing");
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

fn parse_ltc_channel_arg(
    value: Option<&Value>,
) -> Result<crate::media::ltc::LtcChannelSelection, &'static str> {
    let Some(value) = value else {
        return Ok(crate::media::ltc::LtcChannelSelection::Auto);
    };
    let Some(value) = value.as_str() else {
        return Err("ltc_channel must be a string");
    };
    crate::media::ltc::LtcChannelSelection::from_str(value)
        .ok_or("ltc_channel must be auto, left, right, or mono_mix")
}

fn parse_ltc_frame_rate_arg(value: Option<&Value>) -> Result<Option<FrameRate>, &'static str> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(value) = value.as_str() else {
        return Err("frame_rate must be a string");
    };
    let trimmed = value.trim().to_ascii_lowercase();
    if trimmed.is_empty() || matches!(trimmed.as_str(), "auto" | "default" | "project") {
        return Ok(None);
    }

    let parsed = match trimmed.as_str() {
        "23.976" | "23.98" | "24000/1001" => Some(FrameRate {
            numerator: 24_000,
            denominator: 1_001,
        }),
        "24" | "24/1" => Some(FrameRate {
            numerator: 24,
            denominator: 1,
        }),
        "25" | "25/1" => Some(FrameRate {
            numerator: 25,
            denominator: 1,
        }),
        "29.97" | "30000/1001" => Some(FrameRate {
            numerator: 30_000,
            denominator: 1_001,
        }),
        "30" | "30/1" => Some(FrameRate {
            numerator: 30,
            denominator: 1,
        }),
        _ => {
            if let Some((numerator, denominator)) = trimmed.split_once('/') {
                let numerator = numerator.parse::<u32>().ok();
                let denominator = denominator.parse::<u32>().ok();
                numerator
                    .zip(denominator)
                    .filter(|(numerator, denominator)| *numerator > 0 && *denominator > 0)
                    .map(|(numerator, denominator)| FrameRate {
                        numerator,
                        denominator,
                    })
            } else {
                None
            }
        }
    };

    parsed
        .ok_or("frame_rate must be one of 23.976, 24, 25, 29.97, 30, or a fraction like 24000/1001")
        .map(Some)
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

fn parse_prerender_quality_args(args: &Value) -> Result<(&'static str, u32), &'static str> {
    let preset = match args.get("preset").and_then(Value::as_str) {
        Some("ultrafast") => "ultrafast",
        Some("superfast") => "superfast",
        Some("veryfast") => "veryfast",
        Some("faster") => "faster",
        Some("fast") => "fast",
        Some("medium") => "medium",
        Some(_) => {
            return Err(
                "preset must be one of: ultrafast, superfast, veryfast, faster, fast, medium",
            );
        }
        None => return Err("preset is required"),
    };
    let crf_u64 = match args.get("crf").and_then(Value::as_u64) {
        Some(crf) => crf,
        None => return Err("crf must be an integer"),
    };
    if crf_u64 > crate::ui_state::MAX_PRERENDER_CRF as u64 {
        return Err("crf must be between 0 and 51");
    }
    Ok((preset, crf_u64 as u32))
}

#[cfg(test)]
mod tests {
    use super::{call_tool, parse_crossfade_settings_args, parse_prerender_quality_args};
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
    fn parse_prerender_quality_accepts_valid_args() {
        let args = json!({"preset": "fast", "crf": 18});
        assert_eq!(parse_prerender_quality_args(&args), Ok(("fast", 18)));
    }

    #[test]
    fn parse_prerender_quality_rejects_invalid_args() {
        assert_eq!(
            parse_prerender_quality_args(&json!({"preset": "turbo", "crf": 20})),
            Err("preset must be one of: ultrafast, superfast, veryfast, faster, fast, medium")
        );
        assert_eq!(
            parse_prerender_quality_args(&json!({"preset": "fast", "crf": 52})),
            Err("crf must be between 0 and 51")
        );
        assert_eq!(
            parse_prerender_quality_args(&json!({"preset": "fast", "crf": 19.5})),
            Err("crf must be an integer")
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
    fn call_tool_dispatches_get_performance_snapshot() {
        let (sender, receiver) = std::sync::mpsc::channel::<McpCommand>();
        std::thread::spawn(move || {
            let cmd = receiver.recv().expect("expected command");
            match cmd {
                McpCommand::GetPerformanceSnapshot { reply } => {
                    reply
                        .send(json!({
                            "player_state": "paused",
                            "prerender_pending": 0
                        }))
                        .ok();
                }
                _ => panic!("unexpected MCP command"),
            }
        });
        let id = json!(8);
        let params = json!({
            "name": "get_performance_snapshot",
            "arguments": {}
        });
        let mut cache = std::collections::HashMap::new();
        let response = call_tool(&id, &params, &sender, &mut cache);
        assert_eq!(response["id"], id);
        assert_eq!(response["error"], serde_json::Value::Null);
    }

    #[test]
    fn call_tool_dispatches_match_clip_audio() {
        let (sender, receiver) = std::sync::mpsc::channel::<McpCommand>();
        std::thread::spawn(move || {
            let cmd = receiver.recv().expect("expected command");
            match cmd {
                McpCommand::MatchClipAudio {
                    source_clip_id,
                    source_start_ns,
                    source_end_ns,
                    source_channel_mode,
                    reference_clip_id,
                    reference_start_ns,
                    reference_end_ns,
                    reference_channel_mode,
                    reply,
                } => {
                    assert_eq!(source_clip_id, "clip-1");
                    assert_eq!(source_start_ns, Some(1_000_000_000));
                    assert_eq!(source_end_ns, Some(3_000_000_000));
                    assert_eq!(
                        source_channel_mode,
                        crate::media::audio_match::AudioMatchChannelMode::Left
                    );
                    assert_eq!(reference_clip_id, "clip-2");
                    assert_eq!(reference_start_ns, Some(2_000_000_000));
                    assert_eq!(reference_end_ns, Some(4_000_000_000));
                    assert_eq!(
                        reference_channel_mode,
                        crate::media::audio_match::AudioMatchChannelMode::Right
                    );
                    reply
                        .send(json!({
                            "success": true,
                            "source_clip_id": source_clip_id,
                            "reference_clip_id": reference_clip_id
                        }))
                        .ok();
                }
                _ => panic!("unexpected MCP command"),
            }
        });
        let id = json!(9);
        let params = json!({
            "name": "match_clip_audio",
            "arguments": {
                "source_clip_id": "clip-1",
                "source_start_ns": 1_000_000_000u64,
                "source_end_ns": 3_000_000_000u64,
                "source_channel_mode": "left",
                "reference_clip_id": "clip-2",
                "reference_start_ns": 2_000_000_000u64,
                "reference_end_ns": 4_000_000_000u64,
                "reference_channel_mode": "right"
            }
        });
        let mut cache = std::collections::HashMap::new();
        let response = call_tool(&id, &params, &sender, &mut cache);
        assert_eq!(response["id"], id);
        assert_eq!(response["error"], serde_json::Value::Null);
    }

    #[test]
    fn call_tool_dispatches_list_project_snapshots() {
        let (sender, receiver) = std::sync::mpsc::channel::<McpCommand>();
        std::thread::spawn(move || {
            let cmd = receiver.recv().expect("expected command");
            match cmd {
                McpCommand::ListProjectSnapshots { reply } => {
                    reply
                        .send(json!({"ok": true, "snapshots": [], "count": 0}))
                        .ok();
                }
                _ => panic!("unexpected MCP command"),
            }
        });
        let id = json!(10);
        let params = json!({
            "name": "list_project_snapshots",
            "arguments": {}
        });
        let mut cache = std::collections::HashMap::new();
        let response = call_tool(&id, &params, &sender, &mut cache);
        assert_eq!(response["id"], id);
        assert_eq!(response["error"], serde_json::Value::Null);
    }

    #[test]
    fn call_tool_dispatches_create_project_snapshot() {
        let (sender, receiver) = std::sync::mpsc::channel::<McpCommand>();
        std::thread::spawn(move || {
            let cmd = receiver.recv().expect("expected command");
            match cmd {
                McpCommand::CreateProjectSnapshot { name, reply } => {
                    assert_eq!(name, "Before color");
                    reply
                        .send(json!({"ok": true, "snapshot": {"id": "snap-1", "name": name}}))
                        .ok();
                }
                _ => panic!("unexpected MCP command"),
            }
        });
        let id = json!(11);
        let params = json!({
            "name": "create_project_snapshot",
            "arguments": { "name": "Before color" }
        });
        let mut cache = std::collections::HashMap::new();
        let response = call_tool(&id, &params, &sender, &mut cache);
        assert_eq!(response["id"], id);
        assert_eq!(response["error"], serde_json::Value::Null);
    }

    #[test]
    fn call_tool_dispatches_restore_project_snapshot() {
        let (sender, receiver) = std::sync::mpsc::channel::<McpCommand>();
        std::thread::spawn(move || {
            let cmd = receiver.recv().expect("expected command");
            match cmd {
                McpCommand::RestoreProjectSnapshot { snapshot_id, reply } => {
                    assert_eq!(snapshot_id, "snap-1");
                    reply
                        .send(json!({"ok": true, "snapshot_id": snapshot_id, "dirty": true}))
                        .ok();
                }
                _ => panic!("unexpected MCP command"),
            }
        });
        let id = json!(12);
        let params = json!({
            "name": "restore_project_snapshot",
            "arguments": { "snapshot_id": "snap-1" }
        });
        let mut cache = std::collections::HashMap::new();
        let response = call_tool(&id, &params, &sender, &mut cache);
        assert_eq!(response["id"], id);
        assert_eq!(response["error"], serde_json::Value::Null);
    }

    #[test]
    fn call_tool_dispatches_delete_project_snapshot() {
        let (sender, receiver) = std::sync::mpsc::channel::<McpCommand>();
        std::thread::spawn(move || {
            let cmd = receiver.recv().expect("expected command");
            match cmd {
                McpCommand::DeleteProjectSnapshot { snapshot_id, reply } => {
                    assert_eq!(snapshot_id, "snap-1");
                    reply
                        .send(json!({"ok": true, "snapshot_id": snapshot_id}))
                        .ok();
                }
                _ => panic!("unexpected MCP command"),
            }
        });
        let id = json!(13);
        let params = json!({
            "name": "delete_project_snapshot",
            "arguments": { "snapshot_id": "snap-1" }
        });
        let mut cache = std::collections::HashMap::new();
        let response = call_tool(&id, &params, &sender, &mut cache);
        assert_eq!(response["id"], id);
        assert_eq!(response["error"], serde_json::Value::Null);
    }

    #[test]
    fn call_tool_dispatches_save_project_with_media() {
        let (sender, receiver) = std::sync::mpsc::channel::<McpCommand>();
        std::thread::spawn(move || {
            let cmd = receiver.recv().expect("expected command");
            match cmd {
                McpCommand::SaveProjectWithMedia { path, reply } => {
                    assert_eq!(path, "/tmp/packaged.uspxml");
                    reply
                        .send(json!({"success": true, "path": path, "library_path": "/tmp/packaged.Library"}))
                        .ok();
                }
                _ => panic!("unexpected MCP command"),
            }
        });
        let id = json!(9);
        let params = json!({
            "name": "save_project_with_media",
            "arguments": { "path": "/tmp/packaged.uspxml" }
        });
        let mut cache = std::collections::HashMap::new();
        let response = call_tool(&id, &params, &sender, &mut cache);
        assert_eq!(response["id"], id);
        assert_eq!(response["error"], serde_json::Value::Null);
    }

    #[test]
    fn call_tool_dispatches_collect_project_files() {
        let (sender, receiver) = std::sync::mpsc::channel::<McpCommand>();
        std::thread::spawn(move || {
            let cmd = receiver.recv().expect("expected command");
            match cmd {
                McpCommand::CollectProjectFiles {
                    directory_path,
                    mode,
                    use_collected_locations_on_next_save,
                    reply,
                } => {
                    assert_eq!(directory_path, "/tmp/collected");
                    assert_eq!(mode, crate::fcpxml::writer::CollectFilesMode::EntireLibrary);
                    assert!(use_collected_locations_on_next_save);
                    reply
                        .send(json!({
                            "success": true,
                            "directory_path": directory_path,
                            "mode": mode.as_str(),
                            "use_collected_locations_on_next_save": true,
                            "project_paths_updated": true,
                            "project_media_references_updated": 3,
                            "project_lut_references_updated": 1,
                            "library_items_updated": 2,
                            "media_files": 3,
                            "lut_files": 1,
                            "total_files": 4
                        }))
                        .ok();
                }
                _ => panic!("unexpected MCP command"),
            }
        });
        let id = json!(10);
        let params = json!({
            "name": "collect_project_files",
            "arguments": {
                "directory_path": "/tmp/collected",
                "mode": "entire_library",
                "use_collected_locations_on_next_save": true
            }
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

    #[test]
    fn call_tool_dispatches_save_workspace_layout() {
        let (sender, receiver) = std::sync::mpsc::channel::<McpCommand>();
        std::thread::spawn(move || {
            let cmd = receiver.recv().expect("expected command");
            match cmd {
                McpCommand::SaveWorkspaceLayout { name, reply } => {
                    assert_eq!(name, "Color");
                    reply.send(json!({"success": true, "name": name})).ok();
                }
                _ => panic!("unexpected MCP command"),
            }
        });
        let id = json!(401);
        let params = json!({
            "name": "save_workspace_layout",
            "arguments": { "name": "Color" }
        });
        let mut cache = std::collections::HashMap::new();
        let response = call_tool(&id, &params, &sender, &mut cache);
        assert_eq!(response["id"], id);
        assert_eq!(response["error"], serde_json::Value::Null);
    }

    #[test]
    fn call_tool_dispatches_apply_workspace_layout() {
        let (sender, receiver) = std::sync::mpsc::channel::<McpCommand>();
        std::thread::spawn(move || {
            let cmd = receiver.recv().expect("expected command");
            match cmd {
                McpCommand::ApplyWorkspaceLayout { name, reply } => {
                    assert_eq!(name, "Edit");
                    reply.send(json!({"success": true, "name": name})).ok();
                }
                _ => panic!("unexpected MCP command"),
            }
        });
        let id = json!(402);
        let params = json!({
            "name": "apply_workspace_layout",
            "arguments": { "name": "Edit" }
        });
        let mut cache = std::collections::HashMap::new();
        let response = call_tool(&id, &params, &sender, &mut cache);
        assert_eq!(response["id"], id);
        assert_eq!(response["error"], serde_json::Value::Null);
    }

    #[test]
    fn call_tool_dispatches_reset_workspace_layout() {
        let (sender, receiver) = std::sync::mpsc::channel::<McpCommand>();
        std::thread::spawn(move || {
            let cmd = receiver.recv().expect("expected command");
            match cmd {
                McpCommand::ResetWorkspaceLayout { reply } => {
                    reply.send(json!({"success": true})).ok();
                }
                _ => panic!("unexpected MCP command"),
            }
        });
        let id = json!(403);
        let params = json!({
            "name": "reset_workspace_layout",
            "arguments": {}
        });
        let mut cache = std::collections::HashMap::new();
        let response = call_tool(&id, &params, &sender, &mut cache);
        assert_eq!(response["id"], id);
        assert_eq!(response["error"], serde_json::Value::Null);
    }
}
