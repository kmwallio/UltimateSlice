// SPDX-License-Identifier: GPL-3.0-or-later
//! Offline AI background removal cache.
//!
//! Modeled after [`super::proxy_cache::ProxyCache`]: a fixed-size thread pool
//! decodes each frame of a source clip, runs an ONNX segmentation model
//! (MODNet) to produce an alpha matte, and encodes the result as a
//! VP9-with-alpha WebM file.  Both preview and export consume the
//! pre-processed file — guaranteeing exact visual match.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
#[cfg(feature = "ai-inference")]
use std::process::Command;
use std::sync::mpsc;

// ── Public types ───────────────────────────────────────────────────────────

/// Progress update from a background worker.
enum WorkerUpdate {
    /// Job completed (success or failure).
    Done(WorkerResult),
}

struct WorkerResult {
    cache_key: String,
    output_path: String,
    success: bool,
}

/// A single queued job.
struct BgRemovalJob {
    cache_key: String,
    source_path: String,
    output_path: String,
    threshold: f64,
    model_path: String,
}

/// Aggregate progress for the status bar.
pub struct BgRemovalProgress {
    pub total: usize,
    pub completed: usize,
    pub in_flight: bool,
}

// ── Cache ──────────────────────────────────────────────────────────────────

pub struct BgRemovalCache {
    /// Completed bg-removed file paths: **source_path → output_path**.
    pub paths: HashMap<String, String>,
    /// Desired cache key for each source path (encodes threshold).
    source_to_key: HashMap<String, String>,
    /// Currently processing keys (internal cache keys).
    pending: HashSet<String>,
    /// Failed keys (not retried).
    failed: HashSet<String>,
    /// Internal cache_key → source_path reverse mapping.
    key_to_source: HashMap<String, String>,
    total_requested: usize,
    result_rx: mpsc::Receiver<WorkerUpdate>,
    work_tx: Option<mpsc::Sender<BgRemovalJob>>,
    cache_root: PathBuf,
    model_path: Option<String>,
}

impl BgRemovalCache {
    pub fn new() -> Self {
        let (result_tx, result_rx) = mpsc::sync_channel::<WorkerUpdate>(32);
        let (work_tx, work_rx) = mpsc::channel::<BgRemovalJob>();
        let work_rx = std::sync::Arc::new(std::sync::Mutex::new(work_rx));

        // Spawn 2 worker threads (ONNX inference is CPU-heavy).
        for _ in 0..2 {
            let rx = work_rx.clone();
            let tx = result_tx.clone();
            std::thread::spawn(move || {
                loop {
                    let job = {
                        let lock = rx.lock().unwrap();
                        lock.recv()
                    };
                    match job {
                        Ok(job) => {
                            let success = run_bg_removal(
                                &job.source_path,
                                &job.output_path,
                                &job.model_path,
                                job.threshold,
                            );
                            let _ = tx.send(WorkerUpdate::Done(WorkerResult {
                                cache_key: job.cache_key,
                                output_path: job.output_path,
                                success,
                            }));
                        }
                        Err(_) => break, // Channel closed, exit thread.
                    }
                }
            });
        }

        let cache_root = dirs_cache_root();
        let _ = std::fs::create_dir_all(&cache_root);

        let model_path = find_model_path();
        if model_path.is_none() {
            log::warn!("BgRemovalCache: MODNet ONNX model not found; background removal disabled");
        }

        Self {
            paths: HashMap::new(),
            source_to_key: HashMap::new(),
            pending: HashSet::new(),
            failed: HashSet::new(),
            key_to_source: HashMap::new(),
            total_requested: 0,
            result_rx,
            work_tx: Some(work_tx),
            cache_root,
            model_path,
        }
    }

    /// Returns `true` if the ONNX model is available and background removal
    /// can be performed.
    pub fn is_available(&self) -> bool {
        self.model_path.is_some()
    }

    /// Re-check for the model file (e.g. after a download completes).
    pub fn refresh_model_path(&mut self) {
        self.model_path = find_model_path();
    }

