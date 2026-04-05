// SPDX-License-Identifier: GPL-3.0-or-later
//! AI music generation via MusicGen-small ONNX models.
//!
//! Follows the [`super::bg_removal_cache::BgRemovalCache`] pattern: a single
//! worker thread loads three ONNX models (text encoder, decoder, EnCodec)
//! and generates WAV audio from text prompts.  Results are polled from the
//! GTK main thread and placed as audio clips on the timeline.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

// ── Public types ───────────────────────────────────────────────────────────

pub struct MusicGenJob {
    pub job_id: String,
    pub prompt: String,
    pub duration_secs: f64,
    pub output_path: PathBuf,
    pub track_id: String,
    pub timeline_start_ns: u64,
}

pub struct MusicGenResult {
    pub job_id: String,
    pub output_path: PathBuf,
    pub duration_ns: u64,
    pub track_id: String,
    pub timeline_start_ns: u64,
    pub success: bool,
    pub error: Option<String>,
}

pub struct MusicGenProgress {
    pub total: usize,
    pub completed: usize,
    pub in_flight: bool,
}

// ── Cache ──────────────────────────────────────────────────────────────────

pub struct MusicGenCache {
    model_dir: Option<PathBuf>,
    pending: HashSet<String>,
    total_requested: usize,
    total_completed: usize,
    result_rx: mpsc::Receiver<MusicGenResult>,
    work_tx: Option<mpsc::Sender<MusicGenJob>>,
    cache_root: PathBuf,
}

impl MusicGenCache {
    pub fn new() -> Self {
        let (result_tx, result_rx) = mpsc::sync_channel::<MusicGenResult>(8);
        let (work_tx, work_rx) = mpsc::channel::<MusicGenJob>();

        // Single worker thread — music generation is GPU/CPU-heavy.
        {
            let tx = result_tx;
            std::thread::spawn(move || {
                loop {
                    let job = match work_rx.recv() {
                        Ok(j) => j,
                        Err(_) => break,
                    };
                    // Catch panics so the worker thread survives model errors.
                    let result =
                        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            run_music_gen(&job)
                        })) {
                            Ok(r) => r,
                            Err(e) => {
                                let msg = if let Some(s) = e.downcast_ref::<&str>() {
                                    s.to_string()
                                } else if let Some(s) = e.downcast_ref::<String>() {
                                    s.clone()
                                } else {
                                    "unknown panic".to_string()
                                };
                                log::error!("MusicGenCache: worker panic: {msg}");
                                MusicGenResult {
                                    job_id: job.job_id.clone(),
                                    output_path: job.output_path.clone(),
                                    duration_ns: 0,
                                    track_id: job.track_id.clone(),
                                    timeline_start_ns: job.timeline_start_ns,
                                    success: false,
                                    error: Some(format!("Worker panic: {msg}")),
                                }
                            }
                        };
                    let _ = tx.send(result);
                }
            });
        }

        let cache_root = music_gen_cache_dir();
        let _ = std::fs::create_dir_all(&cache_root);

        let model_dir = find_model_dir();
        if model_dir.is_none() {
            log::info!(
                "MusicGenCache: MusicGen ONNX models not found; music generation disabled. \
                 Download musicgen-small ONNX models to ~/.local/share/ultimateslice/models/musicgen-small/"
            );
        }

        Self {
            model_dir,
            pending: HashSet::new(),
            total_requested: 0,
            total_completed: 0,
            result_rx,
            work_tx: Some(work_tx),
            cache_root,
        }
    }

    pub fn is_available(&self) -> bool {
        self.model_dir.is_some()
    }

    pub fn request(&mut self, mut job: MusicGenJob) {
        if self.model_dir.is_none() {
            return;
        }
        if self.pending.contains(&job.job_id) {
            return;
        }
        // Set output path in our cache directory.
        job.output_path = self.cache_root.join(format!("musicgen_{}.wav", job.job_id));
        self.pending.insert(job.job_id.clone());
        self.total_requested += 1;
        if let Some(ref tx) = self.work_tx {
            let _ = tx.send(job);
        }
    }

    pub fn poll(&mut self) -> Vec<MusicGenResult> {
        let mut results = Vec::new();
        while let Ok(result) = self.result_rx.try_recv() {
            self.pending.remove(&result.job_id);
            self.total_completed += 1;
            if result.success {
                log::info!(
                    "MusicGenCache: completed job={} path={}",
                    result.job_id,
                    result.output_path.display()
                );
            } else {
                log::warn!(
                    "MusicGenCache: failed job={} error={:?}",
                    result.job_id,
                    result.error
                );
            }
            results.push(result);
        }
        results
    }

    pub fn progress(&self) -> MusicGenProgress {
        MusicGenProgress {
            total: self.total_requested,
            completed: self.total_completed,
            in_flight: !self.pending.is_empty(),
        }
    }
}

