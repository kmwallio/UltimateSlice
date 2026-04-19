// SPDX-License-Identifier: GPL-3.0-or-later
//! Render-and-Replace: bake a clip's primary pixel-level effect stack
//! into a ProRes 422 HQ sidecar so heavy effect chains stop re-computing
//! per frame during preview.
//!
//! Modeled on `voice_enhance_cache.rs` — same request/poll/status API,
//! one background ffmpeg worker thread, LRU-bounded cache root under
//! `$XDG_CACHE_HOME/ultimateslice/render_replace/`. Differences from the
//! voice-enhance cache:
//!
//! 1. The cache signature spans the whole baked effect scope (color
//!    grade, frei0r user effects, LUT stack, blur/denoise/sharpness),
//!    so unrelated edits like transform or opacity do not invalidate.
//! 2. When the sidecar is used at preview time, the slot builder in
//!    `ProgramPlayer` suppresses the live versions of baked-scope
//!    effect elements — otherwise those effects would be applied
//!    twice (once baked into the sidecar, once live on top).
//! 3. Transforms / opacity / blend / speed / transitions stay LIVE and
//!    are NOT part of the signature — the sidecar holds source-res,
//!    source-duration frames so those compositing-level operations
//!    work unchanged.
//!
//! Phase 1 scope: the listed pixel-level effects. Chroma key, HSL
//! qualifier, masks, stabilization, and audio effects are NOT in the
//! baked scope for now — they continue to apply live on top of the
//! baked frame.

use crate::model::clip::{Clip, ClipKind};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, Arc, Mutex};
use std::time::SystemTime;

/// Default soft cap on the render-replace cache size. ProRes 422 HQ is
/// roughly 150-200 Mb/s, so 4 GiB holds ~25 GiB of proxy-equivalent
/// material at 10:1 source ratios. Raise it if heavy projects
/// evict actively-needed sidecars.
const DEFAULT_MAX_CACHE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

// ── Public types ───────────────────────────────────────────────────────────

enum WorkerUpdate {
    Done(WorkerResult),
}

struct WorkerResult {
    cache_key: String,
    output_path: String,
    success: bool,
}

struct RenderReplaceJob {
    cache_key: String,
    source_path: String,
    output_path: String,
    video_filter: String,
    start_seconds: f64,
    duration_seconds: f64,
}

pub struct RenderReplaceProgress {
    pub total: usize,
    pub completed: usize,
    pub in_flight: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderReplaceStatus {
    Idle,
    Pending,
    Ready,
    Failed,
}

// ── Cache ──────────────────────────────────────────────────────────────────

pub struct RenderReplaceCache {
    pub paths: HashMap<String, String>,
    pending: HashSet<String>,
    failed: HashSet<String>,
    total_requested: usize,
    result_rx: mpsc::Receiver<WorkerUpdate>,
    work_tx: Option<mpsc::Sender<RenderReplaceJob>>,
    cache_root: PathBuf,
    cache_cap_bytes: u64,
}

impl RenderReplaceCache {
    pub fn new() -> Self {
        let (result_tx, result_rx) = mpsc::sync_channel::<WorkerUpdate>(32);
        let (work_tx, work_rx) = mpsc::channel::<RenderReplaceJob>();
        let work_rx = Arc::new(Mutex::new(work_rx));

        let rx = work_rx.clone();
        let tx = result_tx;
        std::thread::spawn(move || loop {
            let job = {
                let lock = rx.lock().unwrap();
                lock.recv()
            };
            match job {
                Ok(job) => {
                    let success = run_render_replace_job(&job);
                    let _ = tx.send(WorkerUpdate::Done(WorkerResult {
                        cache_key: job.cache_key,
                        output_path: job.output_path,
                        success,
                    }));
                }
                Err(_) => break,
            }
        });

        let cache_root = dirs_cache_root();
        let _ = std::fs::create_dir_all(&cache_root);

        Self {
            paths: HashMap::new(),
            pending: HashSet::new(),
            failed: HashSet::new(),
            total_requested: 0,
            result_rx,
            work_tx: Some(work_tx),
            cache_root,
            cache_cap_bytes: DEFAULT_MAX_CACHE_BYTES,
        }
    }

