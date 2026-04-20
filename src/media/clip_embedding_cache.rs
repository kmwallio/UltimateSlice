// SPDX-License-Identifier: GPL-3.0-or-later
//! Background visual-search embedding cache.
//!
//! A small worker pool samples a handful of representative frames from a media
//! item, runs a CLIP-style image encoder over those frames, and stores the
//! normalized embedding vectors as JSON under the user's cache directory.
//! Query-time text embeddings are computed lazily through the paired text
//! encoder and cached per-thread so Media Library search can reuse them across
//! repeated ranking calls for the same query.

use crate::model::media_library::{MediaVisualEmbedding, MediaVisualEmbeddingFrame};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

const CACHE_VERSION: &str = "clip-search-v1";
const FRAME_SIZE: usize = 224;
const FRAME_BYTES: usize = FRAME_SIZE * FRAME_SIZE * 3;
const VISUAL_MATCH_THRESHOLD: f32 = 0.18;

#[derive(Clone, Debug)]
enum WorkerUpdate {
    Done(WorkerResult),
}

#[derive(Clone, Debug)]
struct WorkerResult {
    cache_key: String,
    embedding: Option<MediaVisualEmbedding>,
    success: bool,
}

#[derive(Clone, Debug)]
struct ClipEmbeddingJob {
    cache_key: String,
    source_path: String,
    duration_ns: u64,
    is_image: bool,
    output_path: String,
    model_paths: ClipSearchModelPaths,
}

pub struct ClipEmbeddingProgress {
    pub total: usize,
    pub completed: usize,
    pub in_flight: bool,
}

pub struct ClipEmbeddingPollResult {
    pub source_path: String,
    pub embedding: MediaVisualEmbedding,
}

pub enum ClipEmbeddingRequest {
    Skipped,
    Queued,
    Ready(ClipEmbeddingPollResult),
}

#[derive(Clone, Debug, PartialEq)]
pub struct VisualSearchMatch {
    pub score: i32,
    pub similarity: f32,
    pub best_frame_time_ns: Option<u64>,
}

pub struct ClipEmbeddingCache {
    embeddings: HashMap<String, MediaVisualEmbedding>,
    source_to_key: HashMap<String, String>,
    pending: HashSet<String>,
    failed: HashSet<String>,
    key_to_source: HashMap<String, String>,
    total_requested: usize,
    total_completed: usize,
    result_rx: mpsc::Receiver<WorkerUpdate>,
    work_tx: Option<mpsc::Sender<ClipEmbeddingJob>>,
    cache_root: PathBuf,
    model_paths: Option<ClipSearchModelPaths>,
}

impl ClipEmbeddingCache {
    pub fn new() -> Self {
        let (result_tx, result_rx) = mpsc::sync_channel::<WorkerUpdate>(32);
        let (work_tx, work_rx) = mpsc::channel::<ClipEmbeddingJob>();

        std::thread::spawn(move || {
            while let Ok(job) = work_rx.recv() {
                let embedding = run_clip_embedding_job(&job);
                let success = embedding.is_some();
                let _ = result_tx.send(WorkerUpdate::Done(WorkerResult {
                    cache_key: job.cache_key,
                    embedding,
                    success,
                }));
            }
        });

        let cache_root = crate::media::cache_support::cache_root_dir("clip_embeddings");
        let _ = std::fs::create_dir_all(&cache_root);
        let model_paths = find_model_paths();
        if model_paths.is_none() {
            log::info!(
                "ClipEmbeddingCache: CLIP search models not found; visual search indexing disabled"
            );
        }

        Self {
            embeddings: HashMap::new(),
            source_to_key: HashMap::new(),
            pending: HashSet::new(),
            failed: HashSet::new(),
            key_to_source: HashMap::new(),
            total_requested: 0,
            total_completed: 0,
            result_rx,
            work_tx: Some(work_tx),
            cache_root,
            model_paths,
        }
    }

    pub fn is_available(&self) -> bool {
        self.model_paths.is_some()
    }

    pub fn refresh_model_paths(&mut self) {
        self.model_paths = find_model_paths();
    }

