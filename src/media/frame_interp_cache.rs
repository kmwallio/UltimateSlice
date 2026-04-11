// SPDX-License-Identifier: GPL-3.0-or-later
//! Offline AI frame-interpolation cache (RIFE).
//!
//! Modeled after [`super::bg_removal_cache::BgRemovalCache`]: a fixed-size
//! thread pool decodes the source video at native fps, runs an ONNX
//! frame-interpolation network (RIFE) pairwise to synthesize intermediate
//! frames, and encodes the result as an H.264 sidecar at `multiplier×`
//! source fps.  Both Program Monitor preview and FFmpeg export consume the
//! sidecar so the visible frames match exactly.
//!
//! The cache is *clip-aware* — the cache key includes the source path **and**
//! the desired multiplier (derived from the slowest segment of the clip), so
//! two clips that share a source but request different multipliers each get
//! their own sidecar.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
#[cfg(feature = "ai-inference")]
use std::process::Command;
use std::sync::mpsc;

use crate::model::clip::Clip;

// ── Public types ───────────────────────────────────────────────────────────

enum WorkerUpdate {
    Done(WorkerResult),
}

struct WorkerResult {
    cache_key: String,
    output_path: String,
    success: bool,
}

struct FrameInterpJob {
    cache_key: String,
    source_path: String,
    output_path: String,
    multiplier: u32,
    model_path: String,
}

/// Aggregate progress for the status bar.
pub struct FrameInterpProgress {
    pub total: usize,
    pub completed: usize,
    pub in_flight: bool,
}

/// Per-clip status for the Inspector status row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameInterpStatus {
    /// Clip does not request AI interpolation, or has no slow-motion segment.
    NotApplicable,
    /// Required model is not installed on disk.
    ModelMissing,
    /// Background sidecar generation in progress.
    Generating,
    /// Sidecar is ready and being consumed by preview / export.
    Ready,
    /// Sidecar generation previously failed for this source+multiplier.
    Failed,
}

// ── Cache ──────────────────────────────────────────────────────────────────

pub struct FrameInterpCache {
    /// Completed sidecar paths, keyed by internal cache_key.
    paths: HashMap<String, String>,
    /// Currently processing keys.
    pending: HashSet<String>,
    /// Failed keys (not retried).
    failed: HashSet<String>,
    total_requested: usize,
    result_rx: mpsc::Receiver<WorkerUpdate>,
    work_tx: Option<mpsc::Sender<FrameInterpJob>>,
    cache_root: PathBuf,
    model_path: Option<String>,
}

impl FrameInterpCache {
    pub fn new() -> Self {
        let (result_tx, result_rx) = mpsc::sync_channel::<WorkerUpdate>(32);
        let (work_tx, work_rx) = mpsc::channel::<FrameInterpJob>();
        let work_rx = std::sync::Arc::new(std::sync::Mutex::new(work_rx));

        // Spawn 2 worker threads — RIFE is CPU-heavy and benefits from
        // parallel jobs across clips.
        for _ in 0..2 {
            let rx = work_rx.clone();
            let tx = result_tx.clone();
            std::thread::spawn(move || loop {
                let job = {
                    let lock = rx.lock().unwrap();
                    lock.recv()
                };
                match job {
                    Ok(job) => {
                        let success = run_frame_interp(
                            &job.source_path,
                            &job.output_path,
                            &job.model_path,
                            job.multiplier,
                        );
                        let _ = tx.send(WorkerUpdate::Done(WorkerResult {
                            cache_key: job.cache_key,
                            output_path: job.output_path,
                            success,
                        }));
                    }
                    Err(_) => break,
                }
            });
        }

        let cache_root = dirs_cache_root();
        let _ = std::fs::create_dir_all(&cache_root);

        let model_path = find_model_path();
        if model_path.is_none() {
            log::warn!(
                "FrameInterpCache: RIFE ONNX model not found; AI frame interpolation disabled"
            );
        }

        Self {
            paths: HashMap::new(),
            pending: HashSet::new(),
            failed: HashSet::new(),
            total_requested: 0,
            result_rx,
            work_tx: Some(work_tx),
            cache_root,
            model_path,
        }
    }