    /// Queue a render-replace bake for `clip`. Does nothing when the
    /// signature is already ready, in-flight, or known-failed. The
    /// caller is responsible for deciding whether to request at all
    /// (driven by the clip's `render_replace_enabled` flag plus a walk
    /// of the project on `on_project_changed`).
    ///
    /// Only file-backed clip kinds are bakeable. Compound, Multicam,
    /// Title, Adjustment, and Drawing clips have no single source file
    /// on disk (or have their own dedicated render cache) — the bake
    /// would invoke ffmpeg with an empty or virtual path and fail with
    /// "No such file or directory". Skip them silently.
    pub fn request(&mut self, clip: &Clip) {
        if !is_bakeable_kind(&clip.kind) || clip.source_path.trim().is_empty() {
            return;
        }
        let key = cache_key_for_clip(clip);

        if self.paths.contains_key(&key) {
            if let Some(p) = self.paths.get(&key) {
                touch_mtime(p);
            }
            return;
        }
        if self.pending.contains(&key) || self.failed.contains(&key) {
            return;
        }

        let output_path = self.output_path_for_key(&key);
        if Path::new(&output_path).exists() && file_is_ready(&output_path) {
            log::info!("RenderReplaceCache: found existing sidecar for key={}", key);
            touch_mtime(&output_path);
            self.paths.insert(key, output_path);
            return;
        } else if Path::new(&output_path).exists() {
            let _ = std::fs::remove_file(&output_path);
        }

        self.evict_if_oversized();

        let video_filter = build_bake_video_filter(clip);
        // Bake the trimmed source window (source_in..source_out). ffmpeg's
        // `-ss` + `-t` combo handles this cleanly; the baked sidecar is
        // source-duration, NOT timeline-duration (speed ramps stay live).
        let start_seconds = clip.source_in as f64 / 1_000_000_000.0;
        let duration_seconds =
            clip.source_out.saturating_sub(clip.source_in) as f64 / 1_000_000_000.0;

        self.total_requested += 1;
        self.pending.insert(key.clone());
        if let Some(ref tx) = self.work_tx {
            let _ = tx.send(RenderReplaceJob {
                cache_key: key,
                source_path: clip.source_path.clone(),
                output_path,
                video_filter,
                start_seconds,
                duration_seconds,
            });
        }
    }

    pub fn set_cache_cap_bytes(&mut self, bytes: u64) {
        self.cache_cap_bytes = bytes.max(256 * 1024 * 1024);
    }

    pub fn evict_if_oversized(&mut self) {
        let cap = self.cache_cap_bytes;
        let entries = match std::fs::read_dir(&self.cache_root) {
            Ok(e) => e,
            Err(_) => return,
        };
        let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
        let mut total: u64 = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !meta.is_file() {
                continue;
            }
            let len = meta.len();
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            total += len;
            files.push((path, len, mtime));
        }
        if total <= cap {
            return;
        }
        files.sort_by_key(|(_, _, mtime)| *mtime);
        log::info!(
            "RenderReplaceCache: cache size {} > cap {}, evicting LRU files",
            total,
            cap
        );
        let mut bytes_freed: u64 = 0;
        for (path, len, _mtime) in files {
            if total.saturating_sub(bytes_freed) <= cap {
                break;
            }
            let path_str = path.to_string_lossy().to_string();
            self.paths.retain(|_, v| v != &path_str);
            if let Err(e) = std::fs::remove_file(&path) {
                log::warn!(
                    "RenderReplaceCache: failed to evict {}: {}",
                    path.display(),
                    e
                );
                continue;
            }
            bytes_freed += len;
            log::info!(
                "RenderReplaceCache: evicted {} ({} bytes)",
                path.display(),
                len
            );
        }
    }

    pub fn poll(&mut self) -> Vec<String> {
        let mut resolved = Vec::new();
        while let Ok(update) = self.result_rx.try_recv() {
            match update {
                WorkerUpdate::Done(result) => {
                    self.pending.remove(&result.cache_key);
                    if result.success && Path::new(&result.output_path).exists() {
                        log::info!(
                            "RenderReplaceCache: completed key={} path={}",
                            result.cache_key,
                            result.output_path
                        );
                        self.paths
                            .insert(result.cache_key.clone(), result.output_path);
                        resolved.push(result.cache_key);
                    } else {
                        log::warn!("RenderReplaceCache: failed key={}", result.cache_key);
                        self.failed.insert(result.cache_key);
                    }
                }
            }
        }
        resolved
    }

    pub fn progress(&self) -> RenderReplaceProgress {
        RenderReplaceProgress {
            total: self.total_requested,
            completed: self.paths.len(),
            in_flight: !self.pending.is_empty(),
        }
    }