    /// Request background removal for a source clip.  Returns immediately.
    /// Call [`poll()`] periodically to collect results.
    pub fn request(&mut self, source_path: &str, threshold: f64) {
        let Some(ref model_path) = self.model_path else {
            return;
        };
        let key = cache_key(source_path, threshold);
        if self.pending.contains(&key) || self.failed.contains(&key) {
            return;
        }

        if let Some(prev_key) = self.source_to_key.get(source_path) {
            if prev_key == &key && self.paths.contains_key(source_path) {
                return;
            }
            if prev_key != &key {
                // Threshold changed for this source; drop the old resolved path
                // so preview/export fall back to source media until the new
                // threshold-specific output is ready.
                self.paths.remove(source_path);
            }
        }
        self.source_to_key
            .insert(source_path.to_string(), key.clone());

        // Check disk for pre-existing result.
        let output_path = self.output_path_for_key(&key);
        if Path::new(&output_path).exists() {
            if bg_removal_file_is_ready(&output_path) {
                log::info!("BgRemovalCache: found existing file for key={}", key);
                self.paths.insert(source_path.to_string(), output_path);
                return;
            } else {
                let _ = std::fs::remove_file(&output_path);
            }
        }
        self.total_requested += 1;
        self.pending.insert(key.clone());
        self.key_to_source
            .insert(key.clone(), source_path.to_string());
        if let Some(ref tx) = self.work_tx {
            let _ = tx.send(BgRemovalJob {
                cache_key: key,
                source_path: source_path.to_string(),
                output_path,
                threshold,
                model_path: model_path.clone(),
            });
        }
    }

    /// Non-blocking poll for completed jobs.  Returns list of newly-ready source paths.
    pub fn poll(&mut self) -> Vec<String> {
        let mut resolved = Vec::new();
        while let Ok(update) = self.result_rx.try_recv() {
            match update {
                WorkerUpdate::Done(result) => {
                    self.pending.remove(&result.cache_key);
                    if result.success && Path::new(&result.output_path).exists() {
                        let source = self
                            .key_to_source
                            .remove(&result.cache_key)
                            .unwrap_or_else(|| result.cache_key.clone());
                        if self.source_to_key.get(&source) == Some(&result.cache_key) {
                            log::info!(
                                "BgRemovalCache: completed source={} path={}",
                                source,
                                result.output_path
                            );
                            self.paths.insert(source.clone(), result.output_path);
                            resolved.push(source);
                        } else {
                            log::info!(
                                "BgRemovalCache: ignored stale result for source={} key={}",
                                source,
                                result.cache_key
                            );
                        }
                    } else {
                        log::warn!("BgRemovalCache: failed key={}", result.cache_key);
                        self.failed.insert(result.cache_key);
                    }
                }
            }
        }
        resolved
    }

    /// Aggregate progress for the UI status bar.
    pub fn progress(&self) -> BgRemovalProgress {
        BgRemovalProgress {
            total: self.total_requested,
            completed: self.paths.len(),
            in_flight: !self.pending.is_empty(),
        }
    }

    /// Invalidate all cached results (e.g. when model or threshold changes).
    pub fn invalidate_all(&mut self) {
        self.paths.clear();
        self.source_to_key.clear();
        self.failed.clear();
        self.key_to_source.clear();
        self.total_requested = 0;
    }

    /// Get the bg-removed file path for a source clip, if ready.
    pub fn get_path(&self, source_path: &str, threshold: f64) -> Option<&String> {
        let key = cache_key(source_path, threshold);
        if self.source_to_key.get(source_path) == Some(&key) {
            self.paths.get(source_path)
        } else {
            None
        }
    }

    fn output_path_for_key(&self, key: &str) -> String {
        self.cache_root
            .join(format!("{key}.webm"))
            .to_string_lossy()
            .to_string()
    }
}

impl Drop for BgRemovalCache {
    fn drop(&mut self) {
        // Close work channel so worker threads exit.
        self.work_tx.take();
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn cache_key(source_path: &str, threshold: f64) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    source_path.hash(&mut hasher);
    format!("bgr_{}_{:.2}", hasher.finish(), threshold)
}

fn dirs_cache_root() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".cache")
        });
    base.join("ultimateslice").join("bg_removal")
}