    /// Returns `true` if the ONNX model is available.
    pub fn is_available(&self) -> bool {
        self.model_path.is_some()
    }

    /// Re-check for the model file (e.g. after a manual install).
    pub fn refresh_model_path(&mut self) {
        self.model_path = find_model_path();
    }

    /// Request frame-interpolation for a clip.  Returns immediately.  Call
    /// [`Self::poll`] periodically to collect results.  No-op when:
    ///   - the clip does not request AI mode,
    ///   - the clip has no slow-motion (multiplier would be 1×),
    ///   - the model is not installed.
    pub fn request_for_clip(&mut self, clip: &Clip) {
        if !clip_requests_ai_interp(clip) {
            return;
        }
        let Some(ref model_path) = self.model_path else {
            return;
        };
        let multiplier = recommended_multiplier(clip);
        if multiplier <= 1 {
            return;
        }
        let key = cache_key(&clip.source_path, multiplier);
        if self.pending.contains(&key) || self.failed.contains(&key) {
            return;
        }
        if self.paths.contains_key(&key) {
            return;
        }
        let output_path = self.output_path_for_key(&key);
        if Path::new(&output_path).exists() && sidecar_file_is_ready(&output_path) {
            log::info!("FrameInterpCache: found existing sidecar for key={}", key);
            self.paths.insert(key, output_path);
            return;
        }
        if Path::new(&output_path).exists() {
            let _ = std::fs::remove_file(&output_path);
        }
        self.total_requested += 1;
        self.pending.insert(key.clone());
        if let Some(ref tx) = self.work_tx {
            let _ = tx.send(FrameInterpJob {
                cache_key: key,
                source_path: clip.source_path.clone(),
                output_path,
                multiplier,
                model_path: model_path.clone(),
            });
        }
    }

    /// Non-blocking poll for completed jobs.  Returns the cache keys that
    /// just transitioned to ready.
    pub fn poll(&mut self) -> Vec<String> {
        let mut resolved = Vec::new();
        while let Ok(update) = self.result_rx.try_recv() {
            match update {
                WorkerUpdate::Done(result) => {
                    self.pending.remove(&result.cache_key);
                    if result.success && Path::new(&result.output_path).exists() {
                        log::info!(
                            "FrameInterpCache: completed key={} path={}",
                            result.cache_key,
                            result.output_path
                        );
                        self.paths
                            .insert(result.cache_key.clone(), result.output_path);
                        resolved.push(result.cache_key);
                    } else {
                        log::warn!("FrameInterpCache: failed key={}", result.cache_key);
                        self.failed.insert(result.cache_key);
                    }
                }
            }
        }
        resolved
    }

    /// Aggregate progress for the UI status bar.
    pub fn progress(&self) -> FrameInterpProgress {
        FrameInterpProgress {
            total: self.total_requested,
            completed: self.paths.len(),
            in_flight: !self.pending.is_empty(),
        }
    }

    /// Look up the sidecar path that should be used for `clip` (if ready).
    pub fn path_for_clip(&self, clip: &Clip) -> Option<&String> {
        if !clip_requests_ai_interp(clip) {
            return None;
        }
        let multiplier = recommended_multiplier(clip);
        if multiplier <= 1 {
            return None;
        }
        let key = cache_key(&clip.source_path, multiplier);
        self.paths.get(&key)
    }

    /// Status for a clip — drives the Inspector status row.
    pub fn status_for_clip(&self, clip: &Clip) -> FrameInterpStatus {
        if !clip_requests_ai_interp(clip) {
            return FrameInterpStatus::NotApplicable;
        }
        let multiplier = recommended_multiplier(clip);
        if multiplier <= 1 {
            return FrameInterpStatus::NotApplicable;
        }
        if self.model_path.is_none() {
            return FrameInterpStatus::ModelMissing;
        }
        let key = cache_key(&clip.source_path, multiplier);
        if self.paths.contains_key(&key) {
            FrameInterpStatus::Ready
        } else if self.pending.contains(&key) {
            FrameInterpStatus::Generating
        } else if self.failed.contains(&key) {
            FrameInterpStatus::Failed
        } else {
            // Not yet requested; the window glue will queue it on the next
            // on_project_changed pass.
            FrameInterpStatus::Generating
        }
    }

