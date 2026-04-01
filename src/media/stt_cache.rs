// SPDX-License-Identifier: GPL-3.0-or-later
//! Speech-to-text cache: background whisper inference for subtitle generation.
//!
//! Modeled after [`super::bg_removal_cache::BgRemovalCache`]: a single worker
//! thread extracts audio from source clips, runs whisper inference, and returns
//! timed subtitle segments.  The whisper model stays loaded in the worker for
//! the session to avoid repeated load overhead.

use crate::model::clip::SubtitleSegment;
#[cfg(feature = "speech-to-text")]
use crate::model::clip::SubtitleWord;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;

// ── Public types ───────────────────────────────────────────────────────────

/// Progress update from the STT worker.
enum WorkerUpdate {
    Done(WorkerResult),
}

struct WorkerResult {
    cache_key: String,
    segments: Vec<SubtitleSegment>,
    success: bool,
    error: Option<String>,
}

/// A single queued STT job.
struct SttJob {
    cache_key: String,
    source_path: String,
    source_in_ns: u64,
    source_out_ns: u64,
    model_path: String,
    language: String,
}

/// Aggregate progress for the status bar.
pub struct SttProgress {
    pub total: usize,
    pub completed: usize,
    pub in_flight: bool,
}

// ── Cache ──────────────────────────────────────────────────────────────────

pub struct SttCache {
    /// Completed results: cache_key → subtitle segments.
    results: HashMap<String, Vec<SubtitleSegment>>,
    /// Currently processing keys.
    pending: HashSet<String>,
    /// Failed keys (not retried).
    failed: HashSet<String>,
    /// cache_key → (source_path, source_in, source_out) for result delivery.
    key_to_info: HashMap<String, (String, u64, u64)>,
    total_requested: usize,
    result_rx: mpsc::Receiver<WorkerUpdate>,
    work_tx: Option<mpsc::Sender<SttJob>>,
    model_path: Option<String>,
    /// Last error message from a failed STT job (cleared on next successful request).
    pub last_error: Option<String>,
}

/// Result returned by `poll()`: source info + generated segments.
pub struct SttPollResult {
    pub source_path: String,
    pub source_in_ns: u64,
    pub source_out_ns: u64,
    pub segments: Vec<SubtitleSegment>,
}

impl SttCache {
    pub fn new() -> Self {
        let (result_tx, result_rx) = mpsc::sync_channel::<WorkerUpdate>(32);
        let (work_tx, work_rx) = mpsc::channel::<SttJob>();

        // Single worker thread — whisper is CPU-heavy and benefits from keeping
        // the model resident in memory for the session.
        std::thread::spawn(move || {
            stt_worker_loop(work_rx, result_tx);
        });

        let model_path = find_stt_model_path();
        if model_path.is_none() {
            log::warn!("SttCache: whisper model not found; speech-to-text disabled");
        }

        Self {
            results: HashMap::new(),
            pending: HashSet::new(),
            failed: HashSet::new(),
            key_to_info: HashMap::new(),
            total_requested: 0,
            result_rx,
            work_tx: Some(work_tx),
            model_path,
            last_error: None,
        }
    }

    /// Returns `true` if a whisper model is available.
    pub fn is_available(&self) -> bool {
        self.model_path.is_some()
    }

    /// Returns `true` if the `speech-to-text` feature was compiled in.
    pub fn feature_enabled(&self) -> bool {
        cfg!(feature = "speech-to-text")
    }

    /// Re-check for the model file (e.g. after a download completes).
    pub fn refresh_model_path(&mut self) {
        self.model_path = find_stt_model_path();
    }