    pub fn request(
        &mut self,
        source_path: &str,
        duration_ns: u64,
        is_image: bool,
    ) -> ClipEmbeddingRequest {
        let Some(model_paths) = self.model_paths.clone() else {
            return ClipEmbeddingRequest::Skipped;
        };
        let key = cache_key(source_path, duration_ns, is_image, &model_paths);
        if self.pending.contains(&key) || self.failed.contains(&key) {
            return ClipEmbeddingRequest::Skipped;
        }
        if self.source_to_key.get(source_path) == Some(&key)
            && self.embeddings.contains_key(source_path)
        {
            return ClipEmbeddingRequest::Skipped;
        }

        let output_path = self.output_path_for_key(&key);
        if embedding_file_is_ready(&output_path) {
            if let Some(embedding) = load_embedding_file(&output_path) {
                self.source_to_key
                    .insert(source_path.to_string(), key.clone());
                self.embeddings
                    .insert(source_path.to_string(), embedding.clone());
                return ClipEmbeddingRequest::Ready(ClipEmbeddingPollResult {
                    source_path: source_path.to_string(),
                    embedding,
                });
            }
            let _ = std::fs::remove_file(&output_path);
        }

        self.total_requested += 1;
        self.pending.insert(key.clone());
        self.key_to_source
            .insert(key.clone(), source_path.to_string());
        self.source_to_key
            .insert(source_path.to_string(), key.clone());
        if let Some(ref tx) = self.work_tx {
            if tx
                .send(ClipEmbeddingJob {
                    cache_key: key,
                    source_path: source_path.to_string(),
                    duration_ns,
                    is_image,
                    output_path,
                    model_paths,
                })
                .is_ok()
            {
                return ClipEmbeddingRequest::Queued;
            }
        }
        ClipEmbeddingRequest::Skipped
    }

    pub fn poll(&mut self) -> Vec<ClipEmbeddingPollResult> {
        let mut resolved = Vec::new();
        while let Ok(update) = self.result_rx.try_recv() {
            match update {
                WorkerUpdate::Done(result) => {
                    self.pending.remove(&result.cache_key);
                    self.total_completed += 1;
                    if result.success {
                        if let Some(source_path) = self.key_to_source.remove(&result.cache_key) {
                            if let Some(embedding) = result.embedding {
                                if self.source_to_key.get(&source_path) == Some(&result.cache_key) {
                                    self.embeddings
                                        .insert(source_path.clone(), embedding.clone());
                                    resolved.push(ClipEmbeddingPollResult {
                                        source_path,
                                        embedding,
                                    });
                                }
                            }
                        }
                    } else {
                        log::warn!("ClipEmbeddingCache: failed key={}", result.cache_key);
                        self.failed.insert(result.cache_key);
                    }
                }
            }
        }
        resolved
    }

    pub fn progress(&self) -> ClipEmbeddingProgress {
        ClipEmbeddingProgress {
            total: self.total_requested,
            completed: self.total_completed,
            in_flight: !self.pending.is_empty(),
        }
    }

    fn output_path_for_key(&self, key: &str) -> String {
        self.cache_root
            .join(format!("{key}.json"))
            .to_string_lossy()
            .to_string()
    }
}

impl Drop for ClipEmbeddingCache {
    fn drop(&mut self) {
        self.work_tx.take();
    }
}

pub fn cache_root_dir() -> PathBuf {
    crate::media::cache_support::cache_root_dir("clip_embeddings")
}

pub fn clip_search_model_install_dir() -> PathBuf {
    ClipSearchModelPaths::model_install_dir()
}

pub fn visual_search_match(
    query: &str,
    embedding: &MediaVisualEmbedding,
) -> Option<VisualSearchMatch> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    let query_embedding = encode_text_query(query)?;
    best_visual_frame_match(&query_embedding, embedding)
}

pub(crate) fn clip_search_models_available() -> bool {
    find_model_paths().is_some()
}

pub(crate) fn text_query_embedding(query: &str) -> Option<Vec<f32>> {
    encode_text_query(query)
}