    /// Build a clip-id-keyed snapshot of resolved sidecar paths for handing
    /// off to the export thread / Program Monitor (which only know clip ids).
    pub fn snapshot_paths_by_clip_id(
        &self,
        project: &crate::model::project::Project,
    ) -> HashMap<String, String> {
        let mut out = HashMap::new();
        fn walk(
            cache: &FrameInterpCache,
            tracks: &[crate::model::track::Track],
            out: &mut HashMap<String, String>,
        ) {
            for track in tracks {
                for clip in &track.clips {
                    if let Some(path) = cache.path_for_clip(clip) {
                        out.insert(clip.id.clone(), path.clone());
                    }
                    if let Some(ref ctracks) = clip.compound_tracks {
                        walk(cache, ctracks, out);
                    }
                }
            }
        }
        walk(self, &project.tracks, &mut out);
        out
    }

    /// Drop all cached entries (e.g. when an unrelated invalidation event
    /// such as a model update occurs).
    pub fn invalidate_all(&mut self) {
        self.paths.clear();
        self.failed.clear();
        self.total_requested = 0;
    }

    fn output_path_for_key(&self, key: &str) -> String {
        self.cache_root
            .join(format!("{key}.mp4"))
            .to_string_lossy()
            .to_string()
    }
}

impl Default for FrameInterpCache {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for FrameInterpCache {
    fn drop(&mut self) {
        self.work_tx.take();
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn clip_requests_ai_interp(clip: &Clip) -> bool {
    clip.slow_motion_interp == crate::model::clip::SlowMotionInterp::Ai && clip.has_slow_motion()
}

/// Choose a sensible frame multiplier for a slowed clip.
///
/// `M = ceil(1 / min_speed)` clamped to `[2, 8]` to bound disk and CPU.
/// At 0.5× we get 2×, at 0.25× we get 4×, at 0.125× and slower we cap at 8×.
pub fn recommended_multiplier(clip: &Clip) -> u32 {
    let min_speed = clip.min_effective_speed();
    if min_speed <= 0.0 || min_speed >= 1.0 {
        return 1;
    }
    let raw = (1.0 / min_speed).ceil() as u32;
    raw.clamp(2, 8)
}

fn cache_key(source_path: &str, multiplier: u32) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    source_path.hash(&mut hasher);
    format!("rife_{}_{}x", hasher.finish(), multiplier)
}

fn dirs_cache_root() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".cache")
        });
    base.join("ultimateslice").join("frame_interp")
}

pub const MODEL_FILENAME: &str = "rife.onnx";
/// Filenames accepted in the model directory, in priority order. We accept
/// the conventional `rife.onnx` first; `model.onnx` is also accepted as a
/// fallback so users who downloaded a community ONNX export under its
/// upstream filename don't have to rename it.
pub const ACCEPTED_MODEL_FILENAMES: &[&str] = &["rife.onnx", "model.onnx"];
pub const MODEL_DOWNLOAD_HINT: &str =
    "Obtain a RIFE ONNX export with the standard 6-channel input + timestep \
     convention (see https://github.com/hzwer/Practical-RIFE for the upstream \
     project and export tooling) and place the file at \
     ~/.local/share/ultimateslice/models/rife.onnx (model.onnx is also accepted).";

/// Return the preferred install directory for AI models.
pub fn model_install_dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(home).join(".local/share")
        });
    base.join("ultimateslice").join("models")
}

/// Search standard locations for the RIFE ONNX model file.
///
/// Locations searched (in order): executable-relative `data/models/`, the
/// dev-build CWD `data/models/`, the Flatpak `/app/share/ultimateslice/models/`,
/// and the user's XDG data directory. Each location is probed for every
/// filename in [`ACCEPTED_MODEL_FILENAMES`].
pub fn find_model_path() -> Option<String> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let xdg_dir = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(home).join(".local/share")
        })
        .join("ultimateslice/models");
    let dirs: [PathBuf; 4] = [
        exe_dir.map(|d| d.join("data/models")).unwrap_or_default(),
        PathBuf::from("data/models"),
        PathBuf::from("/app/share/ultimateslice/models"),
        xdg_dir,
    ];
    for dir in &dirs {
        for fname in ACCEPTED_MODEL_FILENAMES {
            let path = dir.join(fname);
            if path.exists() {
                log::info!("FrameInterpCache: found model at {}", path.display());
                return Some(path.to_string_lossy().to_string());
            }
        }
    }
    None
}