pub const MODEL_FILENAME: &str = "modnet_photographic_portrait_matting.onnx";
pub const MODEL_DOWNLOAD_URL: &str =
    "https://drive.usercontent.google.com/download?id=1cgycTQlYXpTh26gB9FTnthE7AvruV8hd&export=download&confirm=t";

/// Return the preferred download destination directory for models
/// (`$XDG_DATA_HOME/ultimateslice/models/`).
pub fn model_download_dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(home).join(".local/share")
        });
    base.join("ultimateslice").join("models")
}

/// Search standard locations for the MODNet ONNX model file.
pub fn find_model_path() -> Option<String> {
    let candidates = [
        // Relative to executable.
        std::env::current_exe()
            .ok()
            .and_then(|p| {
                p.parent()
                    .map(|d| d.join("data/models/modnet_photographic_portrait_matting.onnx"))
            })
            .unwrap_or_default(),
        // Relative to CWD (development).
        PathBuf::from("data/models/modnet_photographic_portrait_matting.onnx"),
        // Flatpak data dir.
        PathBuf::from("/app/share/ultimateslice/models/modnet_photographic_portrait_matting.onnx"),
        // XDG data home.
        std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_default();
                PathBuf::from(home).join(".local/share")
            })
            .join("ultimateslice/models/modnet_photographic_portrait_matting.onnx"),
    ];
    for path in &candidates {
        if path.exists() {
            log::info!("BgRemovalCache: found model at {}", path.display());
            return Some(path.to_string_lossy().to_string());
        }
    }
    None
}

fn bg_removal_file_is_ready(path: &str) -> bool {
    std::fs::metadata(path)
        .map(|m| m.len() > 0)
        .unwrap_or(false)
}

// ── Worker: frame-by-frame background removal ──────────────────────────────