pub(crate) fn best_visual_frame_match(
    query_embedding: &[f32],
    embedding: &MediaVisualEmbedding,
) -> Option<VisualSearchMatch> {
    if query_embedding.is_empty() {
        return None;
    }
    let mut best_time = None;
    let mut best_similarity = f32::NEG_INFINITY;
    for frame in &embedding.frames {
        let similarity = cosine_similarity(query_embedding, &frame.embedding)?;
        if similarity > best_similarity {
            best_similarity = similarity;
            best_time = Some(frame.time_ns);
        }
    }
    if !best_similarity.is_finite() || best_similarity < VISUAL_MATCH_THRESHOLD {
        return None;
    }
    let score = (480.0 + (best_similarity - VISUAL_MATCH_THRESHOLD) * 620.0)
        .round()
        .clamp(480.0, 760.0) as i32;
    Some(VisualSearchMatch {
        score,
        similarity: best_similarity,
        best_frame_time_ns: best_time,
    })
}

#[derive(Clone, Debug)]
struct ClipSearchModelPaths {
    image_encoder: PathBuf,
    text_encoder: PathBuf,
    tokenizer: PathBuf,
    signature: String,
}

impl ClipSearchModelPaths {
    fn model_install_dir() -> PathBuf {
        let base = std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_default();
                PathBuf::from(home).join(".local/share")
            });
        base.join("ultimateslice").join("models")
    }
}

const MODEL_DIR_NAMES: &[&str] = &["clip-search", "clip_search", "clip-vit", "clip_vit"];
const IMAGE_MODEL_FILENAMES: &[&str] = &[
    "image_encoder.onnx",
    "vision_encoder.onnx",
    "vision_model.onnx",
    "clip_image_encoder.onnx",
];
const TEXT_MODEL_FILENAMES: &[&str] = &[
    "text_encoder.onnx",
    "text_model.onnx",
    "text_encoder_model.onnx",
    "clip_text_encoder.onnx",
];
const TOKENIZER_FILENAMES: &[&str] = &["tokenizer.json"];

fn find_model_paths() -> Option<ClipSearchModelPaths> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let mut dirs = Vec::new();
    if let Some(exe_dir) = exe_dir {
        dirs.push(exe_dir.join("data/models"));
    }
    dirs.push(PathBuf::from("data/models"));
    dirs.push(PathBuf::from("/app/share/ultimateslice/models"));
    dirs.push(ClipSearchModelPaths::model_install_dir());

    for base in dirs {
        for dir_name in MODEL_DIR_NAMES {
            let model_dir = base.join(dir_name);
            if !model_dir.is_dir() {
                continue;
            }
            let Some(image_encoder) = first_existing_path(&model_dir, IMAGE_MODEL_FILENAMES) else {
                continue;
            };
            let Some(text_encoder) = first_existing_path(&model_dir, TEXT_MODEL_FILENAMES) else {
                continue;
            };
            let Some(tokenizer) = first_existing_path(&model_dir, TOKENIZER_FILENAMES) else {
                continue;
            };
            let signature = format!(
                "{}:{}:{}:{}",
                model_dir.display(),
                crate::media::cache_key::source_mtime_secs(
                    image_encoder.to_string_lossy().as_ref()
                ),
                crate::media::cache_key::source_mtime_secs(text_encoder.to_string_lossy().as_ref()),
                crate::media::cache_key::source_mtime_secs(tokenizer.to_string_lossy().as_ref()),
            );
            return Some(ClipSearchModelPaths {
                image_encoder,
                text_encoder,
                tokenizer,
                signature,
            });
        }
    }
    None
}

fn first_existing_path(dir: &Path, candidates: &[&str]) -> Option<PathBuf> {
    candidates
        .iter()
        .map(|candidate| dir.join(candidate))
        .find(|path| path.exists())
}

fn cache_key(
    source_path: &str,
    duration_ns: u64,
    is_image: bool,
    model_paths: &ClipSearchModelPaths,
) -> String {
    crate::media::cache_key::hashed_key("clip_embed", |key| {
        key.add(CACHE_VERSION)
            .add_source_fingerprint(source_path)
            .add(duration_ns / 1_000_000_000)
            .add(is_image)
            .add(model_paths.signature.as_str());
    })
}

fn embedding_file_is_ready(path: &str) -> bool {
    crate::media::cache_support::file_has_content(path)
}

fn load_embedding_file(path: &str) -> Option<MediaVisualEmbedding> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<MediaVisualEmbedding>(&text).ok()
}