fn sidecar_file_is_ready(path: &str) -> bool {
    std::fs::metadata(path)
        .map(|m| m.len() > 0)
        .unwrap_or(false)
}

// ── Worker: pairwise RIFE inference ────────────────────────────────────────

/// Run RIFE frame interpolation on a source video.
///
/// Pipeline: FFmpeg decode → pairwise RIFE → FFmpeg H.264 encode at
/// `multiplier × source fps`.  The output keeps the same wall-clock duration
/// as the source so consumers can swap the input path without changing
/// `setpts` math.
#[cfg(feature = "ai-inference")]
fn run_frame_interp(
    source_path: &str,
    output_path: &str,
    model_path: &str,
    multiplier: u32,
) -> bool {
    use ort::session::Session;
    use ort::value::TensorRef;

    let temp_path = format!("{output_path}.partial");

    // 1. Load ONNX model. Routes the SessionBuilder through
    //    `ai_providers::configure_session_builder` so RIFE picks up
    //    the currently-selected execution backend automatically.
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
            log::error!("FrameInterpCache: failed to load model: {}", e);
            return false;
        }
    };

    // 2. Probe source.
    let (src_w, src_h, fps_num, fps_den) = match probe_video_info(source_path) {
        Some(info) => info,
        None => {
            log::error!("FrameInterpCache: failed to probe {}", source_path);
            return false;
        }
    };

    // RIFE expects dimensions divisible by 32. Round up the inference
    // resolution and pad/crop the buffers; the encoded sidecar keeps the
    // original source resolution.
    let model_w = ((src_w + 31) / 32) * 32;
    let model_h = ((src_h + 31) / 32) * 32;

    // 3. Decode source to raw RGB.
    let mut decoder = match Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            source_path,
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            log::error!("FrameInterpCache: failed to spawn decoder: {}", e);
            return false;
        }
    };

    // 4. Encode interpolated frames at multiplier × source fps.
    let out_fps_num = fps_num.saturating_mul(multiplier);
    let mut encoder = match Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-s",
            &format!("{}x{}", src_w, src_h),
            "-r",
            &format!("{}/{}", out_fps_num, fps_den.max(1)),
            "-i",
            "pipe:0",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-preset",
            "veryfast",
            "-crf",
            "16",
            "-movflags",
            "+faststart",
            "-f",
            "mp4",
            &temp_path,
        ])
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            log::error!("FrameInterpCache: failed to spawn encoder: {}", e);
            let _ = decoder.kill();
            return false;
        }
    };

    let frame_bytes = (src_w * src_h * 3) as usize;
    let decoder_stdout = decoder.stdout.take().unwrap();
    let encoder_stdin = encoder.stdin.take().unwrap();
    let mut reader = std::io::BufReader::with_capacity(frame_bytes * 2, decoder_stdout);
    let mut writer = std::io::BufWriter::with_capacity(frame_bytes * 2, encoder_stdin);

    use std::io::{Read, Write};

    let mut prev_frame: Option<Vec<u8>> = None;
    let mut frame_count: u64 = 0;
    let mut interp_count: u64 = 0;

    let mut cur_frame = vec![0u8; frame_bytes];
    loop {
        match reader.read_exact(&mut cur_frame) {
            Ok(()) => {}
            Err(_) => break,
        }

        if let Some(ref prev) = prev_frame {
            // Generate `multiplier - 1` intermediates between prev and cur
            // (then write cur). The first frame is written below in the
            // `else` branch on the very first iteration.
            for k in 1..multiplier {
                let t = k as f32 / multiplier as f32;
                let interp = match interpolate_pair(
                    &mut session,
                    prev,
                    &cur_frame,
                    src_w as usize,
                    src_h as usize,
                    model_w as usize,
                    model_h as usize,
                    t,
                ) {
                    Some(buf) => buf,
                    None => {
                        // Fall back to a straight blend so playback is
                        // still smoother than nearest-neighbor on inference
                        // failure.
                        blend_frames(prev, &cur_frame, t)
                    }
                };
                if writer.write_all(&interp).is_err() {
                    break;
                }
                interp_count += 1;
            }
            if writer.write_all(&cur_frame).is_err() {
                break;
            }
        } else {
            // First decoded frame: write as-is.
            if writer.write_all(&cur_frame).is_err() {
                break;
            }
        }

        prev_frame = Some(cur_frame.clone());
        frame_count += 1;
    }

    drop(writer);
    drop(reader);
    let dec_ok = decoder.wait().map(|s| s.success()).unwrap_or(false);
    let enc_ok = encoder.wait().map(|s| s.success()).unwrap_or(false);

    if !enc_ok || frame_count == 0 {
        log::error!(
            "FrameInterpCache: encode failed (dec={} enc={} frames={})",
            dec_ok,
            enc_ok,
            frame_count
        );
        let _ = std::fs::remove_file(&temp_path);
        return false;
    }

    if std::fs::rename(&temp_path, output_path).is_err() {
        log::error!(
            "FrameInterpCache: failed to rename {} → {}",
            temp_path,
            output_path
        );
        let _ = std::fs::remove_file(&temp_path);
        return false;
    }

    log::info!(
        "FrameInterpCache: completed {} source frames, {} interpolated, multiplier={}× → {}",
        frame_count,
        interp_count,
        multiplier,
        output_path
    );
    true
}

