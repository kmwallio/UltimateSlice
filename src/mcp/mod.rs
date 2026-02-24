use std::sync::mpsc::SyncSender;
use serde_json::Value;

pub mod server;

/// Commands sent from the MCP stdio thread to the GTK main thread.
/// Each variant carries a `reply` one-shot sync-channel for the result.
pub enum McpCommand {
    GetProject { reply: SyncSender<Value> },
    ListTracks  { reply: SyncSender<Value> },
    ListClips   { reply: SyncSender<Value> },
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
    SetTitle    { title: String, reply: SyncSender<Value> },
    SaveFcpxml  { path: String, reply: SyncSender<Value> },
}

/// Spawn the MCP stdio server on a background thread.
/// Returns a `Receiver` that the GTK main thread should poll for commands.
pub fn start_mcp_server() -> std::sync::mpsc::Receiver<McpCommand> {
    let (sender, receiver) = std::sync::mpsc::channel::<McpCommand>();
    std::thread::spawn(move || {
        server::run_stdio_server(sender);
    });
    receiver
}