fn save_embedding_file(path: &str, embedding: &MediaVisualEmbedding) -> bool {
    let temp_path = format!("{path}.partial");
    let data = match serde_json::to_vec(embedding) {
        Ok(data) => data,
        Err(err) => {
            log::error!("ClipEmbeddingCache: failed to serialize embedding: {err}");
            return false;
        }
    };
    if std::fs::write(&temp_path, data).is_err() {
        return false;
    }
    std::fs::rename(&temp_path, path).is_ok()
}

fn run_clip_embedding_job(job: &ClipEmbeddingJob) -> Option<MediaVisualEmbedding> {
    #[cfg(feature = "ai-inference")]
    {
        let embedding = encode_media_item_frames(
            &job.source_path,
            job.duration_ns,
            job.is_image,
            &job.model_paths,
        )?;
        if !save_embedding_file(&job.output_path, &embedding) {
            log::warn!(
                "ClipEmbeddingCache: failed to persist embedding cache for {}",
                job.source_path
            );
        }
        return Some(embedding);
    }

    #[cfg(not(feature = "ai-inference"))]
    {
        let _ = job;
        log::warn!(
            "ClipEmbeddingCache: ai-inference feature not enabled; cannot build visual search embeddings"
        );
        None
    }
}

#[cfg(feature = "ai-inference")]
fn encode_media_item_frames(
    source_path: &str,
    duration_ns: u64,
    is_image: bool,
    model_paths: &ClipSearchModelPaths,
) -> Option<MediaVisualEmbedding> {
    use ort::session::Session;

    use super::ai_providers;
    let mut session = Session::builder()
        .and_then(|b| {
            Ok(b.with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)?)
        })
        .and_then(|b: ort::session::builder::SessionBuilder| {
            ai_providers::configure_session_builder(b, ai_providers::current_backend())
        })
        .and_then(|mut b| b.commit_from_file(&model_paths.image_encoder))
        .ok()?;

    let mut frames = Vec::new();
    for time_ns in representative_frame_times(duration_ns, is_image) {
        let rgb = extract_rgb_frame(source_path, time_ns, is_image).ok()?;
        let embedding = encode_image_frame(&mut session, &rgb).ok()?;
        frames.push(MediaVisualEmbeddingFrame { time_ns, embedding });
    }
    (!frames.is_empty()).then(|| MediaVisualEmbedding {
        frames,
        model_id: model_paths.signature.clone(),
    })
}

#[cfg(feature = "ai-inference")]
fn encode_image_frame(session: &mut ort::session::Session, rgb: &[u8]) -> Result<Vec<f32>, String> {
    use ndarray::Array4;
    use ort::value::TensorRef;

    if rgb.len() != FRAME_BYTES {
        return Err(format!(
            "expected {FRAME_BYTES} RGB bytes, got {}",
            rgb.len()
        ));
    }
    let mut input = Array4::<f32>::zeros((1, 3, FRAME_SIZE, FRAME_SIZE));
    let means = [0.48145466_f32, 0.4578275_f32, 0.40821073_f32];
    let stds = [0.26862954_f32, 0.26130258_f32, 0.2757771_f32];
    for y in 0..FRAME_SIZE {
        for x in 0..FRAME_SIZE {
            let base = (y * FRAME_SIZE + x) * 3;
            for channel in 0..3 {
                let pixel = rgb[base + channel] as f32 / 255.0;
                input[[0, channel, y, x]] = (pixel - means[channel]) / stds[channel];
            }
        }
    }

    let outputs = session
        .run(ort::inputs![
            "pixel_values" => TensorRef::from_array_view(&input).map_err(|err| err.to_string())?
        ])
        .map_err(|err| format!("image encoder run failed: {err}"))?;
    extract_embedding_tensor(
        &outputs,
        &[
            "image_embeds",
            "image_embeddings",
            "image_features",
            "pooled_output",
            "pooler_output",
            "last_hidden_state",
        ],
    )
}

