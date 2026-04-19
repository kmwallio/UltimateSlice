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
use crate::model::project::Project;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, Arc, Mutex};
use std::time::SystemTime;

/// Depth cap for the recursive signature / readiness walks. Matches the
/// existing `clip_to_program_clips` cap in window.rs so we never follow
/// pathological nesting that the preview path won't render anyway.
const MAX_COMPOUND_DEPTH: usize = 16;

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

/// A single bake job queued onto the cache worker thread. The leaf
/// variant invokes ffmpeg directly with a precomputed filter string
/// (Phase 1b — one source file, one filter chain, one output). The
/// compound variant invokes the full project export pipeline on a
/// synthetic project wrapping the compound's internal tracks (Phase
/// 2 — flattens transitions / audio mix / nested compounds through
/// the existing export code path).
enum RenderReplaceJob {
    Leaf {
        cache_key: String,
        source_path: String,
        output_path: String,
        video_filter: String,
        start_seconds: f64,
        duration_seconds: f64,
    },
    Compound {
        cache_key: String,
        synthetic_project: Project,
        output_path: String,
        bg_removal_paths: HashMap<String, String>,
        frame_interp_paths: HashMap<String, String>,
    },
}

impl RenderReplaceJob {
    fn cache_key(&self) -> &str {
        match self {
            RenderReplaceJob::Leaf { cache_key, .. } => cache_key,
            RenderReplaceJob::Compound { cache_key, .. } => cache_key,
        }
    }
    fn output_path(&self) -> &str {
        match self {
            RenderReplaceJob::Leaf { output_path, .. } => output_path,
            RenderReplaceJob::Compound { output_path, .. } => output_path,
        }
    }
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
    /// Bg-removal sidecar paths (source_path → file). Refreshed from the
    /// window-side poll loop; snapshotted into compound jobs at request
    /// time so the export pipeline can swap in those sidecars for
    /// internal clips with `bg_removal_enabled`.
    bg_removal_paths: HashMap<String, String>,
    /// Frame-interpolation sidecar paths (clip_id → file). Same lifecycle
    /// as `bg_removal_paths`.
    frame_interp_paths: HashMap<String, String>,
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
                    let (cache_key, output_path) =
                        (job.cache_key().to_string(), job.output_path().to_string());
                    let success = run_render_replace_job(job);
                    let _ = tx.send(WorkerUpdate::Done(WorkerResult {
                        cache_key,
                        output_path,
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
            bg_removal_paths: HashMap::new(),
            frame_interp_paths: HashMap::new(),
        }
    }

    /// Update the bg-removal sidecar snapshot. Compound bakes launched
    /// after this call see the new map; in-flight jobs keep their
    /// snapshot. Mirrors `ProgramPlayer::update_bg_removal_paths`'s
    /// lifecycle.
    pub fn set_bg_removal_paths(&mut self, paths: HashMap<String, String>) {
        self.bg_removal_paths = paths;
    }

    /// Update the frame-interpolation sidecar snapshot. Same lifecycle
    /// as `set_bg_removal_paths`.
    pub fn set_frame_interp_paths(&mut self, paths: HashMap<String, String>) {
        self.frame_interp_paths = paths;
    }

    /// Queue a render-replace bake for `clip`. Does nothing when the
    /// signature is already ready, in-flight, or known-failed, or when
    /// the clip kind isn't bakeable (Compound is bakeable in Phase 2;
    /// Multicam / Title / Adjustment / Drawing remain excluded).
    ///
    /// Leaf clips (Video/Image/Audio) go through the inline ffmpeg path
    /// inherited from Phase 1b. Compound clips route through the full
    /// export pipeline on a synthetic Project wrapping their internal
    /// tracks (Phase 2).
    pub fn request(&mut self, clip: &Clip) {
        match clip.kind {
            ClipKind::Compound => self.request_compound(clip),
            _ => self.request_leaf(clip),
        }
    }

    /// Leaf-kind (Video / Image / Audio) request path — identical to
    /// the Phase 1b behaviour.
    fn request_leaf(&mut self, clip: &Clip) {
        if !matches!(
            clip.kind,
            ClipKind::Video | ClipKind::Image | ClipKind::Audio
        ) || clip.source_path.trim().is_empty()
        {
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
        let start_seconds = clip.source_in as f64 / 1_000_000_000.0;
        let duration_seconds =
            clip.source_out.saturating_sub(clip.source_in) as f64 / 1_000_000_000.0;

        self.total_requested += 1;
        self.pending.insert(key.clone());
        if let Some(ref tx) = self.work_tx {
            let _ = tx.send(RenderReplaceJob::Leaf {
                cache_key: key,
                source_path: clip.source_path.clone(),
                output_path,
                video_filter,
                start_seconds,
                duration_seconds,
            });
        }
    }

    /// Compound bake request. Defers when any nested compound isn't
    /// yet Ready — the 500 ms project-side poll cycle naturally
    /// re-evaluates once inner sidecars land. Builds a synthetic
    /// Project from the compound's internal tracks and hands it to
    /// the worker; the compound's own transform / opacity / blend /
    /// color / transitions are NOT baked (they stay live per the
    /// Phase 2 locked design).
    pub fn request_compound(&mut self, compound: &Clip) {
        if compound.kind != ClipKind::Compound {
            return;
        }
        let tracks = match compound.compound_tracks.as_ref() {
            Some(t) if !t.is_empty() => t,
            _ => return, // empty or non-compound — nothing to bake
        };

        // Inner readiness: any nested compound that has the toggle on
        // but no sidecar yet must bake first. Skip silently — the
        // project-change walker queues the inner requests and the
        // outer request re-runs on the next tick.
        if !self.nested_compounds_ready(tracks, 0) {
            log::debug!(
                "RenderReplaceCache: deferring compound bake (inner compound not ready)"
            );
            return;
        }

        let key = cache_key_for_compound(compound);

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
            log::info!(
                "RenderReplaceCache: found existing compound sidecar for key={}",
                key
            );
            touch_mtime(&output_path);
            self.paths.insert(key, output_path);
            return;
        } else if Path::new(&output_path).exists() {
            let _ = std::fs::remove_file(&output_path);
        }

        self.evict_if_oversized();

        // The synthetic project needs the parent project's resolution
        // and frame rate. The caller (window.rs) supplies those via
        // `request_compound_with_parent`; when callers go through the
        // plain `request(&Clip)` path without parent context, default
        // to 1920x1080 @ 24 fps (the `Project::new` defaults). This
        // keeps the signature-level guarantee — `cache_key_for_compound`
        // does NOT fold dims or fps, so the cache hit rate is
        // unaffected by the fallback.
        let synthetic_project = match build_synthetic_project_for_compound(
            compound, 1920, 1080, 24, 1,
        ) {
            Some(p) => p,
            None => return,
        };

        self.total_requested += 1;
        self.pending.insert(key.clone());
        if let Some(ref tx) = self.work_tx {
            let _ = tx.send(RenderReplaceJob::Compound {
                cache_key: key,
                synthetic_project,
                output_path,
                bg_removal_paths: self.bg_removal_paths.clone(),
                frame_interp_paths: self.frame_interp_paths.clone(),
            });
        }
    }

    /// Variant of `request_compound` that carries the parent project's
    /// resolution + frame rate through to the synthetic project. The
    /// window-side walker uses this so compound bakes render at the
    /// correct canvas dimensions even when nested in a non-default
    /// project.
    pub fn request_compound_with_parent(
        &mut self,
        compound: &Clip,
        parent_width: u32,
        parent_height: u32,
        parent_fps_num: u32,
        parent_fps_den: u32,
    ) {
        if compound.kind != ClipKind::Compound {
            return;
        }
        let tracks = match compound.compound_tracks.as_ref() {
            Some(t) if !t.is_empty() => t,
            _ => return,
        };
        if !self.nested_compounds_ready(tracks, 0) {
            return;
        }
        let key = cache_key_for_compound(compound);
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
            log::info!(
                "RenderReplaceCache: found existing compound sidecar for key={}",
                key
            );
            touch_mtime(&output_path);
            self.paths.insert(key, output_path);
            return;
        } else if Path::new(&output_path).exists() {
            let _ = std::fs::remove_file(&output_path);
        }
        self.evict_if_oversized();
        let synthetic_project = match build_synthetic_project_for_compound(
            compound,
            parent_width,
            parent_height,
            parent_fps_num,
            parent_fps_den,
        ) {
            Some(p) => p,
            None => return,
        };
        self.total_requested += 1;
        self.pending.insert(key.clone());
        if let Some(ref tx) = self.work_tx {
            let _ = tx.send(RenderReplaceJob::Compound {
                cache_key: key,
                synthetic_project,
                output_path,
                bg_removal_paths: self.bg_removal_paths.clone(),
                frame_interp_paths: self.frame_interp_paths.clone(),
            });
        }
    }