    /// Request STT for a clip region. Returns immediately.
    /// Call [`poll()`] periodically to collect results.
    pub fn request(
        &mut self,
        source_path: &str,
        source_in_ns: u64,
        source_out_ns: u64,
        language: &str,
    ) {
        let Some(ref model_path) = self.model_path else {
            return;
        };
        let key = cache_key(source_path, source_in_ns, source_out_ns, language);
        if self.pending.contains(&key) || self.failed.contains(&key) || self.results.contains_key(&key) {
            return;
        }

        self.last_error = None;
        self.total_requested += 1;
        self.pending.insert(key.clone());
        self.key_to_info.insert(
            key.clone(),
            (source_path.to_string(), source_in_ns, source_out_ns),
        );

        if let Some(ref tx) = self.work_tx {
            let _ = tx.send(SttJob {
                cache_key: key,
                source_path: source_path.to_string(),
                source_in_ns,
                source_out_ns,
                model_path: model_path.clone(),
                language: language.to_string(),
            });
        }
    }

    /// Non-blocking poll for completed jobs.
    pub fn poll(&mut self) -> Vec<SttPollResult> {
        let mut resolved = Vec::new();
        while let Ok(update) = self.result_rx.try_recv() {
            match update {
                WorkerUpdate::Done(result) => {
                    self.pending.remove(&result.cache_key);
                    if result.success {
                        let info = self
                            .key_to_info
                            .remove(&result.cache_key)
                            .unwrap_or_else(|| (String::new(), 0, 0));
                        log::info!(
                            "SttCache: completed source={} segments={}",
                            info.0,
                            result.segments.len()
                        );
                        self.results
                            .insert(result.cache_key, result.segments.clone());
                        resolved.push(SttPollResult {
                            source_path: info.0,
                            source_in_ns: info.1,
                            source_out_ns: info.2,
                            segments: result.segments,
                        });
                    } else {
                        log::warn!("SttCache: failed key={}", result.cache_key);
                        self.failed.insert(result.cache_key);
                        self.last_error = result.error.or_else(|| {
                            Some("Subtitle generation failed. Check the log for details.".to_string())
                        });
                    }
                }
            }
        }
        resolved
    }

    /// Aggregate progress for the UI.
    pub fn progress(&self) -> SttProgress {
        SttProgress {
            total: self.total_requested,
            completed: self.results.len(),
            in_flight: !self.pending.is_empty(),
        }
    }
}

impl Drop for SttCache {
    fn drop(&mut self) {
        self.work_tx.take();
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn cache_key(source_path: &str, source_in_ns: u64, source_out_ns: u64, language: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    source_path.hash(&mut hasher);
    source_in_ns.hash(&mut hasher);
    source_out_ns.hash(&mut hasher);
    language.hash(&mut hasher);
    format!("stt_{}", hasher.finish())
}

/// Search standard locations for any whisper GGML model file (`ggml-*.bin`).
/// Prefers larger/better models when multiple are found in the same directory.
pub fn find_stt_model_path() -> Option<String> {
    let search_dirs: Vec<PathBuf> = vec![
        // Relative to executable.
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("data/models")))
            .unwrap_or_default(),
        // Relative to CWD (development).
        PathBuf::from("data/models"),
        // Flatpak data dir.
        PathBuf::from("/app/share/ultimateslice/models"),
        // XDG data home.
        stt_model_dir(),
    ];

    // Priority order: prefer larger models when multiple exist.
    // Within a size tier, prefer .en (English-only, faster) over multilingual.
    const MODEL_PRIORITY: &[&str] = &[
        "large-v3", "large-v2", "large-v1", "large",
        "medium.en", "medium",
        "small.en", "small",
        "base.en", "base",
        "tiny.en", "tiny",
    ];

    for dir in &search_dirs {
        if !dir.is_dir() {
            continue;
        }
        let entries: Vec<_> = std::fs::read_dir(dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name.starts_with("ggml-") && name.ends_with(".bin")
            })
            .collect();

        if entries.is_empty() {
            continue;
        }

        // Pick the best model by priority order.
        for prio in MODEL_PRIORITY {
            if let Some(entry) = entries.iter().find(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                // Match "ggml-{prio}.bin" or "ggml-{prio}-q*.bin" (quantized variants).
                name.starts_with(&format!("ggml-{prio}"))
            }) {
                let path = entry.path();
                log::info!("SttCache: found model at {}", path.display());
                return Some(path.to_string_lossy().to_string());
            }
        }

        // Fallback: pick the first ggml-*.bin file if no priority match.
        if let Some(entry) = entries.first() {
            let path = entry.path();
            log::info!("SttCache: found model at {}", path.display());
            return Some(path.to_string_lossy().to_string());
        }
    }
    None
}