    pub fn get_path(&self, clip: &Clip) -> Option<&String> {
        self.paths.get(&cache_key_for_clip(clip))
    }

    pub fn status(&self, clip: &Clip) -> RenderReplaceStatus {
        let key = cache_key_for_clip(clip);
        if self.paths.contains_key(&key) {
            RenderReplaceStatus::Ready
        } else if self.pending.contains(&key) {
            RenderReplaceStatus::Pending
        } else if self.failed.contains(&key) {
            RenderReplaceStatus::Failed
        } else {
            RenderReplaceStatus::Idle
        }
    }

    pub fn retry(&mut self, clip: &Clip) -> bool {
        let key = cache_key_for_clip(clip);
        self.failed.remove(&key)
    }

    fn output_path_for_key(&self, key: &str) -> String {
        // MOV + ProRes 422 HQ. Matches the pro-intermediate convention
        // and is decodable by GStreamer without extra plugins on most
        // distros.
        self.cache_root
            .join(format!("{key}.mov"))
            .to_string_lossy()
            .to_string()
    }
}

impl Drop for RenderReplaceCache {
    fn drop(&mut self) {
        self.work_tx.take();
    }
}

// ── Signature ──────────────────────────────────────────────────────────────

/// Baked-scope field collection used by both `Clip` and `ProgramClip`
/// signature builders. Keeping both paths going through this helper
/// guarantees the preview-side lookup (ProgramClip) and the cache
/// population call (Clip) always compute the same key for the same
/// effective clip state.
#[allow(clippy::too_many_arguments)]
fn fold_baked_fields(
    hasher: &mut crate::media::cache_key::CacheKeyHasher,
    source_path: &str,
    source_in: u64,
    source_out: u64,
    brightness: f64,
    contrast: f64,
    saturation: f64,
    temperature: f64,
    tint: f64,
    exposure: f64,
    black_point: f64,
    shadows: f64,
    highlights: f64,
    denoise: f64,
    sharpness: f64,
    blur: f64,
    lut_paths: &[String],
    frei0r_effects: &[crate::model::clip::Frei0rEffect],
) {
    hasher.add_source_fingerprint(source_path);
    hasher.add(source_in).add(source_out);
    hasher
        .add((brightness * 10_000.0) as i64)
        .add((contrast * 10_000.0) as i64)
        .add((saturation * 10_000.0) as i64)
        .add((temperature * 10.0) as i64)
        .add((tint * 10_000.0) as i64)
        .add((exposure * 10_000.0) as i64)
        .add((black_point * 10_000.0) as i64)
        .add((shadows * 10_000.0) as i64)
        .add((highlights * 10_000.0) as i64);
    hasher
        .add((denoise * 10_000.0) as i64)
        .add((sharpness * 10_000.0) as i64)
        .add((blur * 10_000.0) as i64);
    for path in lut_paths {
        hasher.add(path.as_str());
    }
    for effect in frei0r_effects {
        hasher
            .add(effect.plugin_name.as_str())
            .add(if effect.enabled { 1u8 } else { 0u8 });
        let mut pairs: Vec<(&String, &f64)> = effect.params.iter().collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        for (name, value) in pairs {
            hasher.add(name.as_str()).add((*value * 10_000.0) as i64);
        }
        let mut str_pairs: Vec<(&String, &String)> =
            effect.string_params.iter().collect();
        str_pairs.sort_by(|a, b| a.0.cmp(b.0));
        for (name, value) in str_pairs {
            hasher.add(name.as_str()).add(value.as_str());
        }
    }
}

fn fold_color_keyframes(
    hasher: &mut crate::media::cache_key::CacheKeyHasher,
    brightness_kfs: &[crate::model::clip::NumericKeyframe],
    contrast_kfs: &[crate::model::clip::NumericKeyframe],
    saturation_kfs: &[crate::model::clip::NumericKeyframe],
    temperature_kfs: &[crate::model::clip::NumericKeyframe],
    tint_kfs: &[crate::model::clip::NumericKeyframe],
) {
    for kf in brightness_kfs {
        hasher.add(kf.time_ns).add((kf.value * 10_000.0) as i64);
    }
    for kf in contrast_kfs {
        hasher.add(kf.time_ns).add((kf.value * 10_000.0) as i64);
    }
    for kf in saturation_kfs {
        hasher.add(kf.time_ns).add((kf.value * 10_000.0) as i64);
    }
    for kf in temperature_kfs {
        hasher.add(kf.time_ns).add((kf.value * 10.0) as i64);
    }
    for kf in tint_kfs {
        hasher.add(kf.time_ns).add((kf.value * 10_000.0) as i64);
    }
}