/// Run background removal on a source video file using ONNX inference.
///
/// Pipeline: FFmpeg decode → per-frame MODNet inference → FFmpeg VP9+alpha encode.
/// Uses the `ort` crate for ONNX Runtime.
#[cfg(feature = "ai-inference")]
fn run_bg_removal(source_path: &str, output_path: &str, model_path: &str, threshold: f64) -> bool {
    use ort::session::Session;
    use ort::value::TensorRef;

    let temp_path = format!("{output_path}.partial");

    // 1. Load ONNX model. Routes the SessionBuilder through
    //    `ai_providers::configure_session_builder` so the currently-
    //    selected execution backend (CUDA / ROCm / OpenVINO / CPU)
    //    is applied before the model is loaded. On CPU-only builds
    //    this is a no-op and MODNet runs exactly as it did before.
    use super::ai_providers;
    let mut session: Session = match Session::builder()
        .and_then(|b| {
            Ok(b.with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)?)
        })
        .and_then(|b: ort::session::builder::SessionBuilder| {
            ai_providers::configure_session_builder(b, ai_providers::current_backend())
        })
        .and_then(|mut b: ort::session::builder::SessionBuilder| b.commit_from_file(model_path))
    {
        Ok(s) => s,
        Err(e) => {
            log::error!("BgRemovalCache: failed to load model: {}", e);
            return false;
        }
    };

    // 2. Probe source dimensions and frame count via ffprobe.
    let (src_w, src_h, fps_num, fps_den) = match probe_video_info(source_path) {
        Some(info) => info,
        None => {
            log::error!("BgRemovalCache: failed to probe source {}", source_path);
            return false;
        }
    };

    // 3. Set up FFmpeg decode subprocess: outputs raw RGBA frames to stdout.
    let mut decoder = match Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            source_path,
            "-pix_fmt",
            "rgba",
            "-f",
            "rawvideo",
            "-v",
            "error",
            "pipe:1",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            log::error!("BgRemovalCache: failed to spawn decoder: {}", e);
            return false;
        }
    };

    // 4. Set up FFmpeg encode subprocess: reads raw RGBA from stdin, outputs VP9 alpha WebM.
    let mut encoder = match Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-s",
            &format!("{}x{}", src_w, src_h),
            "-r",
            &format!("{}/{}", fps_num, fps_den),
            "-i",
            "pipe:0",
            "-c:v",
            "libvpx-vp9",
            "-pix_fmt",
            "yuva420p",
            "-crf",
            "30",
            "-b:v",
            "0",
            "-auto-alt-ref",
            "0",
            "-f",
            "webm",
            &temp_path,
        ])
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            log::error!("BgRemovalCache: failed to spawn encoder: {}", e);
            let _ = decoder.kill();
            return false;
        }
    };

    let frame_bytes = (src_w * src_h * 4) as usize; // RGBA
    let mut frame_buf = vec![0u8; frame_bytes];
    let model_size: usize = 512;

    let decoder_stdout = decoder.stdout.take().unwrap();
    let encoder_stdin = encoder.stdin.take().unwrap();

    let mut reader = std::io::BufReader::new(decoder_stdout);
    let mut writer = std::io::BufWriter::new(encoder_stdin);

    use std::io::{Read, Write};

    let mut frame_count = 0u64;
    loop {
        // Read one RGBA frame from decoder.
        match reader.read_exact(&mut frame_buf) {
            Ok(()) => {}
            Err(_) => break, // EOF or error.
        }

        // Prepare model input: resize to 512×512, normalize to [0, 1], CHW layout.
        let input_tensor =
            prepare_input_tensor(&frame_buf, src_w as usize, src_h as usize, model_size);

        // Create ort tensor from ndarray.
        let ort_input = match TensorRef::from_array_view(&input_tensor) {
            Ok(t) => t,
            Err(e) => {
                log::error!("BgRemovalCache: failed to create input tensor: {}", e);
                break;
            }
        };

        // Run inference.
        let outputs = match session.run(ort::inputs!["input" => ort_input]) {
            Ok(o) => o,
            Err(e) => {
                log::error!(
                    "BgRemovalCache: inference failed at frame {}: {}",
                    frame_count,
                    e
                );
                break;
            }
        };

        // Extract output matte (1×1×512×512).
        // Extract shape and data into owned types to avoid lifetime issues with ValueRef.
        let (matte_dims, matte_owned): (Vec<i64>, Vec<f32>) =
            if let Some(val) = outputs.get("output") {
                match val.try_extract_tensor::<f32>() {
                    Ok((shape, data)) => (shape.iter().copied().collect(), data.to_vec()),
                    Err(e) => {
                        log::error!("BgRemovalCache: failed to extract matte tensor: {}", e);
                        break;
                    }
                }
            } else if let Some((_name, val)) = outputs.iter().next() {
                match val.try_extract_tensor::<f32>() {
                    Ok((shape, data)) => (shape.iter().copied().collect(), data.to_vec()),
                    Err(e) => {
                        log::error!("BgRemovalCache: failed to extract matte tensor: {}", e);
                        break;
                    }
                }
            } else {
                log::error!("BgRemovalCache: no output tensors");
                break;
            };

        // Apply matte to frame alpha channel.
        apply_matte_to_frame(
            &mut frame_buf,
            src_w as usize,
            src_h as usize,
            &matte_dims,
            &matte_owned,
            model_size,
            threshold,
        );

        // Write frame to encoder.
        if writer.write_all(&frame_buf).is_err() {
            break;
        }
        frame_count += 1;
    }

    // Close pipes and wait for subprocesses.
    drop(writer);
    drop(reader);
    let dec_ok = decoder.wait().map(|s| s.success()).unwrap_or(false);
    let enc_ok = encoder.wait().map(|s| s.success()).unwrap_or(false);

    if !enc_ok || frame_count == 0 {
        log::error!(
            "BgRemovalCache: encode failed or no frames (dec={} enc={} frames={})",
            dec_ok,
            enc_ok,
            frame_count
        );
        let _ = std::fs::remove_file(&temp_path);
        return false;
    }

    // Atomic rename.
    if std::fs::rename(&temp_path, output_path).is_err() {
        log::error!(
            "BgRemovalCache: failed to rename {} → {}",
            temp_path,
            output_path
        );
        let _ = std::fs::remove_file(&temp_path);
        return false;
    }

    log::info!(
        "BgRemovalCache: completed {} frames for {}",
        frame_count,
        source_path
    );
    true
}