/// Return the preferred download destination for STT models.
pub fn stt_model_dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(home).join(".local/share")
        });
    base.join("ultimateslice").join("models")
}

// ── Worker thread ─────────────────────────────────────────────────────────

fn stt_worker_loop(
    work_rx: mpsc::Receiver<SttJob>,
    result_tx: mpsc::SyncSender<WorkerUpdate>,
) {
    // Model is loaded lazily on first job and kept resident.
    #[cfg(feature = "speech-to-text")]
    let mut ctx: Option<whisper_rs::WhisperContext> = None;

    loop {
        let job = match work_rx.recv() {
            Ok(j) => j,
            Err(_) => break, // Channel closed.
        };

        let (success, segments, error) = run_stt_job(
            &job,
            #[cfg(feature = "speech-to-text")]
            &mut ctx,
        );

        let _ = result_tx.send(WorkerUpdate::Done(WorkerResult {
            cache_key: job.cache_key,
            segments,
            success,
            error,
        }));
    }
}

// ── STT inference (feature-gated) ─────────────────────────────────────────

#[cfg(feature = "speech-to-text")]
fn run_stt_job(
    job: &SttJob,
    ctx: &mut Option<whisper_rs::WhisperContext>,
) -> (bool, Vec<SubtitleSegment>, Option<String>) {
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

    // Load model if not already loaded (or if model path changed).
    if ctx.is_none() {
        log::info!("SttCache: loading whisper model from {}", job.model_path);
        match WhisperContext::new_with_params(&job.model_path, WhisperContextParameters::default())
        {
            Ok(c) => *ctx = Some(c),
            Err(e) => {
                let msg = format!("Failed to load whisper model: {e}");
                log::error!("SttCache: {msg}");
                return (false, Vec::new(), Some(msg));
            }
        }
    }

    let whisper_ctx = ctx.as_ref().unwrap();

    // Extract audio as 16kHz mono f32 PCM.
    let samples = match extract_audio_for_stt(&job.source_path, job.source_in_ns, job.source_out_ns)
    {
        Some(s) => s,
        None => {
            let msg = format!("Failed to extract audio from {}", job.source_path);
            log::error!("SttCache: {msg}");
            return (false, Vec::new(), Some(msg));
        }
    };

    // Configure whisper parameters.
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });

    // Language: "auto" or empty triggers auto-detection, otherwise use specified.
    if !job.language.is_empty() && job.language != "auto" {
        params.set_language(Some(&job.language));
    }

    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_token_timestamps(true);
    // Let whisper produce natural sentence-length segments (~30 tokens max).
    // Each segment will contain word-level timing via token timestamps.
    params.set_max_len(0); // 0 = no length limit (whisper uses its own segmenting).
    // Ensure segments break at word boundaries, not mid-word.
    params.set_split_on_word(true);

    // Run inference.
    let mut state = match whisper_ctx.create_state() {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("Failed to create whisper state: {e}");
            log::error!("SttCache: {msg}");
            return (false, Vec::new(), Some(msg));
        }
    };

    if let Err(e) = state.full(params, &samples) {
        let msg = format!("Whisper inference failed: {e}");
        log::error!("SttCache: {msg}");
        return (false, Vec::new(), Some(msg));
    }

    // Extract segments and word-level timestamps.
    let num_segments = state.full_n_segments().unwrap_or(0);
    let mut segments = Vec::with_capacity(num_segments as usize);

    for i in 0..num_segments {
        let start_ts = state.full_get_segment_t0(i).unwrap_or(0); // centiseconds
        let end_ts = state.full_get_segment_t1(i).unwrap_or(0);
        let raw_text = state
            .full_get_segment_text(i)
            .unwrap_or_default()
            .trim()
            .to_string();

        if raw_text.is_empty() {
            continue;
        }

        // Clean up whisper tokenization artifacts: collapse multiple spaces,
        // remove space before punctuation (e.g. "he 's" → "he's").
        let text = clean_whisper_text(&raw_text);

        // Convert centiseconds → nanoseconds.
        let start_ns = (start_ts as u64) * 10_000_000;
        let end_ns = (end_ts as u64) * 10_000_000;

        // Extract word-level timestamps from tokens.
        let mut words: Vec<SubtitleWord> = Vec::new();
        let n_tokens = state.full_n_tokens(i).unwrap_or(0);
        for t in 0..n_tokens {
            if let Ok(token_data) = state.full_get_token_data(i, t) {
                let token_text = state
                    .full_get_token_text(i, t)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if token_text.is_empty() || token_text.starts_with('[') {
                    continue; // Skip special tokens like [BLANK], [SOT], etc.
                }
                let w_start = (token_data.t0 as u64) * 10_000_000;
                let w_end = (token_data.t1 as u64) * 10_000_000;

                // Merge contraction suffixes ('s, 't, 'll, etc.) and
                // punctuation-only tokens into the previous word.
                // Handle both straight apostrophe (') and curly quote (\u{2019}).
                let is_contraction = (token_text.starts_with('\'') || token_text.starts_with('\u{2019}'))
                    && token_text.len() <= 4 // curly quote is multi-byte
                    && !words.is_empty();
                let is_punctuation = !words.is_empty()
                    && token_text.chars().all(|c| c.is_ascii_punctuation());
                if is_contraction || is_punctuation {
                    let prev = words.last_mut().unwrap();
                    prev.text.push_str(&token_text);
                    prev.end_ns = w_end;
                } else {
                    words.push(SubtitleWord {
                        start_ns: w_start,
                        end_ns: w_end,
                        text: token_text,
                    });
                }
            }
        }

        segments.push(SubtitleSegment {
            id: uuid::Uuid::new_v4().to_string(),
            start_ns,
            end_ns,
            text,
            words,
        });
    }

    log::info!(
        "SttCache: generated {} segments for {}",
        segments.len(),
        job.source_path
    );
    (true, segments, None)
}