#[cfg(feature = "ai-inference")]
fn extract_embedding_tensor(
    outputs: &ort::session::SessionOutputs,
    candidate_names: &[&str],
) -> Result<Vec<f32>, String> {
    for name in candidate_names {
        if let Some(value) = outputs.get(name) {
            let (shape, data) = value
                .try_extract_tensor::<f32>()
                .map_err(|err| format!("extract '{name}' failed: {err}"))?;
            let mut embedding =
                reshape_embedding_tensor(shape.iter().copied().collect(), data.to_vec())?;
            normalize_embedding(&mut embedding);
            return Ok(embedding);
        }
    }
    Err(format!(
        "unsupported CLIP output names; tried {}",
        candidate_names.join(", ")
    ))
}

#[cfg(feature = "ai-inference")]
fn reshape_embedding_tensor(shape: Vec<i64>, data: Vec<f32>) -> Result<Vec<f32>, String> {
    match shape.as_slice() {
        [dim] => Ok(data),
        [1, dim] => Ok(data),
        [1, seq_len, dim] if *seq_len > 0 && *dim > 0 => {
            let seq_len = *seq_len as usize;
            let dim = *dim as usize;
            let mut pooled = vec![0.0_f32; dim];
            for seq in 0..seq_len {
                for idx in 0..dim {
                    pooled[idx] += data[seq * dim + idx];
                }
            }
            for value in &mut pooled {
                *value /= seq_len as f32;
            }
            Ok(pooled)
        }
        [1, 1, dim] if *dim > 0 => Ok(data),
        _ => Err(format!("unsupported CLIP embedding shape: {shape:?}")),
    }
}