/// Compute the render-replace cache key from the live ProgramClip field
/// view. Must produce the same key as [`cache_key_for_clip`] for the
/// same effective clip state — call sites rely on both sides agreeing.
pub fn cache_key_for_program_clip_fields(
    source_path: &str,
    source_in: u64,
    source_out: u64,
    brightness: f64,
    contrast: f64,
    saturation: f64,
    temperature: f64,
    tint: f64,
    exposure: f64,
    black_point: f64,
    shadows: f64,
    highlights: f64,
    denoise: f64,
    sharpness: f64,
    blur: f64,
    lut_paths: &[String],
    frei0r_effects: &[crate::model::clip::Frei0rEffect],
    brightness_kfs: &[crate::model::clip::NumericKeyframe],
    contrast_kfs: &[crate::model::clip::NumericKeyframe],
    saturation_kfs: &[crate::model::clip::NumericKeyframe],
    temperature_kfs: &[crate::model::clip::NumericKeyframe],
    tint_kfs: &[crate::model::clip::NumericKeyframe],
) -> String {
    let mut hasher = crate::media::cache_key::CacheKeyHasher::new();
    fold_baked_fields(
        &mut hasher,
        source_path,
        source_in,
        source_out,
        brightness,
        contrast,
        saturation,
        temperature,
        tint,
        exposure,
        black_point,
        shadows,
        highlights,
        denoise,
        sharpness,
        blur,
        lut_paths,
        frei0r_effects,
    );
    // Insert color-KF fold between the scalar color block and the
    // denoise/sharpness/blur block, matching cache_key_for_clip's
    // ordering. We inline the ordering here to keep both key builders
    // bit-for-bit identical.
    fold_color_keyframes(
        &mut hasher,
        brightness_kfs,
        contrast_kfs,
        saturation_kfs,
        temperature_kfs,
        tint_kfs,
    );
    format!("rr_{:016x}", hasher.finish())
}

/// Stable cache key that folds together the source fingerprint and every
/// baked-scope field on the clip. Transforms, opacity, blend, timeline
/// position, label, and other "live scope" fields are deliberately
/// excluded so the user can keep editing those without invalidating an
/// expensive bake. Routes through the same fold helpers as the
/// ProgramClip-side key so preview lookups match cache population.
pub fn cache_key_for_clip(clip: &Clip) -> String {
    cache_key_for_program_clip_fields(
        &clip.source_path,
        clip.source_in,
        clip.source_out,
        clip.brightness as f64,
        clip.contrast as f64,
        clip.saturation as f64,
        clip.temperature as f64,
        clip.tint as f64,
        clip.exposure as f64,
        clip.black_point as f64,
        clip.shadows as f64,
        clip.highlights as f64,
        clip.denoise as f64,
        clip.sharpness as f64,
        clip.blur as f64,
        &clip.lut_paths,
        &clip.frei0r_effects,
        &clip.brightness_keyframes,
        &clip.contrast_keyframes,
        &clip.saturation_keyframes,
        &clip.temperature_keyframes,
        &clip.tint_keyframes,
    )
}

// ── Bake filter builder ────────────────────────────────────────────────────