#[cfg(not(feature = "ai-inference"))]
fn run_frame_interp(
    _source_path: &str,
    _output_path: &str,
    _model_path: &str,
    _multiplier: u32,
) -> bool {
    log::warn!(
        "FrameInterpCache: ai-inference feature not enabled; cannot run frame interpolation"
    );
    false
}

/// Probe video info via ffprobe: returns `(width, height, fps_num, fps_den)`.
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

/// Run a single RIFE pair through the ONNX session.  Returns `None` on
/// inference failure so the caller can fall back to a CPU-side blend.
///
/// Input layout (typical RIFE export): `[1, 6, H, W]` float32, channels =
/// `[R0,G0,B0,R1,G1,B1]`, normalized `[0,1]`.  The output is `[1, 3, H, W]`.
/// Some RIFE exports also accept a separate `timestep` tensor; we try the
/// most common forms in order.
#[cfg(feature = "ai-inference")]
fn interpolate_pair(
    session: &mut ort::session::Session,
    prev: &[u8],
    cur: &[u8],
    src_w: usize,
    src_h: usize,
    model_w: usize,
    model_h: usize,
    t: f32,
) -> Option<Vec<u8>> {
    use ort::value::TensorRef;

    let mut input = ndarray::Array4::<f32>::zeros((1, 6, model_h, model_w));
    for y in 0..src_h.min(model_h) {
        for x in 0..src_w.min(model_w) {
            let src_idx = (y * src_w + x) * 3;
            input[[0, 0, y, x]] = prev[src_idx] as f32 / 255.0;
            input[[0, 1, y, x]] = prev[src_idx + 1] as f32 / 255.0;
            input[[0, 2, y, x]] = prev[src_idx + 2] as f32 / 255.0;
            input[[0, 3, y, x]] = cur[src_idx] as f32 / 255.0;
            input[[0, 4, y, x]] = cur[src_idx + 1] as f32 / 255.0;
            input[[0, 5, y, x]] = cur[src_idx + 2] as f32 / 255.0;
        }
    }

    let timestep_arr = ndarray::Array1::<f32>::from_elem(1, t);

    // Extract the matte data into owned `(shape, Vec<f32>)` *inside* each
    // `run` block so the SessionOutputs (and its mutable borrow on session)
    // is fully dropped before we attempt the next variant. The two RIFE
    // export conventions we try are `(input, timestep)` and `(img, timestep)`.
    fn run_and_extract(
        session: &mut ort::session::Session,
        input: &ndarray::Array4<f32>,
        timestep_arr: &ndarray::Array1<f32>,
        first_name: &str,
    ) -> Option<(Vec<i64>, Vec<f32>)> {
        let pair = TensorRef::from_array_view(input).ok()?;
        let ts = TensorRef::from_array_view(timestep_arr).ok()?;
        let outputs = match first_name {
            "input" => session
                .run(ort::inputs!["input" => pair, "timestep" => ts])
                .ok()?,
            _ => session
                .run(ort::inputs!["img" => pair, "timestep" => ts])
                .ok()?,
        };
        // Materialize owned shape + data inside this scope so the
        // SessionOutputs can be dropped before the function returns.
        let mut iter = outputs.iter();
        let (_, val) = iter.next()?;
        match val.try_extract_tensor::<f32>() {
            Ok((shape, data)) => Some((shape.iter().copied().collect(), data.to_vec())),
            Err(_) => None,
        }
    }

    let extracted = run_and_extract(session, &input, &timestep_arr, "input")
        .or_else(|| run_and_extract(session, &input, &timestep_arr, "img"));
    let (_, data) = match extracted {
        Some(pair) => pair,
        None => {
            log::warn!(
                "FrameInterpCache: RIFE inference produced no output for any input naming variant"
            );
            return None;
        }
    };

    // Convert CHW float [0,1] back to HWC u8 RGB at source resolution.
    let mut out = vec![0u8; src_w * src_h * 3];
    let plane = model_w * model_h;
    for y in 0..src_h {
        for x in 0..src_w {
            let mi = y * model_w + x;
            let r = data.get(mi).copied().unwrap_or(0.0);
            let g = data.get(plane + mi).copied().unwrap_or(0.0);
            let b = data.get(2 * plane + mi).copied().unwrap_or(0.0);
            let oi = (y * src_w + x) * 3;
            out[oi] = (r.clamp(0.0, 1.0) * 255.0) as u8;
            out[oi + 1] = (g.clamp(0.0, 1.0) * 255.0) as u8;
            out[oi + 2] = (b.clamp(0.0, 1.0) * 255.0) as u8;
        }
    }
    Some(out)
}