#[cfg(feature = "ai-inference")]
fn extract_rgb_frame(source_path: &str, time_ns: u64, is_image: bool) -> Result<Vec<u8>, String> {
    let timestamp = format!("{:.3}", time_ns as f64 / 1_000_000_000.0);
    let mut command = std::process::Command::new("ffmpeg");
    command.args(["-hide_banner", "-loglevel", "error"]);
    if !is_image && time_ns > 0 {
        command.args(["-ss", &timestamp]);
    }
    command.args([
        "-i",
        source_path,
        "-frames:v",
        "1",
        "-vf",
        "scale=224:224:force_original_aspect_ratio=increase,crop=224:224",
        "-pix_fmt",
        "rgb24",
        "-f",
        "rawvideo",
        "-",
    ]);
    let output = command
        .output()
        .map_err(|err| format!("ffmpeg spawn failed: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "ffmpeg frame extract failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    if output.stdout.len() != FRAME_BYTES {
        return Err(format!(
            "unexpected frame size: expected {FRAME_BYTES}, got {}",
            output.stdout.len()
        ));
    }
    Ok(output.stdout)
}

fn representative_frame_times(duration_ns: u64, is_image: bool) -> Vec<u64> {
    if is_image || duration_ns == 0 {
        return vec![0];
    }
    let mut times = [10_u64, 35, 65, 90]
        .into_iter()
        .map(|percent| {
            duration_ns
                .saturating_mul(percent)
                .saturating_div(100)
                .min(duration_ns.saturating_sub(1))
        })
        .collect::<Vec<_>>();
    times.sort_unstable();
    times.dedup();
    if times.is_empty() {
        times.push(0);
    }
    times
}

fn normalize_embedding(embedding: &mut [f32]) {
    let norm = embedding
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt();
    if norm <= f32::EPSILON {
        return;
    }
    for value in embedding {
        *value /= norm;
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.is_empty() || a.len() != b.len() {
        return None;
    }
    Some(a.iter().zip(b).map(|(lhs, rhs)| lhs * rhs).sum())
}

#[cfg(feature = "ai-inference")]
struct TextSearchEncoder {
    model_signature: String,
    tokenizer: tokenizers::Tokenizer,
    session: ort::session::Session,
    cached_queries: HashMap<String, Option<Vec<f32>>>,
}

#[cfg(feature = "ai-inference")]
impl TextSearchEncoder {
    fn load(model_paths: &ClipSearchModelPaths) -> Result<Self, String> {
        use ort::session::Session;

        use super::ai_providers;
        let tokenizer = tokenizers::Tokenizer::from_file(&model_paths.tokenizer)
            .map_err(|err| format!("failed to load CLIP tokenizer: {err}"))?;
        let session = Session::builder()
            .and_then(|b| {
                Ok(b.with_optimization_level(
                    ort::session::builder::GraphOptimizationLevel::Level3,
                )?)
            })
            .and_then(|b: ort::session::builder::SessionBuilder| {
                ai_providers::configure_session_builder(b, ai_providers::current_backend())
            })
            .and_then(|mut b| b.commit_from_file(&model_paths.text_encoder))
            .map_err(|err| format!("failed to load CLIP text encoder: {err}"))?;
        Ok(Self {
            model_signature: model_paths.signature.clone(),
            tokenizer,
            session,
            cached_queries: HashMap::new(),
        })
    }

    fn encode_query(&mut self, query: &str) -> Option<Vec<f32>> {
        if let Some(cached) = self.cached_queries.get(query) {
            return cached.clone();
        }
        let encoded = self.encode_query_inner(query).ok();
        self.cached_queries
            .insert(query.to_string(), encoded.clone());
        encoded
    }

    fn encode_query_inner(&mut self, query: &str) -> Result<Vec<f32>, String> {
        use ndarray::Array2;
        use ort::value::TensorRef;

        let encoding = self
            .tokenizer
            .encode(query, true)
            .map_err(|err| format!("CLIP query tokenization failed: {err}"))?;
        let token_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let attn_mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&value| value as i64)
            .collect();
        if token_ids.is_empty() {
            return Err("query tokenization produced no tokens".to_string());
        }

        let input_ids = Array2::<i64>::from_shape_vec((1, token_ids.len()), token_ids)
            .map_err(|err| err.to_string())?;
        let attention_mask = Array2::<i64>::from_shape_vec((1, attn_mask.len()), attn_mask)
            .map_err(|err| err.to_string())?;

        let outputs = self
            .session
            .run(ort::inputs![
                "input_ids" => TensorRef::from_array_view(&input_ids).map_err(|err| err.to_string())?,
                "attention_mask" => TensorRef::from_array_view(&attention_mask).map_err(|err| err.to_string())?
            ])
            .map_err(|err| format!("CLIP text encoder run failed: {err}"))?;
        extract_embedding_tensor(
            &outputs,
            &[
                "text_embeds",
                "text_embeddings",
                "sentence_embedding",
                "pooled_output",
                "pooler_output",
                "last_hidden_state",
            ],
        )
    }
}

#[cfg(feature = "ai-inference")]
fn encode_text_query(query: &str) -> Option<Vec<f32>> {
    use std::cell::RefCell;

    thread_local! {
        static TEXT_SEARCH_ENCODER: RefCell<Option<TextSearchEncoder>> = RefCell::new(None);
    }

    let model_paths = find_model_paths()?;
    TEXT_SEARCH_ENCODER.with(|cell| {
        let mut encoder = cell.borrow_mut();
        let needs_reload = encoder
            .as_ref()
            .map(|current| current.model_signature != model_paths.signature)
            .unwrap_or(true);
        if needs_reload {
            *encoder = TextSearchEncoder::load(&model_paths).ok();
        }
        encoder.as_mut()?.encode_query(query)
    })
}

#[cfg(not(feature = "ai-inference"))]
fn encode_text_query(_query: &str) -> Option<Vec<f32>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn representative_frame_times_cover_middle_of_clip() {
        let times = representative_frame_times(10_000_000_000, false);
        assert_eq!(times.len(), 4);
        assert_eq!(times[0], 1_000_000_000);
        assert_eq!(times[3], 9_000_000_000);
    }

    #[test]
    fn representative_frame_times_collapse_for_stills() {
        assert_eq!(representative_frame_times(5_000_000_000, true), vec![0]);
    }

    #[test]
    fn best_visual_frame_match_prefers_highest_similarity() {
        let embedding = MediaVisualEmbedding {
            model_id: "test".to_string(),
            frames: vec![
                MediaVisualEmbeddingFrame {
                    time_ns: 1_000_000_000,
                    embedding: vec![1.0, 0.0],
                },
                MediaVisualEmbeddingFrame {
                    time_ns: 2_000_000_000,
                    embedding: vec![0.0, 1.0],
                },
            ],
        };
        let matched = best_visual_frame_match(&[0.0, 1.0], &embedding).expect("visual match");
        assert_eq!(matched.best_frame_time_ns, Some(2_000_000_000));
        assert!(matched.score >= 480);
    }
}