/// Clean up whisper tokenization artifacts in segment text.
/// - Collapses multiple spaces
/// - Removes space before punctuation ("he 's" → "he's", "cute !" → "cute!")
/// - Trims leading/trailing whitespace
fn clean_whisper_text(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut prev_space = false;
    for ch in text.chars() {
        if ch == ' ' {
            prev_space = true;
            continue;
        }
        // Don't insert space before punctuation or contraction markers.
        if prev_space
            && !result.is_empty()
            && !matches!(ch, '\'' | '\u{2019}' | ',' | '.' | '!' | '?' | ';' | ':' | ')' | ']')
        {
            result.push(' ');
        }
        prev_space = false;
        result.push(ch);
    }
    result
}

/// Stub: when the `speech-to-text` feature is disabled.
#[cfg(not(feature = "speech-to-text"))]
fn run_stt_job(_job: &SttJob) -> (bool, Vec<SubtitleSegment>, Option<String>) {
    log::warn!("SttCache: speech-to-text feature not enabled");
    (false, Vec::new(), Some("Speech-to-text feature not compiled. Rebuild with: cargo build --features speech-to-text".to_string()))
}

// ── Audio extraction at 16kHz for whisper ─────────────────────────────────

/// Extract raw mono f32 audio at 16000 Hz from a media file via GStreamer.
/// Unlike `audio_sync::extract_raw_audio`, this extracts the full clip range
/// (no MAX_EXTRACT_SECONDS cap) since whisper needs the complete audio.
fn extract_audio_for_stt(
    path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
) -> Option<Vec<f32>> {
    use gstreamer as gst;
    use gstreamer::prelude::*;
    use gstreamer_app::AppSink;

    const WHISPER_SAMPLE_RATE: i32 = 16000;

    let uri = if path.starts_with("file://") {
        path.to_string()
    } else {
        format!("file://{path}")
    };

    let guard = super::PipelineGuard(gst::Pipeline::new());
    let pipeline = &guard.0;

    let src = gst::ElementFactory::make("uridecodebin")
        .property("uri", &uri)
        .build()
        .ok()?;
    let conv = gst::ElementFactory::make("audioconvert").build().ok()?;
    let resample = gst::ElementFactory::make("audioresample").build().ok()?;
    let capsf = gst::ElementFactory::make("capsfilter").build().ok()?;
    let sink = gst::ElementFactory::make("appsink").build().ok()?;

    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("channels", 1i32)
        .field("rate", WHISPER_SAMPLE_RATE)
        .build();
    capsf.set_property("caps", &caps);

    let appsink = sink.clone().dynamic_cast::<AppSink>().ok()?;
    appsink.set_property("sync", false);
    appsink.set_property("max-buffers", 200u32);
    appsink.set_property("drop", false);
    appsink.set_property("emit-signals", false);

    // Limit decoder threads to avoid overwhelming the system.
    if let Ok(src_bin) = src.clone().dynamic_cast::<gst::Bin>() {
        src_bin.connect_element_added(|_, element| {
            if element.find_property("max-threads").is_some() {
                element.set_property_from_str("max-threads", "1");
            }
            if element.find_property("threads").is_some() {
                element.set_property_from_str("threads", "1");
            }
        });
    }

    pipeline
        .add_many([&src, &conv, &resample, &capsf, &sink])
        .ok()?;
    gst::Element::link_many([&conv, &resample, &capsf, &sink]).ok()?;

    {
        let conv = conv.clone();
        src.connect_pad_added(move |_, pad| {
            let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
            let name = caps.structure(0).map(|s| s.name()).unwrap_or_default();
            if name.starts_with("audio/") {
                let sink_pad = conv.static_pad("sink").unwrap();
                if sink_pad.is_linked() {
                    return;
                }
                let _ = pad.link(&sink_pad);
            }
        });
    }

    // Paused first: wait for pads to link.
    pipeline.set_state(gst::State::Paused).ok()?;
    let _ = pipeline.state(Some(gst::ClockTime::from_seconds(5)));

    // Seek to source_in if needed.
    if source_in_ns > 0 {
        let seek_ok = pipeline.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::ClockTime::from_nseconds(source_in_ns),
        );
        if seek_ok.is_err() {
            log::error!("SttCache: seek to {}ns failed for {}", source_in_ns, path);
            return None;
        }
        let _ = pipeline.state(Some(gst::ClockTime::from_seconds(3)));
    }

    pipeline.set_state(gst::State::Playing).ok()?;

    // Compute max samples for the clip region.
    let clip_duration_s =
        source_out_ns.saturating_sub(source_in_ns) as f64 / 1_000_000_000.0;
    let max_samples =
        (clip_duration_s * WHISPER_SAMPLE_RATE as f64) as usize + WHISPER_SAMPLE_RATE as usize;

    let mut samples: Vec<f32> = Vec::new();
    let bus = pipeline.bus()?;

    loop {
        if let Some(s) = appsink.try_pull_sample(gst::ClockTime::from_mseconds(100)) {
            let buffer = s.buffer()?;
            let map = buffer.map_readable().ok()?;
            let raw_bytes = map.as_slice();
            let floats: &[f32] = unsafe {
                std::slice::from_raw_parts(raw_bytes.as_ptr() as *const f32, raw_bytes.len() / 4)
            };
            samples.extend_from_slice(floats);

            if samples.len() >= max_samples {
                break;
            }
        }

        if let Some(msg) = bus.pop() {
            use gst::MessageView;
            match msg.view() {
                MessageView::Eos(_) | MessageView::Error(_) => break,
                _ => {}
            }
        }

        if appsink.is_eos() {
            break;
        }
    }

    if samples.len() > max_samples {
        samples.truncate(max_samples);
    }

    if samples.is_empty() {
        return None;
    }

    log::info!(
        "SttCache: extracted {} samples ({:.1}s) from {}",
        samples.len(),
        samples.len() as f64 / WHISPER_SAMPLE_RATE as f64,
        path
    );
    Some(samples)
}