// ── Model discovery ───────────────────────────────────────────────────────

const REQUIRED_FILES: &[&str] = &[
    "text_encoder.onnx",
    "decoder_model_merged.onnx",
    "encodec_decode.onnx",
    "tokenizer.json",
];

pub fn find_model_dir() -> Option<PathBuf> {
    let candidates = [
        // Next to executable
        std::env::current_exe()
            .ok()
            .map(|p| {
                p.parent()
                    .unwrap_or(Path::new("."))
                    .join("data/models/musicgen-small")
            })
            .unwrap_or_default(),
        // Development
        PathBuf::from("data/models/musicgen-small"),
        // Flatpak
        PathBuf::from("/app/share/ultimateslice/models/musicgen-small"),
        // XDG data home
        {
            let base = std::env::var("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    let home = std::env::var("HOME").unwrap_or_default();
                    PathBuf::from(home).join(".local/share")
                });
            base.join("ultimateslice/models/musicgen-small")
        },
    ];
    for dir in &candidates {
        if dir.is_dir() && REQUIRED_FILES.iter().all(|f| dir.join(f).exists()) {
            log::info!("MusicGenCache: found models at {}", dir.display());
            return Some(dir.clone());
        }
    }
    None
}

/// Returns the expected model directory path for user guidance in Preferences.
pub fn model_install_dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(home).join(".local/share")
        });
    base.join("ultimateslice/models/musicgen-small")
}

pub fn music_gen_cache_dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(home).join(".local/share")
        });
    base.join("ultimateslice/generated-music")
}

// ── Inference ─────────────────────────────────────────────────────────────

fn run_music_gen(job: &MusicGenJob) -> MusicGenResult {
    let result = run_music_gen_inner(job);
    match result {
        Ok((path, duration_ns)) => MusicGenResult {
            job_id: job.job_id.clone(),
            output_path: path,
            duration_ns,
            track_id: job.track_id.clone(),
            timeline_start_ns: job.timeline_start_ns,
            success: true,
            error: None,
        },
        Err(e) => MusicGenResult {
            job_id: job.job_id.clone(),
            output_path: job.output_path.clone(),
            duration_ns: 0,
            track_id: job.track_id.clone(),
            timeline_start_ns: job.timeline_start_ns,
            success: false,
            error: Some(e),
        },
    }
}

