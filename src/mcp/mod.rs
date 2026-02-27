use std::sync::atomic::AtomicBool;
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use serde_json::Value;

pub mod server;

/// Commands sent from the MCP stdio thread to the GTK main thread.
/// Each variant carries a `reply` one-shot sync-channel for the result.
pub enum McpCommand {
    GetProject { reply: SyncSender<Value> },
    ListTracks  { reply: SyncSender<Value> },
    ListClips   { reply: SyncSender<Value> },
    GetTimelineSettings { reply: SyncSender<Value> },
    SetMagneticMode { enabled: bool, reply: SyncSender<Value> },
    CloseSourcePreview { reply: SyncSender<Value> },
    GetPreferences { reply: SyncSender<Value> },
    SetHardwareAcceleration { enabled: bool, reply: SyncSender<Value> },
    SetPlaybackPriority { priority: String, reply: SyncSender<Value> },
    SetProxyMode { mode: String, reply: SyncSender<Value> },
    AddClip {
        source_path:       String,
        track_index:       usize,
        timeline_start_ns: u64,
        source_in_ns:      u64,
        source_out_ns:     u64,
        reply:             SyncSender<Value>,
    },
    RemoveClip  { clip_id: String, reply: SyncSender<Value> },
    MoveClip    { clip_id: String, new_start_ns: u64, reply: SyncSender<Value> },
    TrimClip    { clip_id: String, source_in_ns: u64, source_out_ns: u64, reply: SyncSender<Value> },
    SetClipColor {
        clip_id:    String,
        brightness: f64,
        contrast:   f64,
        saturation: f64,
        denoise:    f64,
        sharpness:  f64,
        reply:      SyncSender<Value>,
    },
    SetTitle    { title: String, reply: SyncSender<Value> },
    OpenFcpxml  { path: String, reply: SyncSender<Value> },
    SaveFcpxml  { path: String, reply: SyncSender<Value> },
    ExportMp4   { path: String, reply: SyncSender<Value> },
    ListLibrary { reply: SyncSender<Value> },
    ImportMedia { path: String, reply: SyncSender<Value> },
    ReorderTrack { from_index: usize, to_index: usize, reply: SyncSender<Value> },
    SetTransition {
        track_index: usize,
        clip_index: usize,
        kind: String,
        duration_ns: u64,
        reply: SyncSender<Value>,
    },
    CreateProject { title: String, reply: SyncSender<Value> },
    SetClipLut {
        clip_id:  String,
        lut_path: Option<String>,
        reply:    SyncSender<Value>,
    },
    SetClipTransform {
        clip_id:    String,
        scale:      f64,
        position_x: f64,
        position_y: f64,
        reply:      SyncSender<Value>,
    },
    SetClipOpacity {
        clip_id: String,
        opacity: f64,
        reply:   SyncSender<Value>,
    },
    SetGskRenderer { renderer: String, reply: SyncSender<Value> },
    SetPreviewQuality { quality: String, reply: SyncSender<Value> },
}

/// Spawn the MCP stdio server on a background thread.
/// Returns the `Sender` (for sharing with other transports) and the `Receiver`
/// that the GTK main thread should poll for commands.
pub fn start_mcp_server() -> (std::sync::mpsc::Sender<McpCommand>, std::sync::mpsc::Receiver<McpCommand>) {
    let (sender, receiver) = std::sync::mpsc::channel::<McpCommand>();
    let stdio_sender = sender.clone();
    std::thread::spawn(move || {
        server::run_stdio_server(stdio_sender);
    });
    (sender, receiver)
}

/// Spawn the MCP Unix-domain-socket server on a background thread.
/// Shares the same command channel as the stdio server.
/// Returns a stop flag — set it to `true` to shut down the listener.
pub fn start_mcp_socket_server(
    sender: std::sync::mpsc::Sender<McpCommand>,
) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    std::thread::spawn(move || {
        server::run_socket_server(sender, stop_clone);
    });
    stop
}
