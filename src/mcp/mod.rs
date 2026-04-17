use serde_json::Value;
use std::collections::HashMap;
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
    ListLadspaPlugins {
        reply: SyncSender<Value>,
    },
    AddClipLadspaEffect {
        clip_id: String,
        plugin_name: String,
        reply: SyncSender<Value>,
    },
    RemoveClipLadspaEffect {
        clip_id: String,
        effect_id: String,
        reply: SyncSender<Value>,
    },
    SetClipLadspaEffectParams {
        clip_id: String,
        effect_id: String,
        params: HashMap<String, f64>,
        reply: SyncSender<Value>,
    },
    SetTrackRole {
        track_id: String,
        role: String,
        reply: SyncSender<Value>,
    },
    SetTrackDuck {
        track_id: String,
        duck: bool,
        reply: SyncSender<Value>,
    },
    SetTrackMuted {
        track_id: String,
        muted: bool,
        reply: SyncSender<Value>,
    },
    SetTrackGain {
        track_id: String,
        gain_db: f64,
        reply: SyncSender<Value>,
    },
    SetTrackPan {
        track_id: String,
        pan: f64,
        reply: SyncSender<Value>,
    },
    GetMixerState {
        reply: SyncSender<Value>,
    },
    SetBusGain {
        role: String,
        gain_db: f64,
        reply: SyncSender<Value>,
    },
    SetBusMuted {
        role: String,
        muted: bool,
        reply: SyncSender<Value>,
    },
    SetBusSoloed {
        role: String,
        soloed: bool,
        reply: SyncSender<Value>,
    },
    SetTrackLocked {
        track_id: String,
        locked: bool,
        reply: SyncSender<Value>,
    },
    SetTrackColor {
        track_id: String,
        color: String,
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
    GetProjectHealth {
        reply: SyncSender<Value>,
    },
    CleanupProjectCache {
        cache: String,
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
    SetProxySidecarPersistence {
        enabled: bool,
        reply: SyncSender<Value>,
    },
    SetBackgroundAiIndexing {
        enabled: bool,
        reply: SyncSender<Value>,
    },
    SetBackgroundAutoTagging {
        enabled: bool,
        reply: SyncSender<Value>,
    },
    SetPrerenderQuality {
        preset: String,
        crf: u32,
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
    ConvertLtcAudioToTimecode {
        clip_id: String,
        ltc_channel: crate::media::ltc::LtcChannelSelection,
        frame_rate: Option<crate::model::project::FrameRate>,
        reply: SyncSender<Value>,
    },
    TrimClip {
        clip_id: String,
        source_in_ns: u64,
        source_out_ns: u64,
        reply: SyncSender<Value>,
    },
    /// Set per-clip playback speed and (optionally) the slow-motion frame
    /// interpolation mode.  When `slow_motion_interp` is `"ai"`, the
    /// FrameInterpCache is asked to precompute a higher-fps sidecar in the
    /// background; preview/export consume the sidecar once it is ready.
    SetClipSpeed {
        clip_id: String,
        speed: f64,
        /// `"off"`, `"blend"`, `"optical-flow"`, or `"ai"`. Omit to leave
        /// the existing interpolation mode unchanged.
        slow_motion_interp: Option<String>,
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
        blur: f64,
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
    /// Set (or clear) the HSL Qualifier on a clip. Pass `qualifier: None`
    /// to clear the qualifier entirely.
    SetClipHslQualifier {
        clip_id: String,
        qualifier: Option<crate::model::clip::HslQualifier>,
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
    SetClipMotionBlur {
        clip_id: String,
        enabled: Option<bool>,
        shutter_angle: Option<f64>,
        reply: SyncSender<Value>,
    },
    SetClipMask {
        clip_id: String,
        enabled: Option<bool>,
        shape: Option<String>,
        center_x: Option<f64>,
        center_y: Option<f64>,
        width: Option<f64>,
        height: Option<f64>,
        rotation: Option<f64>,
        feather: Option<f64>,
        expansion: Option<f64>,
        invert: Option<bool>,
        path: Option<serde_json::Value>,
        reply: SyncSender<Value>,
    },
    /// Generate a new bezier-path ClipMask from a SAM 3 segmentation.
    /// Takes either a box prompt (drag a rectangle) or a point
    /// prompt (single click; emulated via a tiny synthetic box).
    /// Inference is blocking — the MCP caller waits for the full
    /// SAM pipeline (decode frame → image encoder → decoder →
    /// contour extraction) before the reply comes back.
    GenerateSamMask {
        clip_id: String,
        /// Absolute source-media time in ns for the frame to
        /// segment. When `None`, defaults to the clip's `source_in`.
        frame_ns: Option<u64>,
        /// Box prompt in clip-local normalized 0..1 coordinates.
        /// Takes precedence over `point` when both are provided.
        box_x1: Option<f64>,
        box_y1: Option<f64>,
        box_x2: Option<f64>,
        box_y2: Option<f64>,
        /// Point prompt in clip-local normalized 0..1 coordinates.
        /// Emulated as a small synthetic box centered on the click.
        point_x: Option<f64>,
        point_y: Option<f64>,
        /// Douglas-Peucker simplification tolerance in source-image
        /// pixels. `None` uses a sensible default (~2.0).
        tolerance_px: Option<f64>,
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
    SaveEdl {
        path: String,
        reply: SyncSender<Value>,
    },
    SaveOtio {
        path: String,
        path_mode: String,
        reply: SyncSender<Value>,
    },
    OpenOtio {
        path: String,
        reply: SyncSender<Value>,
    },
    SaveProjectWithMedia {
        path: String,
        reply: SyncSender<Value>,
    },
    CollectProjectFiles {
        directory_path: String,
        mode: crate::fcpxml::writer::CollectFilesMode,
        use_collected_locations_on_next_save: bool,
        reply: SyncSender<Value>,
    },
    ExportMp4 {
        path: String,
        /// Optional surround layout for advanced audio mode. Accepts
        /// `"stereo"` (default), `"surround_5_1"` / `"5.1"`, or
        /// `"surround_7_1"` / `"7.1"`. Unknown values fall back to stereo.
        audio_channel_layout: String,
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
        /// Advanced audio mode: see ExportMp4 for accepted values.
        audio_channel_layout: String,
        reply: SyncSender<Value>,
    },
    DeleteExportPreset {
        name: String,
        reply: SyncSender<Value>,
    },
    ListWorkspaceLayouts {
        reply: SyncSender<Value>,
    },
    SaveWorkspaceLayout {
        name: String,
        reply: SyncSender<Value>,
    },
    ApplyWorkspaceLayout {
        name: String,
        reply: SyncSender<Value>,
    },
    RenameWorkspaceLayout {
        old_name: String,
        new_name: String,
        reply: SyncSender<Value>,
    },
    DeleteWorkspaceLayout {
        name: String,
        reply: SyncSender<Value>,
    },
    ResetWorkspaceLayout {
        reply: SyncSender<Value>,
    },
    ExportWithPreset {
        path: String,
        preset_name: String,
        reply: SyncSender<Value>,
    },
    ListLibrary {
        search_text: Option<String>,
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
    CreateBin {
        name: String,
        parent_id: Option<String>,
        reply: SyncSender<Value>,
    },
    DeleteBin {
        bin_id: String,
        reply: SyncSender<Value>,
    },
    RenameBin {
        bin_id: String,
        name: String,
        reply: SyncSender<Value>,
    },
    ListBins {
        reply: SyncSender<Value>,
    },
    MoveToBin {
        source_paths: Vec<String>,
        bin_id: Option<String>,
        reply: SyncSender<Value>,
    },
    ListCollections {
        reply: SyncSender<Value>,
    },
    CreateCollection {
        name: String,
        search_text: Option<String>,
        kind: Option<String>,
        resolution: Option<String>,
        frame_rate: Option<String>,
        rating: Option<String>,
        reply: SyncSender<Value>,
    },
    UpdateCollection {
        collection_id: String,
        name: Option<String>,
        search_text: Option<String>,
        kind: Option<String>,
        resolution: Option<String>,
        frame_rate: Option<String>,
        rating: Option<String>,
        reply: SyncSender<Value>,
    },
    DeleteCollection {
        collection_id: String,
        reply: SyncSender<Value>,
    },
    SetMediaRating {
        library_key: String,
        rating: String,
        reply: SyncSender<Value>,
    },
    AddMediaKeywordRange {
        library_key: String,
        label: String,
        start_ns: u64,
        end_ns: u64,
        reply: SyncSender<Value>,
    },
    UpdateMediaKeywordRange {
        library_key: String,
        range_id: String,
        label: String,
        start_ns: u64,
        end_ns: u64,
        reply: SyncSender<Value>,
    },
    DeleteMediaKeywordRange {
        library_key: String,
        range_id: String,
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
        alignment: String,
        reply: SyncSender<Value>,
    },
    CreateProject {
        title: String,
        reply: SyncSender<Value>,
    },
    SetClipLut {
        clip_id: String,
        lut_paths: Vec<String>,
        reply: SyncSender<Value>,
    },
    SetClipTransform {
        clip_id: String,
        scale: f64,
        position_x: f64,
        position_y: f64,
        rotate: Option<i32>,
        anamorphic_desqueeze: Option<f64>,
        reply: SyncSender<Value>,
    },
    SetClipOpacity {
        clip_id: String,
        opacity: f64,
        reply: SyncSender<Value>,
    },
    SetClipVoiceIsolation {
        clip_id: String,
        voice_isolation: f64,
        reply: SyncSender<Value>,
    },
    SetClipVoiceEnhance {
        clip_id: String,
        enabled: bool,
        /// Optional 0.0..=1.0 strength. `None` keeps the existing value.
        strength: Option<f64>,
        reply: SyncSender<Value>,
    },
    SetClipSubtitleVisible {
        clip_id: String,
        visible: bool,
        reply: SyncSender<Value>,
    },
    SetVoiceIsolationSource {
        clip_id: String,
        /// `"subtitles"` or `"silence"`
        source: String,
        reply: SyncSender<Value>,
    },
    SetVoiceIsolationSilenceParams {
        clip_id: String,
        threshold_db: Option<f64>,
        min_ms: Option<u32>,
        reply: SyncSender<Value>,
    },
    SuggestVoiceIsolationThreshold {
        clip_id: String,
        reply: SyncSender<Value>,
    },
    AnalyzeVoiceIsolationSilence {
        clip_id: String,
        reply: SyncSender<Value>,
    },
    SetClipEq {
        clip_id: String,
        low_freq: Option<f64>,
        low_gain: Option<f64>,
        low_q: Option<f64>,
        mid_freq: Option<f64>,
        mid_gain: Option<f64>,
        mid_q: Option<f64>,
        high_freq: Option<f64>,
        high_gain: Option<f64>,
        high_q: Option<f64>,
        reply: SyncSender<Value>,
    },
    NormalizeClipAudio {
        clip_id: String,
        mode: String,
        target_level: f64,
        reply: SyncSender<Value>,
    },
    /// Render the full timeline mixdown to a temp file and return a
    /// full EBU R128 loudness report (integrated, short-term max,
    /// momentary max, LRA, true peak).
    AnalyzeProjectLoudness {
        reply: SyncSender<Value>,
    },
    /// Set (or clear with 0.0) the project-level master audio gain in dB.
    /// Applied to both preview and export. Clamped to ±24 dB.
    SetProjectMasterGainDb {
        master_gain_db: f64,
        reply: SyncSender<Value>,
    },
    MatchClipAudio {
        source_clip_id: String,
        source_start_ns: Option<u64>,
        source_end_ns: Option<u64>,
        source_channel_mode: crate::media::audio_match::AudioMatchChannelMode,
        reference_clip_id: String,
        reference_start_ns: Option<u64>,
        reference_end_ns: Option<u64>,
        reference_channel_mode: crate::media::audio_match::AudioMatchChannelMode,
        reply: SyncSender<Value>,
    },
    ClearMatchEq {
        clip_id: String,
        reply: SyncSender<Value>,
    },
    DetectSceneCuts {
        clip_id: String,
        track_id: String,
        threshold: f64,
        reply: SyncSender<Value>,
    },
    GenerateMusic {
        prompt: String,
        duration_secs: f64,
        track_index: Option<usize>,
        timeline_start_ns: Option<u64>,
        /// Optional path to a reference audio file. When provided, the
        /// handler runs `audio_features::analyze_audio_file` and appends
        /// the derived natural-language style hints (BPM, key/mode,
        /// brightness, dynamics) to `prompt` before queuing the job.
        /// Analysis failures degrade gracefully — the original prompt is
        /// used and a warning is logged.
        reference_audio_path: Option<String>,
        reply: SyncSender<Value>,
    },
    RecordVoiceover {
        duration_ns: u64,
        track_index: Option<usize>,
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
    SetPrerenderProjectPersistence {
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
    MatchFrame {
        clip_id: Option<String>,
        reply: SyncSender<Value>,
    },
    SetClipStabilization {
        clip_id: String,
        enabled: bool,
        smoothing: f64,
        reply: SyncSender<Value>,
    },
    SetClipAutoCropTrack {
        clip_id: String,
        /// Region center X in normalized clip coordinates (0..1).
        center_x: f64,
        /// Region center Y in normalized clip coordinates (0..1).
        center_y: f64,
        /// Region half-width in normalized clip coordinates (0..0.5).
        width: f64,
        /// Region half-height in normalized clip coordinates (0..0.5).
        height: f64,
        /// Optional extra headroom around the region as a fraction
        /// (e.g. 0.1 = 10% margin). Clamped to [0, 0.5]. Defaults to 0.1
        /// when omitted.
        padding: Option<f64>,
        reply: SyncSender<Value>,
    },
    ListBackups {
        reply: SyncSender<Value>,
    },
    ListProjectSnapshots {
        reply: SyncSender<Value>,
    },
    CreateProjectSnapshot {
        name: String,
        reply: SyncSender<Value>,
    },
    RestoreProjectSnapshot {
        snapshot_id: String,
        reply: SyncSender<Value>,
    },
    DeleteProjectSnapshot {
        snapshot_id: String,
        reply: SyncSender<Value>,
    },
    SyncClipsByAudio {
        clip_ids: Vec<String>,
        replace_audio: bool,
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
        string_params: Option<std::collections::HashMap<String, String>>,
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
        string_params: Option<std::collections::HashMap<String, String>>,
        reply: SyncSender<Value>,
    },
    ReorderClipFrei0rEffects {
        clip_id: String,
        effect_ids: Vec<String>,
        reply: SyncSender<Value>,
    },
    AddTitleClip {
        template_id: String,
        track_index: Option<usize>,
        timeline_start_ns: Option<u64>,
        duration_ns: Option<u64>,
        title_text: Option<String>,
        reply: SyncSender<Value>,
    },
    AddAdjustmentLayer {
        track_index: usize,
        timeline_start_ns: u64,
        duration_ns: u64,
        reply: SyncSender<Value>,
    },
    SetClipTitleStyle {
        clip_id: String,
        title_text: Option<String>,
        title_font: Option<String>,
        title_color: Option<u32>,
        title_x: Option<f64>,
        title_y: Option<f64>,
        title_outline_width: Option<f64>,
        title_outline_color: Option<u32>,
        title_shadow: Option<bool>,
        title_shadow_color: Option<u32>,
        title_shadow_offset_x: Option<f64>,
        title_shadow_offset_y: Option<f64>,
        title_bg_box: Option<bool>,
        title_bg_box_color: Option<u32>,
        title_bg_box_padding: Option<f64>,
        title_clip_bg_color: Option<u32>,
        title_secondary_text: Option<String>,
        reply: SyncSender<Value>,
    },
    AddToExportQueue {
        output_path: String,
        preset_name: Option<String>,
        reply: SyncSender<Value>,
    },
    ListExportQueue {
        reply: SyncSender<Value>,
    },
    ClearExportQueue {
        /// "all", "done", "error", or None (same as "all")
        status_filter: Option<String>,
        reply: SyncSender<Value>,
    },
    RunExportQueue {
        reply: SyncSender<Value>,
    },
    CreateCompoundClip {
        clip_ids: Vec<String>,
        reply: SyncSender<Value>,
    },
    BreakApartCompoundClip {
        clip_id: String,
        reply: SyncSender<Value>,
    },
    CreateMulticamClip {
        clip_ids: Vec<String>,
        reply: SyncSender<Value>,
    },
    AddAngleSwitch {
        clip_id: String,
        position_ns: u64,
        angle_index: usize,
        reply: SyncSender<Value>,
    },
    ListMulticamAngles {
        clip_id: String,
        reply: SyncSender<Value>,
    },
    SetMulticamAngleAudio {
        clip_id: String,
        angle_index: usize,
        volume: Option<f32>,
        muted: Option<bool>,
        reply: SyncSender<Value>,
    },
    SetMulticamAngleColor {
        clip_id: String,
        angle_index: usize,
        brightness: Option<f32>,
        contrast: Option<f32>,
        saturation: Option<f32>,
        temperature: Option<f32>,
        tint: Option<f32>,
        lut_paths: Option<Vec<String>>,
        reply: SyncSender<Value>,
    },
    // ── Audition / clip-versions commands ─────────────────────────────
    CreateAuditionClip {
        clip_ids: Vec<String>,
        active_index: usize,
        reply: SyncSender<Value>,
    },
    AddAuditionTake {
        audition_clip_id: String,
        source_path: String,
        source_in_ns: u64,
        source_out_ns: u64,
        label: Option<String>,
        reply: SyncSender<Value>,
    },
    RemoveAuditionTake {
        audition_clip_id: String,
        take_index: usize,
        reply: SyncSender<Value>,
    },
    SetActiveAuditionTake {
        audition_clip_id: String,
        take_index: usize,
        reply: SyncSender<Value>,
    },
    ListAuditionTakes {
        audition_clip_id: String,
        reply: SyncSender<Value>,
    },
    FinalizeAudition {
        audition_clip_id: String,
        reply: SyncSender<Value>,
    },
    // ── Subtitle / STT commands ────────────────────────────────────────
    GenerateSubtitles {
        clip_id: String,
        language: String,
        reply: SyncSender<Value>,
    },
    GetClipSubtitles {
        clip_id: String,
        reply: SyncSender<Value>,
    },
    EditSubtitleText {
        clip_id: String,
        segment_id: String,
        text: String,
        reply: SyncSender<Value>,
    },
    EditSubtitleTiming {
        clip_id: String,
        segment_id: String,
        start_ns: u64,
        end_ns: u64,
        reply: SyncSender<Value>,
    },
    ClearSubtitles {
        clip_id: String,
        reply: SyncSender<Value>,
    },
    DeleteTranscriptRange {
        clip_id: String,
        start_word_index: u32,
        end_word_index: u32, // exclusive
        reply: SyncSender<Value>,
    },
    SetSubtitleStyle {
        clip_id: String,
        font: Option<String>,
        color: Option<u32>,
        outline_color: Option<u32>,
        outline_width: Option<f64>,
        bg_box: Option<bool>,
        bg_box_color: Option<u32>,
        highlight_mode: Option<String>,
        highlight_color: Option<u32>,
        // New base style fields
        bold: Option<bool>,
        italic: Option<bool>,
        underline: Option<bool>,
        shadow: Option<bool>,
        // New highlight flag fields
        highlight_bold: Option<bool>,
        highlight_color_flag: Option<bool>,
        highlight_underline: Option<bool>,
        highlight_stroke: Option<bool>,
        highlight_italic: Option<bool>,
        highlight_background: Option<bool>,
        highlight_shadow: Option<bool>,
        bg_highlight_color: Option<u32>,
        highlight_stroke_color: Option<u32>,
        reply: SyncSender<Value>,
    },
    ExportSrt {
        path: String,
        reply: SyncSender<Value>,
    },
    // ── Script-to-Timeline ──────────────────────────────────────────
    LoadScript {
        path: String,
        reply: SyncSender<Value>,
    },
    GetScriptScenes {
        reply: SyncSender<Value>,
    },
    RunScriptAlignment {
        clip_paths: Vec<String>,
        confidence_threshold: f64,
        reply: SyncSender<Value>,
    },
    ApplyScriptAssembly {
        include_titles: bool,
        reply: SyncSender<Value>,
    },
    ReorderByScript {
        track_id: String,
        reply: SyncSender<Value>,
    },
    // ── Marker tools ──
    ListMarkers {
        reply: SyncSender<Value>,
    },
    AddMarker {
        position_ns: u64,
        label: String,
        color: Option<u32>,
        notes: Option<String>,
        reply: SyncSender<Value>,
    },
    RemoveMarker {
        marker_id: String,
        reply: SyncSender<Value>,
    },
    EditMarker {
        marker_id: String,
        label: Option<String>,
        color: Option<u32>,
        notes: Option<String>,
        position_ns: Option<u64>,
        reply: SyncSender<Value>,
    },
}

/// Spawn the MCP stdio server on a background thread.
/// Returns the `Sender` (for sharing with other transports) and the `Receiver`
/// that the GTK main thread should poll for commands.
// Public convenience wrapper; currently unused (window.rs calls run_stdio_server
// directly) but kept as a stable entry point for external / test consumers.
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