/// Linear blend fallback used when RIFE inference fails for a pair.
#[cfg(feature = "ai-inference")]
fn blend_frames(prev: &[u8], cur: &[u8], t: f32) -> Vec<u8> {
    let mut out = Vec::with_capacity(prev.len());
    let t = t.clamp(0.0, 1.0);
    let omt = 1.0 - t;
    for i in 0..prev.len() {
        let v = prev[i] as f32 * omt + cur[i] as f32 * t;
        out.push(v.clamp(0.0, 255.0) as u8);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clip::{Clip, ClipKind, SlowMotionInterp};

    fn make_clip(speed: f64, interp: SlowMotionInterp) -> Clip {
        let mut clip = Clip::new("/tmp/test.mp4", 5_000_000_000, 0, ClipKind::Video);
        clip.speed = speed;
        clip.slow_motion_interp = interp;
        clip
    }

    #[test]
    fn multiplier_no_slow_motion() {
        let c = make_clip(1.0, SlowMotionInterp::Ai);
        assert_eq!(recommended_multiplier(&c), 1);
    }

    #[test]
    fn multiplier_half_speed() {
        let c = make_clip(0.5, SlowMotionInterp::Ai);
        assert_eq!(recommended_multiplier(&c), 2);
    }

    #[test]
    fn multiplier_quarter_speed() {
        let c = make_clip(0.25, SlowMotionInterp::Ai);
        assert_eq!(recommended_multiplier(&c), 4);
    }

    #[test]
    fn multiplier_clamped_to_eight() {
        let c = make_clip(0.05, SlowMotionInterp::Ai);
        assert_eq!(recommended_multiplier(&c), 8);
    }

    #[test]
    fn requests_only_when_ai_and_slow() {
        assert!(!clip_requests_ai_interp(&make_clip(
            0.5,
            SlowMotionInterp::Off
        )));
        assert!(!clip_requests_ai_interp(&make_clip(
            0.5,
            SlowMotionInterp::OpticalFlow
        )));
        assert!(!clip_requests_ai_interp(&make_clip(
            1.0,
            SlowMotionInterp::Ai
        )));
        assert!(clip_requests_ai_interp(&make_clip(
            0.5,
            SlowMotionInterp::Ai
        )));
    }
}