/// Assemble the video filter chain that bakes this clip's baked-scope
/// effects into the sidecar. Reuses the same helpers the MP4 export
/// pipeline uses so preview-vs-export parity is preserved downstream.
fn build_bake_video_filter(clip: &Clip) -> String {
    let mut parts: Vec<String> = Vec::new();

    let lut = crate::media::export::build_lut_filter_prefix(clip);
    if !lut.is_empty() {
        parts.push(lut);
    }
    let color = crate::media::export::build_color_filter(clip);
    if !color.is_empty() {
        parts.push(color);
    }
    let denoise = crate::media::export::build_denoise_filter(clip);
    if !denoise.is_empty() {
        parts.push(denoise);
    }
    let sharpen = crate::media::export::build_sharpen_filter(clip);
    if !sharpen.is_empty() {
        parts.push(sharpen);
    }
    let blur = crate::media::export::build_blur_filter(clip);
    if !blur.is_empty() {
        parts.push(blur);
    }
    let frei0r = crate::media::export::build_frei0r_effects_filter(clip);
    if !frei0r.is_empty() {
        parts.push(frei0r);
    }

    if parts.is_empty() {
        // No effects in the baked scope — return a simple passthrough so
        // the sidecar is still valid ProRes. (Could be optimized by
        // skipping the request entirely; left as-is for Phase 1
        // simplicity.)
        "null".to_string()
    } else {
        parts.join(",")
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Which clip kinds can be baked to a render-replace sidecar. Anything
/// without a single backing media file on disk is excluded: compound
/// clips are virtual sub-timelines, multicam clips choose among
/// angles, titles / adjustments are synthetic, and drawings already
/// have their own render cache. Callers should also check
/// `source_path` is non-empty before requesting a bake — defense in
/// depth for imported projects where a bakeable kind happens to have
/// an empty path.
pub fn is_bakeable_kind(kind: &ClipKind) -> bool {
    matches!(kind, ClipKind::Video | ClipKind::Image | ClipKind::Audio)
}

fn dirs_cache_root() -> PathBuf {
    crate::media::cache_support::cache_root_dir("render_replace")
}

pub fn cache_root_dir() -> PathBuf {
    dirs_cache_root()
}

fn file_is_ready(path: &str) -> bool {
    crate::media::cache_support::file_has_content(path)
}

fn touch_mtime(path: &str) {
    use std::ffi::CString;
    if let Ok(c_path) = CString::new(path) {
        unsafe {
            let _ = libc::utime(c_path.as_ptr(), std::ptr::null());
        }
    }
}

/// Invoke ffmpeg to produce one render-replace sidecar. Output is
/// ProRes 422 HQ video + PCM s24le audio in MOV; source window is
/// trimmed by `-ss` + `-t` so only the clip's active range is baked.
fn run_render_replace_job(job: &RenderReplaceJob) -> bool {
    let duration = if job.duration_seconds > 0.001 {
        format!("{:.6}", job.duration_seconds)
    } else {
        // Fall back to "no -t" (full remaining source); should never
        // trigger for a valid Clip whose source_out > source_in.
        String::new()
    };
    let start = format!("{:.6}", job.start_seconds.max(0.0));

    log::info!(
        "RenderReplaceCache: ffmpeg src={} -> out={} start={}s dur={}s filter={}",
        job.source_path,
        job.output_path,
        start,
        duration,
        if job.video_filter.is_empty() {
            "(passthrough)"
        } else {
            job.video_filter.as_str()
        }
    );
    let mut args: Vec<String> = vec![
        "-y".into(),
        "-loglevel".into(),
        "warning".into(),
        "-ss".into(),
        start,
    ];
    if !duration.is_empty() {
        args.push("-t".into());
        args.push(duration);
    }
    args.push("-i".into());
    args.push(job.source_path.clone());
    args.push("-map".into());
    args.push("0:v:0?".into());
    args.push("-map".into());
    args.push("0:a?".into());
    if !job.video_filter.is_empty() {
        args.push("-vf".into());
        args.push(job.video_filter.clone());
    }
    args.extend([
        "-c:v".into(),
        "prores_ks".into(),
        "-profile:v".into(),
        "3".into(),
        "-vendor".into(),
        "apl0".into(),
        "-pix_fmt".into(),
        "yuv422p10le".into(),
        "-c:a".into(),
        "pcm_s24le".into(),
        "-movflags".into(),
        "+faststart".into(),
        job.output_path.clone(),
    ]);
    let status = Command::new("ffmpeg").args(&args).status();
    match status {
        Ok(s) if s.success() => true,
        Ok(s) => {
            log::warn!("RenderReplaceCache: ffmpeg exited with {s:?}");
            false
        }
        Err(e) => {
            log::warn!("RenderReplaceCache: ffmpeg spawn error: {e}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clip::{Clip, ClipKind, Frei0rEffect};
    use std::collections::HashMap;

    fn make_clip() -> Clip {
        Clip::new("/tmp/fake.mp4", 5_000_000_000, 0, ClipKind::Video)
    }

    #[test]
    fn signature_stable_across_unrelated_changes() {
        let mut a = make_clip();
        a.brightness = 0.1;
        a.lut_paths.push("/tmp/look.cube".into());
        let key_a = cache_key_for_clip(&a);

        // Unrelated fields: timeline_start, label, scale, position,
        // opacity, rotation. These are the "live scope" and must not
        // invalidate the bake.
        let mut b = a.clone();
        b.timeline_start = 12_000_000_000;
        b.label = "Renamed".to_string();
        b.scale = 2.5;
        b.position_x = 0.3;
        b.position_y = -0.4;
        b.opacity = 0.5;
        b.rotate = 45;
        let key_b = cache_key_for_clip(&b);

        assert_eq!(key_a, key_b, "signature should survive live-scope edits");
    }

    #[test]
    fn signature_changes_on_brightness() {
        let a = make_clip();
        let mut b = a.clone();
        b.brightness = 0.25;
        assert_ne!(cache_key_for_clip(&a), cache_key_for_clip(&b));
    }

    #[test]
    fn signature_changes_on_lut_addition() {
        let a = make_clip();
        let mut b = a.clone();
        b.lut_paths.push("/tmp/another.cube".into());
        assert_ne!(cache_key_for_clip(&a), cache_key_for_clip(&b));
    }

    #[test]
    fn signature_changes_on_frei0r_param() {
        let mut a = make_clip();
        let mut params = HashMap::new();
        params.insert("Size".to_string(), 0.1_f64);
        a.frei0r_effects.push(Frei0rEffect {
            id: "eff-1".into(),
            plugin_name: "boxblur".into(),
            enabled: true,
            params,
            string_params: HashMap::new(),
        });
        let mut b = a.clone();
        if let Some(eff) = b.frei0r_effects.first_mut() {
            eff.params.insert("Size".to_string(), 0.2_f64);
        }
        assert_ne!(cache_key_for_clip(&a), cache_key_for_clip(&b));
    }

    #[test]
    fn request_is_noop_after_ready() {
        let mut cache = RenderReplaceCache::new();
        let clip = make_clip();
        let key = cache_key_for_clip(&clip);
        // Pretend the sidecar is already cached.
        cache.paths.insert(key.clone(), "/tmp/fake_sidecar.mov".into());
        let before = cache.total_requested;
        cache.request(&clip);
        assert_eq!(cache.total_requested, before);
    }

    #[test]
    fn retry_clears_failed_state() {
        let mut cache = RenderReplaceCache::new();
        let clip = make_clip();
        let key = cache_key_for_clip(&clip);
        cache.failed.insert(key);
        assert!(cache.retry(&clip));
        assert!(!cache.retry(&clip)); // second retry is a no-op
    }

    #[test]
    fn request_skips_non_bakeable_kinds() {
        // Compound / Multicam / Title / Adjustment / Drawing clips
        // have no file-backed `source_path` so ffmpeg would fail on
        // them — `request()` must silently decline.
        let mut cache = RenderReplaceCache::new();
        for kind in [
            ClipKind::Compound,
            ClipKind::Multicam,
            ClipKind::Title,
            ClipKind::Adjustment,
            ClipKind::Drawing,
        ] {
            let mut clip = Clip::new("", 5_000_000_000, 0, kind);
            clip.render_replace_enabled = true;
            let before = cache.total_requested;
            cache.request(&clip);
            assert_eq!(
                cache.total_requested, before,
                "non-bakeable kind should not queue a job"
            );
        }
    }

    #[test]
    fn request_skips_empty_source_path() {
        // Even for a bakeable kind, an empty source path (e.g. from a
        // corrupt import) must skip the bake — otherwise ffmpeg gets
        // an empty `-i` argument and fails with "No such file or
        // directory" on an empty string.
        let mut cache = RenderReplaceCache::new();
        let mut clip = Clip::new("", 5_000_000_000, 0, ClipKind::Video);
        clip.render_replace_enabled = true;
        let before = cache.total_requested;
        cache.request(&clip);
        assert_eq!(cache.total_requested, before);
    }

    #[test]
    fn is_bakeable_kind_covers_file_backed_kinds() {
        assert!(is_bakeable_kind(&ClipKind::Video));
        assert!(is_bakeable_kind(&ClipKind::Image));
        assert!(is_bakeable_kind(&ClipKind::Audio));
        assert!(!is_bakeable_kind(&ClipKind::Compound));
        assert!(!is_bakeable_kind(&ClipKind::Multicam));
        assert!(!is_bakeable_kind(&ClipKind::Title));
        assert!(!is_bakeable_kind(&ClipKind::Adjustment));
        assert!(!is_bakeable_kind(&ClipKind::Drawing));
    }
}