#[cfg(feature = "ai-inference")]
fn run_music_gen_inner(job: &MusicGenJob) -> Result<(PathBuf, u64), String> {
    use ort::session::Session;
    use ort::value::{Tensor, TensorRef};

    let model_dir = find_model_dir().ok_or("MusicGen models not found")?;

    let tokenizer_path = model_dir.join("tokenizer.json");
    let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)
        .map_err(|e| format!("Failed to load tokenizer: {e}"))?;

    let load_session = |name: &str| -> Result<Session, String> {
        Session::builder()
            .and_then(|b| {
                Ok(b.with_optimization_level(
                    ort::session::builder::GraphOptimizationLevel::Level3,
                )?)
            })
            .and_then(|mut b| b.commit_from_file(model_dir.join(name).to_string_lossy().as_ref()))
            .map_err(|e| format!("Failed to load {name}: {e}"))
    };

    let mut text_encoder = load_session("text_encoder.onnx")?;
    let mut decoder = load_session("decoder_model_merged.onnx")?;
    let mut encodec = load_session("encodec_decode.onnx")?;

    log::info!(
        "MusicGenCache: models loaded, generating for prompt={:?}",
        job.prompt
    );

    // ── Tokenize prompt ──────────────────────────────────────────────
    let encoding = tokenizer
        .encode(job.prompt.as_str(), true)
        .map_err(|e| format!("Tokenization failed: {e}"))?;
    let token_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
    let attn_mask: Vec<i64> = encoding
        .get_attention_mask()
        .iter()
        .map(|&m| m as i64)
        .collect();
    let enc_seq_len = token_ids.len();

    // ── Run text encoder ─────────────────────────────────────────────
    // input_ids: (1, enc_seq_len) i64
    // attention_mask: (1, enc_seq_len) i64
    let input_ids_arr = ndarray::Array2::<i64>::from_shape_vec((1, enc_seq_len), token_ids.clone())
        .map_err(|e| format!("input_ids shape: {e}"))?;
    let attn_mask_arr = ndarray::Array2::<i64>::from_shape_vec((1, enc_seq_len), attn_mask.clone())
        .map_err(|e| format!("attn_mask shape: {e}"))?;

    let enc_outputs = text_encoder
        .run(ort::inputs![
            "input_ids" => TensorRef::from_array_view(&input_ids_arr).map_err(|e| format!("{e}"))?,
            "attention_mask" => TensorRef::from_array_view(&attn_mask_arr).map_err(|e| format!("{e}"))?
        ])
        .map_err(|e| format!("text_encoder run: {e}"))?;

    // last_hidden_state: (1, enc_seq_len, 768)
    let encoder_hidden_data: Vec<f32> = {
        let val = enc_outputs
            .get("last_hidden_state")
            .ok_or("No text_encoder output")?;
        let (_shape, data) = val
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("extract encoder output: {e}"))?;
        data.to_vec()
    };
    let hidden_dim = encoder_hidden_data.len() / enc_seq_len; // should be 768

    log::info!("MusicGenCache: encoder done, hidden_dim={hidden_dim}, enc_seq_len={enc_seq_len}");

    // ── Decoder constants ────────────────────────────────────────────
    let num_codebooks: usize = 4;
    let num_layers: usize = 24;
    let num_heads: usize = 16;
    let head_dim: usize = 64;
    let vocab_size: usize = 2048;
    let max_new_tokens = (job.duration_secs * 50.0).ceil() as usize;
    let max_new_tokens = max_new_tokens.min(1500);
    let bos_token_id: i64 = 2048;

    // ── Autoregressive decoder loop ──────────────────────────────────
    // The merged decoder requires ALL inputs on every call:
    //   encoder_attention_mask: (1, enc_seq_len) i64
    //   input_ids: (num_codebooks, seq_len) i64
    //   encoder_hidden_states: (1, enc_seq_len, 768) f32
    //   past_key_values.{0..23}.{decoder,encoder}.{key,value}: f32
    //   use_cache_branch: (1,) bool
    //
    // Without KV cache, we pass zero-length past_key_values (dim 0 for seq axis)
    // and use_cache_branch=false. The model internally ignores the dummy values.

    let mut generated_tokens: Vec<Vec<i64>> = vec![vec![bos_token_id]; num_codebooks];

    // Encoder attention mask for decoder (stays constant).
    let enc_attn_mask = ndarray::Array2::<i64>::from_shape_vec((1, enc_seq_len), attn_mask)
        .map_err(|e| format!("enc_attn_mask: {e}"))?;

    // Encoder hidden states for decoder (stays constant).
    let enc_hidden =
        ndarray::Array3::<f32>::from_shape_vec((1, enc_seq_len, hidden_dim), encoder_hidden_data)
            .map_err(|e| format!("enc_hidden: {e}"))?;

    // KV cache: 24 layers × 4 tensors (decoder.key, decoder.value, encoder.key, encoder.value)
    // Each is shape (1, num_heads, seq_len, head_dim). Starts empty (seq_len=0).
    // After step 0 we store the outputs and reuse them.
    let mut kv_cache: Option<Vec<ndarray::Array4<f32>>> = None;
    // Order: for each layer: decoder.key, decoder.value, encoder.key, encoder.value

    let start_time = std::time::Instant::now();

    for step in 0..max_new_tokens {
        let use_cache = kv_cache.is_some();

        // input_ids: full sequence on step 0, last token only on subsequent steps.
        let dec_input_ids = if use_cache {
            let mut ids = Vec::with_capacity(num_codebooks);
            for cb in 0..num_codebooks {
                ids.push(*generated_tokens[cb].last().unwrap());
            }
            ndarray::Array2::<i64>::from_shape_vec((num_codebooks, 1), ids)
                .map_err(|e| format!("dec_input_ids: {e}"))?
        } else {
            let current_len = generated_tokens[0].len();
            let mut ids_flat = Vec::with_capacity(num_codebooks * current_len);
            for cb in 0..num_codebooks {
                ids_flat.extend_from_slice(&generated_tokens[cb]);
            }
            ndarray::Array2::<i64>::from_shape_vec((num_codebooks, current_len), ids_flat)
                .map_err(|e| format!("dec_input_ids: {e}"))?
        };

        let input_seq_len = dec_input_ids.shape()[1];

        let use_cache_arr = ndarray::Array1::<bool>::from_vec(vec![use_cache]);

        let mut inputs: Vec<(String, ort::value::DynValue)> = Vec::new();

        inputs.push((
            "encoder_attention_mask".into(),
            Tensor::from_array(enc_attn_mask.clone())
                .map_err(|e| format!("{e}"))?
                .into_dyn(),
        ));
        inputs.push((
            "input_ids".into(),
            Tensor::from_array(dec_input_ids)
                .map_err(|e| format!("{e}"))?
                .into_dyn(),
        ));
        inputs.push((
            "encoder_hidden_states".into(),
            Tensor::from_array(enc_hidden.clone())
                .map_err(|e| format!("{e}"))?
                .into_dyn(),
        ));

        // Add KV cache tensors (empty on step 0, populated on step 1+).
        if let Some(ref cache) = kv_cache {
            for (i, arr) in cache.iter().enumerate() {
                let layer = i / 4;
                let sub = i % 4;
                let (kind, kv) = match sub {
                    0 => ("decoder", "key"),
                    1 => ("decoder", "value"),
                    2 => ("encoder", "key"),
                    _ => ("encoder", "value"),
                };
                let name = format!("past_key_values.{layer}.{kind}.{kv}");
                inputs.push((
                    name,
                    Tensor::from_array(arr.clone())
                        .map_err(|e| format!("{e}"))?
                        .into_dyn(),
                ));
            }
        } else {
            let empty_kv = ndarray::Array4::<f32>::zeros((1, num_heads, 0, head_dim));
            for layer in 0..num_layers {
                for kind in ["decoder", "encoder"] {
                    for kv in ["key", "value"] {
                        let name = format!("past_key_values.{layer}.{kind}.{kv}");
                        inputs.push((
                            name,
                            Tensor::from_array(empty_kv.clone())
                                .map_err(|e| format!("{e}"))?
                                .into_dyn(),
                        ));
                    }
                }
            }
        }

        inputs.push((
            "use_cache_branch".into(),
            Tensor::from_array(use_cache_arr)
                .map_err(|e| format!("{e}"))?
                .into_dyn(),
        ));

        let dec_outputs = decoder
            .run(inputs)
            .map_err(|e| format!("decoder step {step}: {e}"))?;

        // Extract logits: (num_codebooks, input_seq_len, vocab_size)
        let logits_data = {
            let val = dec_outputs
                .get("logits")
                .ok_or("No decoder logits output")?;
            let (_shape, data) = val
                .try_extract_tensor::<f32>()
                .map_err(|e| format!("extract logits: {e}"))?;
            data.to_vec()
        };

        // Extract updated KV cache from present.{N}.{decoder,encoder}.{key,value}
        // When use_cache=true, the encoder KV outputs are dummy (batch=0) —
        // reuse the previous encoder KV unchanged; only update decoder KV.
        let prev_cache = kv_cache.take();
        let mut new_cache: Vec<ndarray::Array4<f32>> = Vec::with_capacity(num_layers * 4);
        for layer in 0..num_layers {
            for (sub_idx, (kind, kv)) in [
                ("decoder", "key"),
                ("decoder", "value"),
                ("encoder", "key"),
                ("encoder", "value"),
            ]
            .iter()
            .enumerate()
            {
                let cache_idx = layer * 4 + sub_idx;
                let is_encoder = *kind == "encoder";

                // When cached, encoder KV is passed through unchanged.
                if use_cache && is_encoder {
                    if let Some(ref prev) = prev_cache {
                        new_cache.push(prev[cache_idx].clone());
                        continue;
                    }
                }

                let name = format!("present.{layer}.{kind}.{kv}");
                if let Some(val) = dec_outputs.get(&name) {
                    let (shape, data) = val
                        .try_extract_tensor::<f32>()
                        .map_err(|e| format!("extract {name}: {e}"))?;
                    let shape_vec: Vec<usize> = shape.iter().map(|&s| s as usize).collect();
                    if shape_vec.len() == 4 && shape_vec[0] > 0 {
                        new_cache.push(
                            ndarray::Array4::from_shape_vec(
                                (shape_vec[0], shape_vec[1], shape_vec[2], shape_vec[3]),
                                data.to_vec(),
                            )
                            .map_err(|e| format!("reshape {name}: {e}"))?,
                        );
                    } else {
                        // Dummy output (batch=0) — reuse previous if available.
                        if let Some(ref prev) = prev_cache {
                            new_cache.push(prev[cache_idx].clone());
                        } else {
                            new_cache.push(ndarray::Array4::zeros((1, num_heads, 0, head_dim)));
                        }
                    }
                } else {
                    if let Some(ref prev) = prev_cache {
                        new_cache.push(prev[cache_idx].clone());
                    } else {
                        new_cache.push(ndarray::Array4::zeros((1, num_heads, 0, head_dim)));
                    }
                }
            }
        }
        kv_cache = Some(new_cache);

        // Take the last position's logits for each codebook.
        for cb in 0..num_codebooks {
            if step < cb {
                generated_tokens[cb].push(bos_token_id);
                continue;
            }
            let offset = (cb * input_seq_len + (input_seq_len - 1)) * vocab_size;
            if offset + vocab_size > logits_data.len() {
                generated_tokens[cb].push(bos_token_id);
                continue;
            }
            let next_token = sample_top_k(&logits_data[offset..offset + vocab_size], 250, 1.0);
            generated_tokens[cb].push(next_token);
        }

        if step % 50 == 0 {
            let elapsed = start_time.elapsed().as_secs_f64();
            let steps_per_sec = if elapsed > 0.0 {
                (step + 1) as f64 / elapsed
            } else {
                0.0
            };
            log::info!(
                "MusicGenCache: step {}/{} ({:.1}s/{:.1}s) [{:.1} steps/s]",
                step,
                max_new_tokens,
                step as f64 / 50.0,
                job.duration_secs,
                steps_per_sec
            );
        }
    }

    // ── Remove delay pattern offset ──────────────────────────────────
    let min_len = generated_tokens.iter().map(|t| t.len()).min().unwrap_or(0);
    let mut aligned: Vec<Vec<i64>> = Vec::with_capacity(num_codebooks);
    for cb in 0..num_codebooks {
        let skip = cb + 1;
        let tokens: Vec<i64> = generated_tokens[cb]
            .iter()
            .skip(skip)
            .take(min_len.saturating_sub(skip))
            .copied()
            .collect();
        aligned.push(tokens);
    }
    let output_len = aligned.iter().map(|t| t.len()).min().unwrap_or(0);
    if output_len == 0 {
        return Err("No tokens generated".to_string());
    }

    // ── Run EnCodec decoder ──────────────────────────────────────────
    // audio_codes input: (1, 1, 4, output_len) i64
    let mut codes_flat = Vec::with_capacity(num_codebooks * output_len);
    for cb in 0..num_codebooks {
        codes_flat.extend_from_slice(&aligned[cb][..output_len]);
    }
    let codes_arr =
        ndarray::Array4::<i64>::from_shape_vec((1, 1, num_codebooks, output_len), codes_flat)
            .map_err(|e| format!("encodec codes shape: {e}"))?;

    let codes_tensor = Tensor::from_array(codes_arr).map_err(|e| format!("codes tensor: {e}"))?;
    let enc_dec_outputs = encodec
        .run(ort::inputs!["audio_codes" => codes_tensor])
        .map_err(|e| format!("encodec_decode: {e}"))?;

    let audio_samples: Vec<f32> = {
        let val = enc_dec_outputs
            .get("audio_values")
            .ok_or("No encodec output")?;
        let (_shape, data) = val
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("extract audio: {e}"))?;
        data.to_vec()
    };

    if audio_samples.is_empty() {
        return Err("EnCodec produced no audio samples".to_string());
    }

    // ── Write WAV file ───────────────────────────────────────────────
    let sample_rate = 32000u32;
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let temp_path = job.output_path.with_extension("partial.wav");
    {
        let mut writer =
            hound::WavWriter::create(&temp_path, spec).map_err(|e| format!("WAV create: {e}"))?;
        for &s in &audio_samples {
            writer
                .write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)
                .map_err(|e| format!("WAV write: {e}"))?;
        }
        writer
            .finalize()
            .map_err(|e| format!("WAV finalize: {e}"))?;
    }
    std::fs::rename(&temp_path, &job.output_path).map_err(|e| format!("WAV rename: {e}"))?;

    let duration_ns = (audio_samples.len() as f64 / sample_rate as f64 * 1_000_000_000.0) as u64;
    log::info!(
        "MusicGenCache: wrote {} samples ({:.1}s) to {}",
        audio_samples.len(),
        audio_samples.len() as f64 / sample_rate as f64,
        job.output_path.display()
    );
    Ok((job.output_path.clone(), duration_ns))
}

