use serde_json::Value;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::SyncSender;
use std::sync::Arc;

pub mod server;

/// Commands sent from the MCP stdio thread to the GTK main thread.
/// Each variant carries a `reply` one-shot sync-channel for the result.
pub enum McpCommand {
    GetProject {
        reply: SyncSender<Value>,
    },
    ListTracks {
        compact: bool,
        reply: SyncSender<Value>,
    },
    ListClips {
        compact: bool,
        reply: SyncSender<Value>,
    },
    GetTimelineSettings {
        reply: SyncSender<Value>,
    },
    GetPlayheadPosition {
        reply: SyncSender<Value>,
    },
    GetPerformanceSnapshot {
        reply: SyncSender<Value>,
    },
    SetMagneticMode {
        enabled: bool,
        reply: SyncSender<Value>,
    },
    SetTrackSolo {
        track_id: String,
        solo: bool,
        reply: SyncSender<Value>,
    },
    SetTrackHeightPreset {
        track_id: String,
        height_preset: String,
        reply: SyncSender<Value>,
    },
    CloseSourcePreview {
        reply: SyncSender<Value>,
    },
    GetPreferences {
        reply: SyncSender<Value>,
    },
    SetHardwareAcceleration {
        enabled: bool,
        reply: SyncSender<Value>,
    },
    SetPlaybackPriority {
        priority: String,
        reply: SyncSender<Value>,
    },
    SetSourcePlaybackPriority {
        priority: String,
        reply: SyncSender<Value>,
    },
    SetCrossfadeSettings {
        enabled: bool,
        curve: String,
        duration_ns: u64,
        reply: SyncSender<Value>,
    },
    SetProxyMode {
        mode: String,
        reply: SyncSender<Value>,
    },
    AddClip {
        source_path: String,
        track_index: usize,
        timeline_start_ns: u64,
        source_in_ns: u64,
        source_out_ns: u64,
        reply: SyncSender<Value>,
    },
    RemoveClip {
        clip_id: String,
        reply: SyncSender<Value>,
    },
    MoveClip {
        clip_id: String,
        new_start_ns: u64,
        reply: SyncSender<Value>,
    },
    LinkClips {
        clip_ids: Vec<String>,
        reply: SyncSender<Value>,
    },
    UnlinkClips {
        clip_ids: Vec<String>,
        reply: SyncSender<Value>,
    },
    AlignGroupedClipsByTimecode {
        clip_ids: Vec<String>,
        reply: SyncSender<Value>,
    },
    TrimClip {
        clip_id: String,
        source_in_ns: u64,
        source_out_ns: u64,
        reply: SyncSender<Value>,
    },
    SetClipColor {
        clip_id: String,
        brightness: f64,
        contrast: f64,
        saturation: f64,
        temperature: f64,
        tint: f64,
        denoise: f64,
        sharpness: f64,
        shadows: f64,
        midtones: f64,
        highlights: f64,
        exposure: f64,
        black_point: f64,
        highlights_warmth: f64,
        highlights_tint: f64,
        midtones_warmth: f64,
        midtones_tint: f64,
        shadows_warmth: f64,
        shadows_tint: f64,
        reply: SyncSender<Value>,
    },
    SetClipColorLabel {
        clip_id: String,
        color_label: String,
        reply: SyncSender<Value>,
    },
    SetClipChromaKey {
        clip_id: String,
        enabled: Option<bool>,
        color: Option<u32>,
        tolerance: Option<f64>,
        softness: Option<f64>,
        reply: SyncSender<Value>,
    },
    SetClipBgRemoval {
        clip_id: String,
        enabled: Option<bool>,
        threshold: Option<f64>,
        reply: SyncSender<Value>,
    },
    SetTitle {
        title: String,
        reply: SyncSender<Value>,
    },
    OpenFcpxml {
        path: String,
        reply: SyncSender<Value>,
    },
    SaveFcpxml {
        path: String,
        reply: SyncSender<Value>,
    },
    SaveProjectWithMedia {
        path: String,
        reply: SyncSender<Value>,
    },
    ExportMp4 {
        path: String,
        reply: SyncSender<Value>,
    },
    ListExportPresets {
        reply: SyncSender<Value>,
    },
    SaveExportPreset {
        name: String,
        video_codec: String,
        container: String,
        output_width: u32,
        output_height: u32,
        crf: u32,
        audio_codec: String,
        audio_bitrate_kbps: u32,
        reply: SyncSender<Value>,
    },
    DeleteExportPreset {
        name: String,
        reply: SyncSender<Value>,
    },
    ExportWithPreset {
        path: String,
        preset_name: String,
        reply: SyncSender<Value>,
    },
    ListLibrary {
        reply: SyncSender<Value>,
    },
    ImportMedia {
        path: String,
        reply: SyncSender<Value>,
    },
    RelinkMedia {
        root_path: String,
        reply: SyncSender<Value>,
    },
    ReorderTrack {
        from_index: usize,
        to_index: usize,
        reply: SyncSender<Value>,
    },
    SetTransition {
        track_index: usize,
        clip_index: usize,
        kind: String,
        duration_ns: u64,
        reply: SyncSender<Value>,
    },
    CreateProject {
        title: String,
        reply: SyncSender<Value>,
    },
    SetClipLut {
        clip_id: String,
        lut_path: Option<String>,
        reply: SyncSender<Value>,
    },
    SetClipTransform {
        clip_id: String,
        scale: f64,
        position_x: f64,
        position_y: f64,
        rotate: Option<i32>,
        reply: SyncSender<Value>,
    },
    SetClipOpacity {
        clip_id: String,
        opacity: f64,
        reply: SyncSender<Value>,
    },
    SetClipBlendMode {
        clip_id: String,
        blend_mode: String,
        reply: SyncSender<Value>,
    },
    SetClipKeyframe {
        clip_id: String,
        property: String,
        timeline_pos_ns: Option<u64>,
        value: f64,
        interpolation: Option<String>,
        bezier_controls: Option<(f64, f64, f64, f64)>,
        reply: SyncSender<Value>,
    },
    RemoveClipKeyframe {
        clip_id: String,
        property: String,
        timeline_pos_ns: Option<u64>,
        reply: SyncSender<Value>,
    },
    SlipClip {
        clip_id: String,
        delta_ns: i64,
        reply: SyncSender<Value>,
    },
    SlideClip {
        clip_id: String,
        delta_ns: i64,
        reply: SyncSender<Value>,
    },
    SetGskRenderer {
        renderer: String,
        reply: SyncSender<Value>,
    },
    SetPreviewQuality {
        quality: String,
        reply: SyncSender<Value>,
    },
    SetRealtimePreview {
        enabled: bool,
        reply: SyncSender<Value>,
    },
    SetExperimentalPreviewOptimizations {
        enabled: bool,
        reply: SyncSender<Value>,
    },
    SetBackgroundPrerender {
        enabled: bool,
        reply: SyncSender<Value>,
    },
    SetPreviewLuts {
        enabled: bool,
        reply: SyncSender<Value>,
    },
    SeekPlayhead {
        timeline_pos_ns: u64,
        reply: SyncSender<Value>,
    },
    ExportDisplayedFrame {
        path: String,
        reply: SyncSender<Value>,
    },
    ExportTimelineSnapshot {
        path: String,
        width: u32,
        height: u32,
        reply: SyncSender<Value>,
    },
    Play {
        reply: SyncSender<Value>,
    },
    Pause {
        reply: SyncSender<Value>,
    },
    Stop {
        reply: SyncSender<Value>,
    },
    InsertClip {
        source_path: String,
        source_in_ns: u64,
        source_out_ns: u64,
        track_index: Option<usize>,
        timeline_pos_ns: Option<u64>,
        reply: SyncSender<Value>,
    },
    OverwriteClip {
        source_path: String,
        source_in_ns: u64,
        source_out_ns: u64,
        track_index: Option<usize>,
        timeline_pos_ns: Option<u64>,
        reply: SyncSender<Value>,
    },
    TakeScreenshot {
        reply: SyncSender<Value>,
    },
    SelectLibraryItem {
        path: String,
        reply: SyncSender<Value>,
    },
    SourcePlay {
        reply: SyncSender<Value>,
    },
    SourcePause {
        reply: SyncSender<Value>,
    },
    SyncClipsByAudio {
        clip_ids: Vec<String>,
        reply: SyncSender<Value>,
    },
    CopyClipColorGrade {
        clip_id: String,
        reply: SyncSender<Value>,
    },
    PasteClipColorGrade {
        clip_id: String,
        reply: SyncSender<Value>,
    },
    MatchClipColors {
        source_clip_id: String,
        reference_clip_id: String,
        generate_lut: bool,
        reply: SyncSender<Value>,
    },
    ListFrei0rPlugins {
        reply: SyncSender<Value>,
    },
    ListClipFrei0rEffects {
        clip_id: String,
        reply: SyncSender<Value>,
    },
    AddClipFrei0rEffect {
        clip_id: String,
        plugin_name: String,
        params: Option<std::collections::HashMap<String, f64>>,
        reply: SyncSender<Value>,
    },
    RemoveClipFrei0rEffect {
        clip_id: String,
        effect_id: String,
        reply: SyncSender<Value>,
    },
    SetClipFrei0rEffectParams {
        clip_id: String,
        effect_id: String,
        params: std::collections::HashMap<String, f64>,
        reply: SyncSender<Value>,
    },
    ReorderClipFrei0rEffects {
        clip_id: String,
        effect_ids: Vec<String>,
        reply: SyncSender<Value>,
    },
}

/// Spawn the MCP stdio server on a background thread.
/// Returns the `Sender` (for sharing with other transports) and the `Receiver`
/// that the GTK main thread should poll for commands.
#[allow(dead_code)]
pub fn start_mcp_server() -> (
    std::sync::mpsc::Sender<McpCommand>,
    std::sync::mpsc::Receiver<McpCommand>,
) {
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
pub fn start_mcp_socket_server(sender: std::sync::mpsc::Sender<McpCommand>) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    std::thread::spawn(move || {
        server::run_socket_server(sender, stop_clone);
    });
    stop
}