    /// Walk `compound_tracks` checking whether every nested compound
    /// with `render_replace_enabled` has a ready sidecar. Leaf-kind
    /// render-replace clips are not checked here — the leaf bake is
    /// orthogonal to compound bakes and their sidecars are consumed
    /// by the export pipeline via its own sidecar lookups, not by
    /// this cache's path map.
    fn nested_compounds_ready(
        &self,
        tracks: &[crate::model::track::Track],
        depth: usize,
    ) -> bool {
        if depth >= MAX_COMPOUND_DEPTH {
            return true; // depth-capped: assume ready to avoid infinite defer
        }
        for track in tracks {
            for clip in &track.clips {
                if clip.kind == ClipKind::Compound && clip.render_replace_enabled {
                    let inner_key = cache_key_for_compound(clip);
                    if !self.paths.contains_key(&inner_key) {
                        return false;
                    }
                }
                if let Some(ref inner) = clip.compound_tracks {
                    if !self.nested_compounds_ready(inner, depth + 1) {
                        return false;
                    }
                }
            }
        }
        true
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

/// Compound signature: folds every internal clip's baked-scope fields
/// plus the compositing-level state that affects the final rendered
/// pixels (transform, opacity, blend, transitions, track order, track
/// metadata), the outer compound's window (`source_in` / `source_out`),
/// and the signatures of any nested compounds. Excludes the compound's
/// OWN transform / opacity / blend / color — those stay live.
pub fn cache_key_for_compound(compound: &Clip) -> String {
    let mut hasher = crate::media::cache_key::CacheKeyHasher::new();
    hasher.add("compound");
    hasher.add(compound.source_in).add(compound.source_out);
    if let Some(ref tracks) = compound.compound_tracks {
        fold_compound_tracks(&mut hasher, tracks, 0);
    }
    format!("rr_compound_{:016x}", hasher.finish())
}

fn fold_compound_tracks(
    hasher: &mut crate::media::cache_key::CacheKeyHasher,
    tracks: &[crate::model::track::Track],
    depth: usize,
) {
    if depth >= MAX_COMPOUND_DEPTH {
        return;
    }
    for (track_idx, track) in tracks.iter().enumerate() {
        hasher
            .add(track_idx as u32)
            .add(track.id.as_str())
            .add(if track.muted { 1u8 } else { 0u8 })
            .add(if track.locked { 1u8 } else { 0u8 })
            .add(if track.duck { 1u8 } else { 0u8 })
            .add((track.duck_amount_db * 100.0) as i64)
            .add((track.pan * 10_000.0) as i64)
            .add((track.gain_db * 100.0) as i64);
        // Track audio role enum → discriminant bytes.
        let role_tag: u8 = match track.audio_role {
            crate::model::track::AudioRole::None => 0,
            crate::model::track::AudioRole::Dialogue => 1,
            crate::model::track::AudioRole::Effects => 2,
            crate::model::track::AudioRole::Music => 3,
        };
        hasher.add(role_tag);
        for clip in &track.clips {
            fold_inner_clip(hasher, clip, depth);
        }
    }
}

fn fold_inner_clip(
    hasher: &mut crate::media::cache_key::CacheKeyHasher,
    clip: &Clip,
    depth: usize,
) {
    // Clip identity + compositing-layer fields that the compositor reads.
    hasher
        .add(clip.id.as_str())
        .add(clip.timeline_start)
        .add(clip.source_in)
        .add(clip.source_out)
        .add((clip.scale * 10_000.0) as i64)
        .add((clip.position_x * 10_000.0) as i64)
        .add((clip.position_y * 10_000.0) as i64)
        .add(clip.rotate as i32)
        .add((clip.opacity * 10_000.0) as i64)
        .add(clip.crop_left)
        .add(clip.crop_right)
        .add(clip.crop_top)
        .add(clip.crop_bottom)
        .add(if clip.flip_h { 1u8 } else { 0u8 })
        .add(if clip.flip_v { 1u8 } else { 0u8 })
        .add((clip.anamorphic_desqueeze * 10_000.0) as i64);
    // Blend mode: use the serde snake_case name for stability.
    let blend_name = match serde_json::to_value(&clip.blend_mode) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::new(),
    };
    hasher.add(blend_name.as_str());
    // Outgoing transition: an empty `kind` with duration_ns == 0
    // represents "no transition" — the default. Fold kind string +
    // duration + alignment serde name so changes at the boundary
    // invalidate the bake.
    let t = &clip.outgoing_transition;
    if !t.kind.is_empty() || t.duration_ns != 0 {
        let align_name = match serde_json::to_value(&t.alignment) {
            Ok(serde_json::Value::String(s)) => s,
            _ => String::new(),
        };
        hasher
            .add("trans")
            .add(t.kind.as_str())
            .add(t.duration_ns)
            .add(align_name.as_str());
    }
    // Speed / reverse / freeze — compositing-timing state that changes
    // the rendered pixels per frame.
    hasher
        .add((clip.speed * 10_000.0) as i64)
        .add(if clip.reverse { 1u8 } else { 0u8 })
        .add(if clip.freeze_frame { 1u8 } else { 0u8 });

    // Nested compound: recurse into its window + internal tracks.
    if clip.kind == ClipKind::Compound {
        hasher.add("nested_compound");
        hasher.add(clip.source_in).add(clip.source_out);
        if let Some(ref inner) = clip.compound_tracks {
            fold_compound_tracks(hasher, inner, depth + 1);
        }
        return;
    }

    // Leaf clip: fold its baked-scope signature (same key-space as
    // `cache_key_for_clip` minus the prefix/suffix). This means an inner
    // leaf clip with its own render-replace sidecar shares hash bits
    // with a standalone version — fine, signatures match means the
    // baked content is identical.
    let leaf_key = cache_key_for_clip(clip);
    hasher.add(leaf_key.as_str());
}

/// Construct a synthetic Project that wraps a compound's internal
/// tracks at the parent project's resolution + frame rate. The export
/// pipeline consumes this as if it were a top-level project, handling
/// flattening of nested compounds, transitions, audio mix, titles,
/// adjustment layers, and masks automatically.
///
/// Returns `None` when the compound has no tracks (empty compound —
/// nothing to bake). The synthetic project is deliberately minimal:
/// master_gain_db = 0 (don't apply project-level gain inside a
/// compound bake), no markers, no audio buses, no FCPXML unknown
/// passthrough (this project is thrown away after the export finishes).
pub fn build_synthetic_project_for_compound(
    compound: &Clip,
    parent_width: u32,
    parent_height: u32,
    parent_fps_num: u32,
    parent_fps_den: u32,
) -> Option<Project> {
    let tracks = compound.compound_tracks.as_ref()?;
    if tracks.is_empty() {
        return None;
    }
    let mut synthetic = Project::new(format!("Compound bake: {}", compound.label));
    synthetic.width = parent_width.max(1);
    synthetic.height = parent_height.max(1);
    synthetic.frame_rate = crate::model::project::FrameRate {
        numerator: parent_fps_num.max(1),
        denominator: parent_fps_den.max(1),
    };
    synthetic.tracks = tracks.clone();
    synthetic.markers.clear();
    synthetic.master_gain_db = 0.0;
    synthetic.reference_stills.clear();
    synthetic.dirty = false;
    synthetic.file_path = None;

    // Keep subtitles OUT of the baked pixels. The Program Monitor's
    // subtitle overlay already recurses through `compound_tracks`
    // independently (window.rs:14365) and reads from the real
    // project model, so inner-clip subtitles keep rendering live at
    // the correct playhead time. Leaving them editable after a bake
    // is important — subtitle text / styling is a late-pass workflow
    // and users shouldn't lose editability by toggling
    // Render-and-Replace.
    strip_subtitle_visibility_recursive(&mut synthetic.tracks, 0);

    Some(synthetic)
}

/// Flip `subtitle_visible = false` on every leaf clip in `tracks`,
/// recursing through nested compounds up to `MAX_COMPOUND_DEPTH`. The
/// export pipeline's subtitle burn-in checks this flag (export.rs:910)
/// and skips clips where it's false — so the baked sidecar has zero
/// subtitle pixels. Only the synthetic project's clone of the tracks
/// is mutated; the live project model is untouched and the Program
/// Monitor overlay keeps drawing subtitles from there.
fn strip_subtitle_visibility_recursive(
    tracks: &mut [crate::model::track::Track],
    depth: usize,
) {
    if depth >= MAX_COMPOUND_DEPTH {
        return;
    }
    for track in tracks.iter_mut() {
        for clip in track.clips.iter_mut() {
            clip.subtitle_visible = false;
            if let Some(ref mut inner) = clip.compound_tracks {
                strip_subtitle_visibility_recursive(inner, depth + 1);
            }
        }
    }
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

/// Which clip kinds can be baked to a render-replace sidecar.
/// Compound clips bake via the full export pipeline on a synthetic
/// Project (Phase 2). File-backed leaf kinds (Video / Image / Audio)
/// bake via the inline ffmpeg filter chain (Phase 1b). Multicam
/// clips choose among angles, titles / adjustments are synthetic, and
/// drawings already have their own render cache — those stay out of
/// scope. For leaf kinds, callers should also check `source_path` is
/// non-empty before requesting a bake.
pub fn is_bakeable_kind(kind: &ClipKind) -> bool {
    matches!(
        kind,
        ClipKind::Video | ClipKind::Image | ClipKind::Audio | ClipKind::Compound
    )
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

/// Dispatch worker: runs the appropriate pipeline for the job variant.
/// Returns true on success, false on any ffmpeg / export failure.
fn run_render_replace_job(job: RenderReplaceJob) -> bool {
    match job {
        RenderReplaceJob::Leaf {
            source_path,
            output_path,
            video_filter,
            start_seconds,
            duration_seconds,
            ..
        } => run_leaf_bake(
            &source_path,
            &output_path,
            &video_filter,
            start_seconds,
            duration_seconds,
        ),
        RenderReplaceJob::Compound {
            synthetic_project,
            output_path,
            bg_removal_paths,
            frame_interp_paths,
            ..
        } => run_compound_bake(
            &synthetic_project,
            &output_path,
            &bg_removal_paths,
            &frame_interp_paths,
        ),
    }
}

/// Leaf bake: invoke ffmpeg directly with a precomputed filter string.
/// Output is ProRes 422 HQ + PCM s24le in MOV; source window is
/// trimmed by `-ss` + `-t`.
fn run_leaf_bake(
    source_path: &str,
    output_path: &str,
    video_filter: &str,
    start_seconds: f64,
    duration_seconds: f64,
) -> bool {
    let duration = if duration_seconds > 0.001 {
        format!("{:.6}", duration_seconds)
    } else {
        String::new()
    };
    let start = format!("{:.6}", start_seconds.max(0.0));

    log::info!(
        "RenderReplaceCache: leaf ffmpeg src={} -> out={} start={}s dur={}s filter={}",
        source_path,
        output_path,
        start,
        duration,
        if video_filter.is_empty() {
            "(passthrough)"
        } else {
            video_filter
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
    args.push(source_path.to_string());
    args.push("-map".into());
    args.push("0:v:0?".into());
    args.push("-map".into());
    args.push("0:a?".into());
    if !video_filter.is_empty() {
        args.push("-vf".into());
        args.push(video_filter.to_string());
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
        output_path.to_string(),
    ]);
    let status = Command::new("ffmpeg").args(&args).status();
    match status {
        Ok(s) if s.success() => true,
        Ok(s) => {
            log::warn!("RenderReplaceCache: leaf ffmpeg exited with {s:?}");
            false
        }
        Err(e) => {
            log::warn!("RenderReplaceCache: leaf ffmpeg spawn error: {e}");
            false
        }
    }
}

/// Compound bake: route a synthetic Project (wrapping the compound's
/// internal tracks) through the existing export pipeline so the baked
/// sidecar includes transitions, audio mix, nested compound flattening,
/// titles, adjustment layers, and all the other export-side
/// correctness that a per-clip filter chain can't express. The export
/// pipeline already runs ffmpeg as a subprocess internally, so this
/// stays on the worker thread alongside leaf bakes.
fn run_compound_bake(
    synthetic_project: &Project,
    output_path: &str,
    bg_removal_paths: &HashMap<String, String>,
    frame_interp_paths: &HashMap<String, String>,
) -> bool {
    use crate::media::export::{
        AudioChannelLayout, AudioCodec, Container, ExportOptions, VideoCodec,
    };
    log::info!(
        "RenderReplaceCache: compound bake → {} ({} tracks)",
        output_path,
        synthetic_project.tracks.len()
    );
    let options = ExportOptions {
        video_codec: VideoCodec::ProRes,
        container: Container::Mov,
        audio_codec: AudioCodec::Pcm,
        audio_channel_layout: AudioChannelLayout::Stereo,
        hdr_passthrough: false,
        output_width: 0,
        output_height: 0,
        ..Default::default()
    };
    // The export pipeline takes an mpsc progress channel; we discard
    // progress events because the cache only cares about the final
    // success/failure.
    let (tx, _rx) = mpsc::channel();
    match crate::media::export::export_project(
        synthetic_project,
        output_path,
        options,
        None,
        bg_removal_paths,
        frame_interp_paths,
        tx,
    ) {
        Ok(()) => true,
        Err(e) => {
            log::warn!("RenderReplaceCache: compound export failed: {e}");
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

    /// Helper — build a compound clip wrapping a single video track
    /// with one internal video clip. The compound itself has default
    /// transform/opacity so its own fields are neutral; tests mutate
    /// whichever fields they need.
    fn make_compound_with_one_inner(inner_brightness: f32) -> Clip {
        let mut inner = Clip::new("/tmp/inner.mp4", 5_000_000_000, 0, ClipKind::Video);
        inner.brightness = inner_brightness;
        let mut track = crate::model::track::Track::new_video("V1");
        track.clips.push(inner);
        let mut compound = Clip::new("", 5_000_000_000, 0, ClipKind::Compound);
        compound.compound_tracks = Some(vec![track]);
        compound
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
        // Multicam / Title / Adjustment / Drawing clips remain out of
        // scope — the bake path would fail for them. Compound clips
        // are now bakeable (Phase 2) and covered by a separate test.
        let mut cache = RenderReplaceCache::new();
        for kind in [
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

    // ── Phase 2: compound signature + readiness + synthetic project ──

    #[test]
    fn compound_signature_stable_across_live_scope_edits() {
        let a = make_compound_with_one_inner(0.15);
        let key_a = cache_key_for_compound(&a);

        // Live-scope edits on the COMPOUND itself: transform, opacity,
        // blend, timeline position, label, own color. None of these
        // should invalidate — they apply on top of the baked sidecar
        // at playback time.
        let mut b = a.clone();
        b.timeline_start = 12_000_000_000;
        b.label = "Renamed".to_string();
        b.scale = 2.5;
        b.position_x = 0.3;
        b.position_y = -0.4;
        b.opacity = 0.5;
        b.rotate = 45;
        b.brightness = 0.1; // compound's OWN color stays live
        b.contrast = 1.2;
        let key_b = cache_key_for_compound(&b);

        assert_eq!(
            key_a, key_b,
            "compound signature should survive live-scope edits"
        );
    }

    #[test]
    fn compound_signature_changes_on_inner_clip_edit() {
        let a = make_compound_with_one_inner(0.0);
        let b = make_compound_with_one_inner(0.25);
        assert_ne!(
            cache_key_for_compound(&a),
            cache_key_for_compound(&b),
            "editing an inner clip's brightness must invalidate"
        );
    }

    #[test]
    fn compound_signature_recurses_into_nested_compound() {
        // Outer compound contains an inner compound that contains a
        // leaf. Changing the deeply-nested leaf's brightness must flip
        // the outer compound's signature.
        let mut inner_leaf = Clip::new("/tmp/leaf.mp4", 5_000_000_000, 0, ClipKind::Video);
        inner_leaf.brightness = 0.0;
        let mut inner_track = crate::model::track::Track::new_video("V1");
        inner_track.clips.push(inner_leaf.clone());
        let mut inner_compound = Clip::new("", 5_000_000_000, 0, ClipKind::Compound);
        inner_compound.compound_tracks = Some(vec![inner_track]);
        let mut outer_track = crate::model::track::Track::new_video("V1");
        outer_track.clips.push(inner_compound);
        let mut outer = Clip::new("", 5_000_000_000, 0, ClipKind::Compound);
        outer.compound_tracks = Some(vec![outer_track]);
        let key_a = cache_key_for_compound(&outer);

        // Now edit the nested leaf's brightness.
        let outer_tracks = outer.compound_tracks.as_mut().unwrap();
        let inner_compound_mut = &mut outer_tracks[0].clips[0];
        let inner_tracks = inner_compound_mut.compound_tracks.as_mut().unwrap();
        inner_tracks[0].clips[0].brightness = 0.25;
        let key_b = cache_key_for_compound(&outer);

        assert_ne!(
            key_a, key_b,
            "signature must reach through nested compounds"
        );
    }

    #[test]
    fn request_defers_compound_when_inner_compound_not_ready() {
        // Outer compound with render_replace_enabled on, containing an
        // inner compound ALSO with render_replace_enabled on but no
        // sidecar in the cache. The outer request must not queue a
        // job — inner bakes first.
        let mut inner_leaf = Clip::new("/tmp/leaf.mp4", 5_000_000_000, 0, ClipKind::Video);
        inner_leaf.brightness = 0.1;
        let mut inner_track = crate::model::track::Track::new_video("V1");
        inner_track.clips.push(inner_leaf);
        let mut inner_compound = Clip::new("", 5_000_000_000, 0, ClipKind::Compound);
        inner_compound.compound_tracks = Some(vec![inner_track]);
        inner_compound.render_replace_enabled = true;
        let mut outer_track = crate::model::track::Track::new_video("V1");
        outer_track.clips.push(inner_compound);
        let mut outer = Clip::new("", 5_000_000_000, 0, ClipKind::Compound);
        outer.compound_tracks = Some(vec![outer_track]);
        outer.render_replace_enabled = true;

        let mut cache = RenderReplaceCache::new();
        let before = cache.total_requested;
        cache.request_compound(&outer);
        assert_eq!(
            cache.total_requested, before,
            "outer compound should defer while inner compound is not Ready"
        );
    }

    #[test]
    fn synthetic_project_none_for_empty_compound() {
        let empty = Clip::new("", 5_000_000_000, 0, ClipKind::Compound);
        assert!(build_synthetic_project_for_compound(&empty, 1920, 1080, 24, 1).is_none());
    }

    #[test]
    fn synthetic_project_inherits_parent_dims_and_fps() {
        let compound = make_compound_with_one_inner(0.0);
        let synthetic =
            build_synthetic_project_for_compound(&compound, 3840, 2160, 60_000, 1001).unwrap();
        assert_eq!(synthetic.width, 3840);
        assert_eq!(synthetic.height, 2160);
        assert_eq!(synthetic.frame_rate.numerator, 60_000);
        assert_eq!(synthetic.frame_rate.denominator, 1001);
        assert!((synthetic.master_gain_db - 0.0).abs() < 1e-9);
        assert!(synthetic.markers.is_empty());
    }

    #[test]
    fn synthetic_project_strips_subtitle_visibility_on_inner_clips() {
        // A compound with an inner leaf that has subtitles enabled.
        // The export-path synthetic project must clear
        // `subtitle_visible` so the baked sidecar has NO burned-in
        // subtitle pixels — the overlay keeps drawing them live.
        let mut inner = Clip::new("/tmp/inner.mp4", 5_000_000_000, 0, ClipKind::Video);
        inner.subtitle_visible = true;
        let mut track = crate::model::track::Track::new_video("V1");
        track.clips.push(inner);
        let mut compound = Clip::new("", 5_000_000_000, 0, ClipKind::Compound);
        compound.compound_tracks = Some(vec![track]);

        let synthetic =
            build_synthetic_project_for_compound(&compound, 1920, 1080, 24, 1).unwrap();
        assert!(
            !synthetic.tracks[0].clips[0].subtitle_visible,
            "synthetic project must have subtitle_visible=false on internal clips"
        );
        // The original compound's internal tracks are untouched —
        // the overlay keeps seeing `subtitle_visible = true` on the
        // real clips.
        assert!(compound.compound_tracks.as_ref().unwrap()[0].clips[0].subtitle_visible);
    }

    #[test]
    fn synthetic_project_strips_subtitles_recursively_through_nested_compound() {
        // Inner-most leaf with subtitle_visible = true.
        let mut deep_leaf = Clip::new("/tmp/deep.mp4", 5_000_000_000, 0, ClipKind::Video);
        deep_leaf.subtitle_visible = true;
        let mut inner_track = crate::model::track::Track::new_video("V1");
        inner_track.clips.push(deep_leaf);
        let mut inner_compound = Clip::new("", 5_000_000_000, 0, ClipKind::Compound);
        inner_compound.compound_tracks = Some(vec![inner_track]);
        let mut outer_track = crate::model::track::Track::new_video("V1");
        outer_track.clips.push(inner_compound);
        let mut outer = Clip::new("", 5_000_000_000, 0, ClipKind::Compound);
        outer.compound_tracks = Some(vec![outer_track]);

        let synthetic = build_synthetic_project_for_compound(&outer, 1920, 1080, 24, 1).unwrap();
        let inner_c = &synthetic.tracks[0].clips[0];
        let deep = &inner_c.compound_tracks.as_ref().unwrap()[0].clips[0];
        assert!(
            !deep.subtitle_visible,
            "subtitle stripping must reach into nested compounds"
        );
    }

    #[test]
    fn compound_signature_unchanged_by_subtitle_edits() {
        // Subtitles stay live on top of baked compounds — editing
        // subtitle text / styling / visibility must NOT invalidate
        // the sidecar (users iterate on subtitles after locking
        // pixels).
        let a = make_compound_with_one_inner(0.0);
        let key_a = cache_key_for_compound(&a);

        let mut b = a.clone();
        let inner = &mut b.compound_tracks.as_mut().unwrap()[0].clips[0];
        inner.subtitle_visible = false;
        inner.subtitle_font = "Sans 48px".to_string();
        inner.subtitle_color = 0xFFCC33FF;
        let key_b = cache_key_for_compound(&b);

        assert_eq!(
            key_a, key_b,
            "compound signature must ignore subtitle edits"
        );
    }

    #[test]
    fn is_bakeable_kind_covers_file_backed_kinds() {
        assert!(is_bakeable_kind(&ClipKind::Video));
        assert!(is_bakeable_kind(&ClipKind::Image));
        assert!(is_bakeable_kind(&ClipKind::Audio));
        assert!(is_bakeable_kind(&ClipKind::Compound)); // Phase 2
        assert!(!is_bakeable_kind(&ClipKind::Multicam));
        assert!(!is_bakeable_kind(&ClipKind::Title));
        assert!(!is_bakeable_kind(&ClipKind::Adjustment));
        assert!(!is_bakeable_kind(&ClipKind::Drawing));
    }
}