#[cfg(not(feature = "ai-inference"))]
fn run_music_gen_inner(_job: &MusicGenJob) -> Result<(PathBuf, u64), String> {
    Err("Music generation requires the ai-inference feature".to_string())
}

// ── Sampling ──────────────────────────────────────────────────────────────

fn sample_top_k(logits: &[f32], k: usize, temperature: f32) -> i64 {
    let k = k.min(logits.len());

    // Apply temperature.
    let scaled: Vec<f32> = logits.iter().map(|&l| l / temperature).collect();

    // Find top-k indices.
    let mut indexed: Vec<(usize, f32)> = scaled.iter().copied().enumerate().collect();
    indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    indexed.truncate(k);

    // Softmax over top-k.
    let max_val = indexed[0].1;
    let exps: Vec<f32> = indexed.iter().map(|&(_, v)| (v - max_val).exp()).collect();
    let sum: f32 = exps.iter().sum();
    let probs: Vec<f32> = exps.iter().map(|&e| e / sum).collect();

    // Sample from the distribution using a simple random approach.
    // Use a basic LCG since we don't need cryptographic randomness.
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    seed ^= logits.len() as u64;
    seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let rand_val = (seed >> 33) as f32 / (1u64 << 31) as f32;

    let mut cumulative = 0.0f32;
    for (i, &p) in probs.iter().enumerate() {
        cumulative += p;
        if rand_val < cumulative {
            return indexed[i].0 as i64;
        }
    }
    indexed.last().map(|&(idx, _)| idx as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_music_gen_inference() {
        env_logger::builder().is_test(true).try_init().ok();
        if find_model_dir().is_none() {
            eprintln!("SKIP: MusicGen models not installed");
            return;
        }
        let dir = music_gen_cache_dir();
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("test_output.wav");
        let job = MusicGenJob {
            job_id: "test".into(),
            prompt: "calm piano".into(),
            duration_secs: 1.0,
            output_path: out.clone(),
            track_id: "t".into(),
            timeline_start_ns: 0,
        };
        let result = run_music_gen(&job);
        eprintln!("success={} error={:?}", result.success, result.error);
        if !result.success {
            panic!("Music gen failed: {:?}", result.error);
        }
        assert!(out.exists(), "WAV file should exist");
        let _ = std::fs::remove_file(&out);
    }
}