/// Stub: when the `ai-inference` feature is disabled, bg removal always fails.
#[cfg(not(feature = "ai-inference"))]
fn run_bg_removal(
    _source_path: &str,
    _output_path: &str,
    _model_path: &str,
    _threshold: f64,
) -> bool {
    log::warn!("BgRemovalCache: ai-inference feature not enabled; cannot run bg removal");
    false
}

/// Probe video info via ffprobe: returns (width, height, fps_num, fps_den).
#[cfg(feature = "ai-inference")]
fn probe_video_info(path: &str) -> Option<(u32, u32, u32, u32)> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,r_frame_rate",
            "-of",
            "csv=p=0",
            path,
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = text.trim().split(',').collect();
    if parts.len() < 3 {
        return None;
    }
    let w: u32 = parts[0].parse().ok()?;
    let h: u32 = parts[1].parse().ok()?;
    let fps_parts: Vec<&str> = parts[2].split('/').collect();
    let fps_num: u32 = fps_parts.first()?.parse().ok()?;
    let fps_den: u32 = fps_parts.get(1).unwrap_or(&"1").parse().ok()?;
    Some((w, h, fps_num, fps_den))
}

/// Prepare a 512×512 CHW float32 tensor from an RGBA frame buffer.
#[cfg(feature = "ai-inference")]
fn prepare_input_tensor(
    frame: &[u8],
    src_w: usize,
    src_h: usize,
    model_size: usize,
) -> ndarray::Array4<f32> {
    let mut tensor = ndarray::Array4::<f32>::zeros((1, 3, model_size, model_size));
    let x_ratio = src_w as f64 / model_size as f64;
    let y_ratio = src_h as f64 / model_size as f64;

    for y in 0..model_size {
        for x in 0..model_size {
            let sx = ((x as f64 * x_ratio) as usize).min(src_w - 1);
            let sy = ((y as f64 * y_ratio) as usize).min(src_h - 1);
            let idx = (sy * src_w + sx) * 4; // RGBA
            let r = frame[idx] as f32 / 255.0;
            let g = frame[idx + 1] as f32 / 255.0;
            let b = frame[idx + 2] as f32 / 255.0;
            tensor[[0, 0, y, x]] = r;
            tensor[[0, 1, y, x]] = g;
            tensor[[0, 2, y, x]] = b;
        }
    }
    tensor
}

/// Apply a model-output matte to the frame's alpha channel.
///
/// `matte_shape` is the Shape from ort (e.g. [1, 1, 512, 512]).
/// `matte_data` is the flat f32 slice of alpha values.
#[cfg(feature = "ai-inference")]
fn apply_matte_to_frame(
    frame: &mut [u8],
    src_w: usize,
    src_h: usize,
    matte_shape: &[i64],
    matte_data: &[f32],
    _model_size: usize,
    threshold: f64,
) {
    // Determine matte height/width from shape.
    let (matte_h, matte_w) = if matte_shape.len() == 4 {
        (matte_shape[2] as usize, matte_shape[3] as usize)
    } else if matte_shape.len() == 3 {
        (matte_shape[1] as usize, matte_shape[2] as usize)
    } else if matte_shape.len() == 2 {
        (matte_shape[0] as usize, matte_shape[1] as usize)
    } else {
        log::error!("BgRemovalCache: unexpected matte shape {:?}", matte_shape);
        return;
    };

    let x_ratio = matte_w as f64 / src_w as f64;
    let y_ratio = matte_h as f64 / src_h as f64;

    for y in 0..src_h {
        for x in 0..src_w {
            let mx = ((x as f64 * x_ratio) as usize).min(matte_w - 1);
            let my = ((y as f64 * y_ratio) as usize).min(matte_h - 1);

            let matte_idx = my * matte_w + mx;
            let alpha_val = matte_data.get(matte_idx).copied().unwrap_or(0.0);

            // Apply threshold: values below threshold become transparent.
            let alpha = if (alpha_val as f64) < threshold * 0.5 {
                0.0_f32
            } else {
                alpha_val.clamp(0.0, 1.0)
            };

            let idx = (y * src_w + x) * 4 + 3; // Alpha channel offset.
            frame[idx] = (alpha * 255.0) as u8;
        }
    }
}
