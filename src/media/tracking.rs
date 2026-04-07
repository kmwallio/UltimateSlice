// SPDX-License-Identifier: GPL-3.0-or-later
//! Motion-tracking analysis backend.
//!
//! Follows the background cache pattern used by other media subsystems:
//! GTK/UI code enqueues a [`TrackingJob`], polls [`TrackingCache`] for progress
//! and completions, and receives a fully-populated [`MotionTracker`] ready to
//! attach to a clip or mask. Results are also cached to disk so re-running the
//! same analysis does not decode the source again.

use crate::media::program_player::ProgramClip;
use crate::model::clip::{
    Clip, ClipMask, KeyframeInterpolation, MotionTracker, NumericKeyframe, TrackingBinding,
    TrackingRegion, TrackingSample,
};
use crate::model::track::Track;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;
use tempfile::NamedTempFile;

const CACHE_VERSION: u32 = 1;
const ANALYSIS_WIDTH: usize = 160;
const ANALYSIS_HEIGHT: usize = 90;
const DEFAULT_FRAME_STEP_NS: u64 = 100_000_000;
const DEFAULT_SEARCH_RADIUS_PX: u32 = 18;
const MIN_TEMPLATE_HALF_SIZE_PX: i32 = 4;
const MAX_TRACKING_FRAMES: usize = 240;

#[derive(Debug, Clone)]
pub struct TrackingJob {
    pub tracker_id: String,
    pub tracker_label: String,
    pub source_path: String,
    /// Source timestamp that corresponds to clip-local time 0.
    pub clip_source_in_ns: u64,
    /// Clip-local analysis window start.
    pub analysis_start_ns: u64,
    /// Clip-local analysis window end.
    pub analysis_end_ns: u64,
    pub analysis_region: TrackingRegion,
    /// Requested sample spacing. `0` falls back to a safe default.
    pub frame_step_ns: u64,
    /// Search radius around the previously matched position.
    pub search_radius_px: u32,
}

impl TrackingJob {
    pub fn new(
        tracker_id: impl Into<String>,
        tracker_label: impl Into<String>,
        source_path: impl Into<String>,
        clip_source_in_ns: u64,
        analysis_start_ns: u64,
        analysis_end_ns: u64,
        analysis_region: TrackingRegion,
    ) -> Self {
        Self {
            tracker_id: tracker_id.into(),
            tracker_label: tracker_label.into(),
            source_path: source_path.into(),
            clip_source_in_ns,
            analysis_start_ns,
            analysis_end_ns,
            analysis_region,
            frame_step_ns: DEFAULT_FRAME_STEP_NS,
            search_radius_px: DEFAULT_SEARCH_RADIUS_PX,
        }
    }

    pub fn analysis_duration_ns(&self) -> u64 {
        self.analysis_end_ns.saturating_sub(self.analysis_start_ns)
    }

    pub fn effective_frame_step_ns(&self) -> u64 {
        let requested = if self.frame_step_ns == 0 {
            DEFAULT_FRAME_STEP_NS
        } else {
            self.frame_step_ns
        };
        let duration = self.analysis_duration_ns();
        if duration == 0 {
            return requested;
        }
        let min_step = ((duration as f64) / MAX_TRACKING_FRAMES as f64).ceil() as u64;
        requested.max(min_step.max(1))
    }

    pub fn effective_search_radius_px(&self) -> u32 {
        if self.search_radius_px == 0 {
            DEFAULT_SEARCH_RADIUS_PX
        } else {
            self.search_radius_px
        }
    }

    pub fn estimated_sample_count(&self) -> usize {
        let duration = self.analysis_duration_ns();
        if duration == 0 {
            0
        } else {
            ((duration - 1) / self.effective_frame_step_ns() + 1) as usize
        }
    }

    pub fn cache_key(&self) -> String {
        let mut hasher = DefaultHasher::new();
        CACHE_VERSION.hash(&mut hasher);
        self.source_path.hash(&mut hasher);
        self.clip_source_in_ns.hash(&mut hasher);
        self.analysis_start_ns.hash(&mut hasher);
        self.analysis_end_ns.hash(&mut hasher);
        self.effective_frame_step_ns().hash(&mut hasher);
        self.effective_search_radius_px().hash(&mut hasher);
        quantize_norm(self.analysis_region.center_x).hash(&mut hasher);
        quantize_norm(self.analysis_region.center_y).hash(&mut hasher);
        quantize_norm(self.analysis_region.width).hash(&mut hasher);
        quantize_norm(self.analysis_region.height).hash(&mut hasher);
        quantize_norm(self.analysis_region.rotation_deg).hash(&mut hasher);
        format!("tracking_{:016x}", hasher.finish())
    }
}

#[derive(Debug, Clone)]
pub struct TrackingJobProgress {
    pub processed_samples: usize,
    pub total_samples: usize,
}

#[derive(Debug, Clone)]
pub struct TrackingProgress {
    pub total: usize,
    pub completed: usize,
    pub in_flight: bool,
    pub sample_fraction: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct TrackingPollResult {
    pub cache_key: String,
    pub source_path: String,
    pub tracker: Option<MotionTracker>,
    pub canceled: bool,
    pub error: Option<String>,
}

#[derive(Debug)]
enum WorkerUpdate {
    Progress {
        cache_key: String,
        processed_samples: usize,
        total_samples: usize,
    },
    Done(WorkerResult),
}

#[derive(Debug)]
struct WorkerResult {
    cache_key: String,
    job: TrackingJob,
    analysis: Option<CachedTrackingAnalysis>,
    canceled: bool,
    error: Option<String>,
}

#[derive(Debug)]
struct TrackingWorkerJob {
    cache_key: String,
    job: TrackingJob,
    cancel_flag: Arc<AtomicBool>,
    cache_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct CachedTrackingAnalysis {
    analysis_region: TrackingRegion,
    analysis_start_ns: u64,
    analysis_end_ns: Option<u64>,
    samples: Vec<TrackingSample>,
}

impl CachedTrackingAnalysis {
    fn into_motion_tracker(&self, tracker_id: &str, tracker_label: &str) -> MotionTracker {
        let mut tracker = MotionTracker::new(tracker_label.to_string());
        tracker.id = tracker_id.to_string();
        tracker.analysis_region = self.analysis_region;
        tracker.analysis_start_ns = self.analysis_start_ns;
        tracker.analysis_end_ns = self.analysis_end_ns;
        tracker.samples = self.samples.clone();
        tracker
    }
}

#[derive(Clone)]
struct TrackerSource {
    timeline_start_ns: u64,
    trackers_by_id: HashMap<String, MotionTracker>,
}

fn collect_tracker_sources_from_flat_tracks(tracks: &[Track]) -> HashMap<String, TrackerSource> {
    let mut sources = HashMap::new();
    for track in tracks {
        for clip in &track.clips {
            if clip.motion_trackers.is_empty() {
                continue;
            }
            sources.insert(
                clip.id.clone(),
                TrackerSource {
                    timeline_start_ns: clip.timeline_start,
                    trackers_by_id: clip
                        .motion_trackers
                        .iter()
                        .cloned()
                        .map(|tracker| (tracker.id.clone(), tracker))
                        .collect(),
                },
            );
        }
    }
    sources
}

fn binding_source<'a>(
    binding: &TrackingBinding,
    sources: &'a HashMap<String, TrackerSource>,
) -> Option<(&'a TrackerSource, &'a MotionTracker)> {
    let source = sources.get(&binding.source_clip_id)?;
    let tracker = source.trackers_by_id.get(&binding.tracker_id)?;
    tracker.enabled.then_some((source, tracker))
}

fn binding_strength(binding: &TrackingBinding) -> f64 {
    if binding.strength.is_finite() {
        binding.strength.clamp(0.0, 1.0)
    } else {
        1.0
    }
}

fn collect_target_local_times(
    target_timeline_start_ns: u64,
    target_duration_ns: u64,
    source: &TrackerSource,
    tracker: &MotionTracker,
    existing_times: impl IntoIterator<Item = u64>,
) -> Vec<u64> {
    let target_end_ns = target_timeline_start_ns.saturating_add(target_duration_ns);
    let mut times = std::collections::BTreeSet::new();
    times.insert(0);
    times.insert(target_duration_ns);
    for time_ns in existing_times {
        times.insert(time_ns.min(target_duration_ns));
    }
    for sample in &tracker.samples {
        let global_time_ns = source.timeline_start_ns.saturating_add(sample.time_ns);
        if global_time_ns <= target_timeline_start_ns || global_time_ns >= target_end_ns {
            continue;
        }
        times.insert(global_time_ns.saturating_sub(target_timeline_start_ns));
    }
    times.into_iter().collect()
}

fn tracking_sample_for_target_local_ns(
    target_timeline_start_ns: u64,
    source: &TrackerSource,
    tracker: &MotionTracker,
    local_time_ns: u64,
) -> Option<TrackingSample> {
    let global_time_ns = target_timeline_start_ns.saturating_add(local_time_ns);
    let source_local_ns = global_time_ns.saturating_sub(source.timeline_start_ns);
    tracker.sample_at_local_ns(source_local_ns)
}

fn make_linear_keyframes<F>(times: &[u64], mut value_at: F) -> Vec<NumericKeyframe>
where
    F: FnMut(u64) -> f64,
{
    times
        .iter()
        .copied()
        .map(|time_ns| NumericKeyframe {
            time_ns,
            value: value_at(time_ns),
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        })
        .collect()
}

fn resolve_translation_keyframes(
    target_timeline_start_ns: u64,
    target_duration_ns: u64,
    default_x: f64,
    keyframes_x: &[NumericKeyframe],
    default_y: f64,
    keyframes_y: &[NumericKeyframe],
    binding: &TrackingBinding,
    source: &TrackerSource,
    tracker: &MotionTracker,
) -> Option<(Vec<NumericKeyframe>, Vec<NumericKeyframe>)> {
    if !binding.apply_translation
        || (tracker.samples.is_empty()
            && binding.offset_x.abs() < f64::EPSILON
            && binding.offset_y.abs() < f64::EPSILON)
    {
        return None;
    }
    let strength = binding_strength(binding);
    let times = collect_target_local_times(
        target_timeline_start_ns,
        target_duration_ns,
        source,
        tracker,
        keyframes_x
            .iter()
            .chain(keyframes_y.iter())
            .map(|keyframe| keyframe.time_ns),
    );
    let position_x_keyframes = make_linear_keyframes(&times, |time_ns| {
        let base = Clip::evaluate_keyframed_value(keyframes_x, time_ns, default_x);
        let tracked =
            tracking_sample_for_target_local_ns(target_timeline_start_ns, source, tracker, time_ns)
                .map(|sample| sample.offset_x * strength)
                .unwrap_or(0.0);
        (base + tracked + binding.offset_x).clamp(-1.0, 1.0)
    });
    let position_y_keyframes = make_linear_keyframes(&times, |time_ns| {
        let base = Clip::evaluate_keyframed_value(keyframes_y, time_ns, default_y);
        let tracked =
            tracking_sample_for_target_local_ns(target_timeline_start_ns, source, tracker, time_ns)
                .map(|sample| sample.offset_y * strength)
                .unwrap_or(0.0);
        (base + tracked + binding.offset_y).clamp(-1.0, 1.0)
    });
    Some((position_x_keyframes, position_y_keyframes))
}

fn resolve_scale_keyframes(
    target_timeline_start_ns: u64,
    target_duration_ns: u64,
    default_scale: f64,
    scale_keyframes: &[NumericKeyframe],
    binding: &TrackingBinding,
    source: &TrackerSource,
    tracker: &MotionTracker,
    min_value: f64,
    max_value: f64,
) -> Option<Vec<NumericKeyframe>> {
    if !binding.apply_scale
        || (tracker.samples.is_empty() && (binding.scale_multiplier - 1.0).abs() < f64::EPSILON)
    {
        return None;
    }
    let strength = binding_strength(binding);
    let times = collect_target_local_times(
        target_timeline_start_ns,
        target_duration_ns,
        source,
        tracker,
        scale_keyframes.iter().map(|keyframe| keyframe.time_ns),
    );
    Some(make_linear_keyframes(&times, |time_ns| {
        let base = Clip::evaluate_keyframed_value(scale_keyframes, time_ns, default_scale);
        let tracked =
            tracking_sample_for_target_local_ns(target_timeline_start_ns, source, tracker, time_ns)
                .map(|sample| 1.0 + (sample.scale_multiplier - 1.0) * strength)
                .unwrap_or(1.0);
        (base * tracked * binding.scale_multiplier).clamp(min_value, max_value)
    }))
}

fn resolve_rotation_keyframes(
    target_timeline_start_ns: u64,
    target_duration_ns: u64,
    default_rotation: f64,
    rotation_keyframes: &[NumericKeyframe],
    binding: &TrackingBinding,
    source: &TrackerSource,
    tracker: &MotionTracker,
) -> Option<Vec<NumericKeyframe>> {
    if !binding.apply_rotation
        || (tracker.samples.is_empty() && binding.rotation_offset_deg.abs() < f64::EPSILON)
    {
        return None;
    }
    let strength = binding_strength(binding);
    let times = collect_target_local_times(
        target_timeline_start_ns,
        target_duration_ns,
        source,
        tracker,
        rotation_keyframes.iter().map(|keyframe| keyframe.time_ns),
    );
    Some(make_linear_keyframes(&times, |time_ns| {
        let base = Clip::evaluate_keyframed_value(rotation_keyframes, time_ns, default_rotation);
        let tracked =
            tracking_sample_for_target_local_ns(target_timeline_start_ns, source, tracker, time_ns)
                .map(|sample| sample.rotation_deg * strength)
                .unwrap_or(0.0);
        (base + tracked + binding.rotation_offset_deg).clamp(-180.0, 180.0)
    }))
}

fn apply_tracking_binding_to_clip(
    clip: &mut Clip,
    binding: &TrackingBinding,
    source: &TrackerSource,
    tracker: &MotionTracker,
) {
    let clip_duration_ns = clip.duration();
    if let Some((position_x_keyframes, position_y_keyframes)) = resolve_translation_keyframes(
        clip.timeline_start,
        clip_duration_ns,
        clip.position_x,
        &clip.position_x_keyframes,
        clip.position_y,
        &clip.position_y_keyframes,
        binding,
        source,
        tracker,
    ) {
        clip.position_x_keyframes = position_x_keyframes;
        clip.position_y_keyframes = position_y_keyframes;
    }
    if let Some(scale_keyframes) = resolve_scale_keyframes(
        clip.timeline_start,
        clip_duration_ns,
        clip.scale,
        &clip.scale_keyframes,
        binding,
        source,
        tracker,
        0.1,
        4.0,
    ) {
        clip.scale_keyframes = scale_keyframes;
    }
    if let Some(rotate_keyframes) = resolve_rotation_keyframes(
        clip.timeline_start,
        clip_duration_ns,
        clip.rotate as f64,
        &clip.rotate_keyframes,
        binding,
        source,
        tracker,
    ) {
        clip.rotate_keyframes = rotate_keyframes;
    }
}

fn apply_tracking_binding_to_program_clip(
    clip: &mut ProgramClip,
    binding: &TrackingBinding,
    source: &TrackerSource,
    tracker: &MotionTracker,
) {
    let clip_duration_ns = clip.source_duration_ns();
    if let Some((position_x_keyframes, position_y_keyframes)) = resolve_translation_keyframes(
        clip.timeline_start_ns,
        clip_duration_ns,
        clip.position_x,
        &clip.position_x_keyframes,
        clip.position_y,
        &clip.position_y_keyframes,
        binding,
        source,
        tracker,
    ) {
        clip.position_x_keyframes = position_x_keyframes;
        clip.position_y_keyframes = position_y_keyframes;
    }
    if let Some(scale_keyframes) = resolve_scale_keyframes(
        clip.timeline_start_ns,
        clip_duration_ns,
        clip.scale,
        &clip.scale_keyframes,
        binding,
        source,
        tracker,
        0.1,
        4.0,
    ) {
        clip.scale_keyframes = scale_keyframes;
    }
    if let Some(rotate_keyframes) = resolve_rotation_keyframes(
        clip.timeline_start_ns,
        clip_duration_ns,
        clip.rotate as f64,
        &clip.rotate_keyframes,
        binding,
        source,
        tracker,
    ) {
        clip.rotate_keyframes = rotate_keyframes;
    }
}

fn apply_tracking_binding_to_mask(
    clip_timeline_start_ns: u64,
    clip_duration_ns: u64,
    mask: &mut ClipMask,
    binding: &TrackingBinding,
    source: &TrackerSource,
    tracker: &MotionTracker,
) {
    if mask.shape == crate::model::clip::MaskShape::Path {
        return;
    }
    if let Some((center_x_keyframes, center_y_keyframes)) = resolve_translation_keyframes(
        clip_timeline_start_ns,
        clip_duration_ns,
        mask.center_x,
        &mask.center_x_keyframes,
        mask.center_y,
        &mask.center_y_keyframes,
        binding,
        source,
        tracker,
    ) {
        mask.center_x_keyframes = center_x_keyframes;
        mask.center_y_keyframes = center_y_keyframes;
    }
    if let Some(width_keyframes) = resolve_scale_keyframes(
        clip_timeline_start_ns,
        clip_duration_ns,
        mask.width,
        &mask.width_keyframes,
        binding,
        source,
        tracker,
        0.01,
        0.5,
    ) {
        mask.width_keyframes = width_keyframes;
    }
    if let Some(height_keyframes) = resolve_scale_keyframes(
        clip_timeline_start_ns,
        clip_duration_ns,
        mask.height,
        &mask.height_keyframes,
        binding,
        source,
        tracker,
        0.01,
        0.5,
    ) {
        mask.height_keyframes = height_keyframes;
    }
    if let Some(rotation_keyframes) = resolve_rotation_keyframes(
        clip_timeline_start_ns,
        clip_duration_ns,
        mask.rotation,
        &mask.rotation_keyframes,
        binding,
        source,
        tracker,
    ) {
        mask.rotation_keyframes = rotation_keyframes;
    }
}

pub fn apply_tracking_bindings_to_program_clips(
    clips: &mut [ProgramClip],
    source_tracks: &[Track],
) {
    let flattened_tracks = crate::media::export::flatten_compound_tracks(source_tracks);
    let sources = collect_tracker_sources_from_flat_tracks(&flattened_tracks);
    for clip in clips {
        if let Some(binding) = clip.tracking_binding.clone() {
            if let Some((source, tracker)) = binding_source(&binding, &sources) {
                apply_tracking_binding_to_program_clip(clip, &binding, source, tracker);
            }
        }
        let clip_duration_ns = clip.source_duration_ns();
        if let Some(mask) = clip.masks.first_mut() {
            if let Some(binding) = mask.tracking_binding.clone() {
                if let Some((source, tracker)) = binding_source(&binding, &sources) {
                    apply_tracking_binding_to_mask(
                        clip.timeline_start_ns,
                        clip_duration_ns,
                        mask,
                        &binding,
                        source,
                        tracker,
                    );
                }
            }
        }
    }
}

pub fn apply_tracking_bindings_to_tracks(tracks: &mut [Track]) {
    let sources = collect_tracker_sources_from_flat_tracks(tracks);
    for track in tracks {
        for clip in &mut track.clips {
            if let Some(binding) = clip.tracking_binding.clone() {
                if let Some((source, tracker)) = binding_source(&binding, &sources) {
                    apply_tracking_binding_to_clip(clip, &binding, source, tracker);
                }
            }
            let clip_timeline_start_ns = clip.timeline_start;
            let clip_duration_ns = clip.duration();
            if let Some(mask) = clip.masks.first_mut() {
                if let Some(binding) = mask.tracking_binding.clone() {
                    if let Some((source, tracker)) = binding_source(&binding, &sources) {
                        apply_tracking_binding_to_mask(
                            clip_timeline_start_ns,
                            clip_duration_ns,
                            mask,
                            &binding,
                            source,
                            tracker,
                        );
                    }
                }
            }
        }
    }
}

pub struct TrackingCache {
    results: HashMap<String, CachedTrackingAnalysis>,
    pending: HashSet<String>,
    failed: HashSet<String>,
    progress_by_key: HashMap<String, TrackingJobProgress>,
    cancel_flags: HashMap<String, Arc<AtomicBool>>,
    total_requested: usize,
    result_rx: mpsc::Receiver<WorkerUpdate>,
    work_tx: Option<mpsc::Sender<TrackingWorkerJob>>,
    cache_root: PathBuf,
    pub last_error: Option<String>,
}

impl TrackingCache {
    pub fn new() -> Self {
        let (result_tx, result_rx) = mpsc::sync_channel::<WorkerUpdate>(64);
        let (work_tx, work_rx) = mpsc::channel::<TrackingWorkerJob>();
        let work_rx = Arc::new(std::sync::Mutex::new(work_rx));
        let cache_root = tracking_cache_root();
        let _ = std::fs::create_dir_all(&cache_root);

        let worker_count = 2;
        for _ in 0..worker_count {
            let rx = work_rx.clone();
            let tx = result_tx.clone();
            std::thread::spawn(move || loop {
                let item = {
                    let lock = rx.lock().unwrap();
                    lock.recv()
                };
                let worker_job = match item {
                    Ok(job) => job,
                    Err(_) => break,
                };
                let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_tracking_job(&worker_job, &tx)
                })) {
                    Ok(result) => result,
                    Err(panic) => WorkerResult {
                        cache_key: worker_job.cache_key.clone(),
                        job: worker_job.job.clone(),
                        analysis: None,
                        canceled: false,
                        error: Some(format!("Tracking worker panic: {}", panic_message(&panic))),
                    },
                };
                if tx.send(WorkerUpdate::Done(result)).is_err() {
                    break;
                }
            });
        }

        Self {
            results: HashMap::new(),
            pending: HashSet::new(),
            failed: HashSet::new(),
            progress_by_key: HashMap::new(),
            cancel_flags: HashMap::new(),
            total_requested: 0,
            result_rx,
            work_tx: Some(work_tx),
            cache_root,
            last_error: None,
        }
    }

    pub fn request(&mut self, job: TrackingJob) -> String {
        let cache_key = job.cache_key();
        if self.results.contains_key(&cache_key)
            || self.pending.contains(&cache_key)
            || self.failed.contains(&cache_key)
        {
            return cache_key;
        }

        let cache_path = self.cache_path_for_key(&cache_key);
        if let Some(cached) = read_cached_analysis(&cache_path) {
            self.results.insert(cache_key.clone(), cached);
            return cache_key;
        }

        self.last_error = None;
        self.total_requested += 1;
        self.pending.insert(cache_key.clone());
        self.progress_by_key.insert(
            cache_key.clone(),
            TrackingJobProgress {
                processed_samples: 0,
                total_samples: job.estimated_sample_count(),
            },
        );
        let cancel_flag = Arc::new(AtomicBool::new(false));
        self.cancel_flags
            .insert(cache_key.clone(), cancel_flag.clone());
        if let Some(ref tx) = self.work_tx {
            let _ = tx.send(TrackingWorkerJob {
                cache_key: cache_key.clone(),
                job,
                cancel_flag,
                cache_path,
            });
        }
        cache_key
    }

    pub fn cancel(&mut self, cache_key: &str) -> bool {
        if let Some(flag) = self.cancel_flags.get(cache_key) {
            flag.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    pub fn poll(&mut self) -> Vec<TrackingPollResult> {
        let mut resolved = Vec::new();
        while let Ok(update) = self.result_rx.try_recv() {
            match update {
                WorkerUpdate::Progress {
                    cache_key,
                    processed_samples,
                    total_samples,
                } => {
                    self.progress_by_key.insert(
                        cache_key,
                        TrackingJobProgress {
                            processed_samples,
                            total_samples,
                        },
                    );
                }
                WorkerUpdate::Done(result) => {
                    self.pending.remove(&result.cache_key);
                    self.progress_by_key.remove(&result.cache_key);
                    self.cancel_flags.remove(&result.cache_key);
                    if let Some(analysis) = result.analysis {
                        self.results
                            .insert(result.cache_key.clone(), analysis.clone());
                        resolved.push(TrackingPollResult {
                            cache_key: result.cache_key,
                            source_path: result.job.source_path.clone(),
                            tracker: Some(analysis.into_motion_tracker(
                                &result.job.tracker_id,
                                &result.job.tracker_label,
                            )),
                            canceled: false,
                            error: None,
                        });
                    } else if result.canceled {
                        resolved.push(TrackingPollResult {
                            cache_key: result.cache_key,
                            source_path: result.job.source_path.clone(),
                            tracker: None,
                            canceled: true,
                            error: None,
                        });
                    } else {
                        self.failed.insert(result.cache_key.clone());
                        self.last_error = result.error.clone();
                        resolved.push(TrackingPollResult {
                            cache_key: result.cache_key,
                            source_path: result.job.source_path.clone(),
                            tracker: None,
                            canceled: false,
                            error: result.error,
                        });
                    }
                }
            }
        }
        resolved
    }

    pub fn get_for_job(&self, job: &TrackingJob) -> Option<MotionTracker> {
        let cache_key = job.cache_key();
        self.results
            .get(&cache_key)
            .map(|analysis| analysis.into_motion_tracker(&job.tracker_id, &job.tracker_label))
    }

    pub fn job_progress(&self, cache_key: &str) -> Option<TrackingJobProgress> {
        self.progress_by_key.get(cache_key).cloned()
    }

    pub fn progress(&self) -> TrackingProgress {
        let processed_samples = self
            .progress_by_key
            .values()
            .map(|progress| progress.processed_samples)
            .sum::<usize>();
        let total_samples = self
            .progress_by_key
            .values()
            .map(|progress| progress.total_samples)
            .sum::<usize>();
        TrackingProgress {
            total: self.total_requested,
            completed: self.results.len(),
            in_flight: !self.pending.is_empty(),
            sample_fraction: (total_samples > 0)
                .then_some(processed_samples as f64 / total_samples as f64),
        }
    }

    pub fn invalidate(&mut self, cache_key: &str) {
        self.results.remove(cache_key);
        self.failed.remove(cache_key);
        self.progress_by_key.remove(cache_key);
        let _ = std::fs::remove_file(self.cache_path_for_key(cache_key));
    }

    pub fn invalidate_all(&mut self) {
        self.results.clear();
        self.pending.clear();
        self.failed.clear();
        self.progress_by_key.clear();
        self.cancel_flags.clear();
        self.last_error = None;
        if let Ok(entries) = std::fs::read_dir(&self.cache_root) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    fn cache_path_for_key(&self, cache_key: &str) -> PathBuf {
        self.cache_root.join(format!("{cache_key}.json"))
    }
}

impl Drop for TrackingCache {
    fn drop(&mut self) {
        self.work_tx.take();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PixelRect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl PixelRect {
    fn center_x(self) -> f64 {
        self.x as f64 + self.width as f64 / 2.0
    }

    fn center_y(self) -> f64 {
        self.y as f64 + self.height as f64 / 2.0
    }
}

#[derive(Debug, Clone)]
struct TemplatePatch {
    pixels: Vec<u8>,
    mean: f64,
}

#[derive(Debug, Clone)]
struct GrayFrameSequence {
    width: usize,
    height: usize,
    frames: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackingFailure {
    Cancelled,
    Failed,
}

fn run_tracking_job(
    worker_job: &TrackingWorkerJob,
    progress_tx: &mpsc::SyncSender<WorkerUpdate>,
) -> WorkerResult {
    match analyze_tracking_job(worker_job, progress_tx) {
        Ok(analysis) => WorkerResult {
            cache_key: worker_job.cache_key.clone(),
            job: worker_job.job.clone(),
            analysis: Some(analysis),
            canceled: false,
            error: None,
        },
        Err((TrackingFailure::Cancelled, _)) => WorkerResult {
            cache_key: worker_job.cache_key.clone(),
            job: worker_job.job.clone(),
            analysis: None,
            canceled: true,
            error: None,
        },
        Err((TrackingFailure::Failed, error)) => WorkerResult {
            cache_key: worker_job.cache_key.clone(),
            job: worker_job.job.clone(),
            analysis: None,
            canceled: false,
            error: Some(error),
        },
    }
}

fn analyze_tracking_job(
    worker_job: &TrackingWorkerJob,
    progress_tx: &mpsc::SyncSender<WorkerUpdate>,
) -> Result<CachedTrackingAnalysis, (TrackingFailure, String)> {
    let job = &worker_job.job;
    if job.source_path.trim().is_empty() {
        return Err((
            TrackingFailure::Failed,
            "Tracking source_path is empty".to_string(),
        ));
    }
    if job.analysis_end_ns <= job.analysis_start_ns {
        return Err((
            TrackingFailure::Failed,
            "Tracking analysis_end_ns must be greater than analysis_start_ns".to_string(),
        ));
    }

    let metadata = crate::media::probe_cache::probe_media_metadata(&job.source_path);
    if metadata.is_image || metadata.is_audio_only {
        return Err((
            TrackingFailure::Failed,
            "Motion tracking currently requires a video clip with decodable frames".to_string(),
        ));
    }

    let frames = extract_gray_frames(job, &worker_job.cancel_flag)?;
    let analysis =
        track_motion_in_frames(&frames, job, &worker_job.cancel_flag, |processed, total| {
            let _ = progress_tx.send(WorkerUpdate::Progress {
                cache_key: worker_job.cache_key.clone(),
                processed_samples: processed,
                total_samples: total,
            });
        })?;
    if let Err(error) = write_cached_analysis(&worker_job.cache_path, &analysis) {
        log::warn!(
            "TrackingCache: failed to write cache {}: {}",
            worker_job.cache_path.display(),
            error
        );
    }
    Ok(analysis)
}

fn track_motion_in_frames<F>(
    sequence: &GrayFrameSequence,
    job: &TrackingJob,
    cancel_flag: &AtomicBool,
    mut report_progress: F,
) -> Result<CachedTrackingAnalysis, (TrackingFailure, String)>
where
    F: FnMut(usize, usize),
{
    if cancel_flag.load(Ordering::Relaxed) {
        return Err((TrackingFailure::Cancelled, "Tracking canceled".to_string()));
    }
    if sequence.frames.is_empty() {
        return Err((
            TrackingFailure::Failed,
            "No video frames extracted for tracking".to_string(),
        ));
    }

    let initial_rect = region_to_rect(job.analysis_region, sequence.width, sequence.height);
    let mut current_rect = initial_rect;
    let mut template = extract_template(&sequence.frames[0], sequence.width, current_rect)?;
    let effective_step_ns = job.effective_frame_step_ns();
    let total_frames = sequence.frames.len();
    let mut samples = Vec::with_capacity(total_frames);
    samples.push(TrackingSample::identity(job.analysis_start_ns));
    report_progress(1, total_frames);

    for (frame_index, frame) in sequence.frames.iter().enumerate().skip(1) {
        if cancel_flag.load(Ordering::Relaxed) {
            return Err((TrackingFailure::Cancelled, "Tracking canceled".to_string()));
        }
        let (matched_rect, confidence) = find_best_match(
            frame,
            sequence.width,
            sequence.height,
            &template,
            current_rect,
            job.effective_search_radius_px() as i32,
        )?;
        let sample_time = job
            .analysis_start_ns
            .saturating_add((frame_index as u64).saturating_mul(effective_step_ns))
            .min(job.analysis_end_ns);
        samples.push(TrackingSample {
            time_ns: sample_time,
            offset_x: (matched_rect.center_x() - initial_rect.center_x()) / sequence.width as f64,
            offset_y: (matched_rect.center_y() - initial_rect.center_y()) / sequence.height as f64,
            scale_multiplier: 1.0,
            rotation_deg: 0.0,
            confidence,
        });
        current_rect = matched_rect;
        template = extract_template(frame, sequence.width, current_rect)?;
        report_progress(frame_index + 1, total_frames);
    }

    Ok(CachedTrackingAnalysis {
        analysis_region: job.analysis_region,
        analysis_start_ns: job.analysis_start_ns,
        analysis_end_ns: Some(job.analysis_end_ns),
        samples,
    })
}

fn extract_gray_frames(
    job: &TrackingJob,
    cancel_flag: &AtomicBool,
) -> Result<GrayFrameSequence, (TrackingFailure, String)> {
    let start_ns = job.clip_source_in_ns.saturating_add(job.analysis_start_ns);
    let duration_ns = job.analysis_duration_ns();
    let fps = 1_000_000_000.0 / job.effective_frame_step_ns() as f64;
    let expected_frames = job.estimated_sample_count().max(1);
    let temp = NamedTempFile::new().map_err(|e| {
        (
            TrackingFailure::Failed,
            format!("Failed to create temporary tracking buffer: {e}"),
        )
    })?;
    let raw_path = temp.path().to_path_buf();
    let raw_path_str = raw_path.to_string_lossy().to_string();
    let filter =
        format!("fps={fps:.6},scale={ANALYSIS_WIDTH}:{ANALYSIS_HEIGHT}:flags=bilinear,format=gray");
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-ss",
            &format_time_arg(start_ns),
            "-t",
            &format_time_arg(duration_ns),
            "-i",
            &job.source_path,
            "-an",
            "-sn",
            "-dn",
            "-vf",
            &filter,
            "-frames:v",
            &expected_frames.to_string(),
            "-pix_fmt",
            "gray",
            "-f",
            "rawvideo",
            &raw_path_str,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            (
                TrackingFailure::Failed,
                format!("Failed to start ffmpeg for tracking: {e}"),
            )
        })?;

    let status = loop {
        if cancel_flag.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err((TrackingFailure::Cancelled, "Tracking canceled".to_string()));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err((
                    TrackingFailure::Failed,
                    format!("Failed waiting for ffmpeg tracking decode: {e}"),
                ));
            }
        }
    };
    if !status.success() {
        return Err((
            TrackingFailure::Failed,
            "ffmpeg tracking decode failed".to_string(),
        ));
    }

    let bytes = std::fs::read(&raw_path).map_err(|e| {
        (
            TrackingFailure::Failed,
            format!("Failed reading temporary tracking frames: {e}"),
        )
    })?;
    let frame_size = ANALYSIS_WIDTH * ANALYSIS_HEIGHT;
    if bytes.len() < frame_size {
        return Err((
            TrackingFailure::Failed,
            "Tracking decode produced no frames".to_string(),
        ));
    }
    let frames = bytes
        .chunks_exact(frame_size)
        .map(|chunk| chunk.to_vec())
        .collect::<Vec<_>>();
    if frames.is_empty() {
        return Err((
            TrackingFailure::Failed,
            "Tracking decode produced no complete frames".to_string(),
        ));
    }
    Ok(GrayFrameSequence {
        width: ANALYSIS_WIDTH,
        height: ANALYSIS_HEIGHT,
        frames,
    })
}

fn region_to_rect(region: TrackingRegion, image_width: usize, image_height: usize) -> PixelRect {
    let width = ((region.width * image_width as f64 * 2.0).round() as i32)
        .max(MIN_TEMPLATE_HALF_SIZE_PX * 2)
        .min(image_width as i32);
    let height = ((region.height * image_height as f64 * 2.0).round() as i32)
        .max(MIN_TEMPLATE_HALF_SIZE_PX * 2)
        .min(image_height as i32);
    let center_x = (region.center_x * image_width as f64).round() as i32;
    let center_y = (region.center_y * image_height as f64).round() as i32;
    let max_x = image_width as i32 - width;
    let max_y = image_height as i32 - height;
    PixelRect {
        x: (center_x - width / 2).clamp(0, max_x.max(0)),
        y: (center_y - height / 2).clamp(0, max_y.max(0)),
        width,
        height,
    }
}

fn extract_template(
    frame: &[u8],
    image_width: usize,
    rect: PixelRect,
) -> Result<TemplatePatch, (TrackingFailure, String)> {
    if rect.width <= 0 || rect.height <= 0 {
        return Err((
            TrackingFailure::Failed,
            "Tracking template rect is empty".to_string(),
        ));
    }
    let mut pixels = Vec::with_capacity((rect.width * rect.height) as usize);
    for y in rect.y..rect.y + rect.height {
        let start = y as usize * image_width + rect.x as usize;
        let end = start + rect.width as usize;
        pixels.extend_from_slice(&frame[start..end]);
    }
    let mean = patch_mean(&pixels);
    Ok(TemplatePatch { pixels, mean })
}

fn find_best_match(
    frame: &[u8],
    image_width: usize,
    image_height: usize,
    template: &TemplatePatch,
    current_rect: PixelRect,
    search_radius_px: i32,
) -> Result<(PixelRect, f64), (TrackingFailure, String)> {
    let mut best_rect = current_rect;
    let mut best_score = u64::MAX;
    let mut second_best = u64::MAX;
    for dy in -search_radius_px..=search_radius_px {
        for dx in -search_radius_px..=search_radius_px {
            let candidate = PixelRect {
                x: current_rect.x + dx,
                y: current_rect.y + dy,
                width: current_rect.width,
                height: current_rect.height,
            };
            if candidate.x < 0
                || candidate.y < 0
                || candidate.x + candidate.width > image_width as i32
                || candidate.y + candidate.height > image_height as i32
            {
                continue;
            }
            let score = centered_sad(frame, image_width, template, candidate);
            if score < best_score {
                second_best = best_score;
                best_score = score;
                best_rect = candidate;
            } else if score < second_best {
                second_best = score;
            }
        }
    }
    if best_score == u64::MAX {
        return Err((
            TrackingFailure::Failed,
            "Tracking search failed to find a valid candidate window".to_string(),
        ));
    }
    Ok((
        best_rect,
        tracking_confidence(best_score, second_best, template.pixels.len()),
    ))
}

fn centered_sad(
    frame: &[u8],
    image_width: usize,
    template: &TemplatePatch,
    candidate: PixelRect,
) -> u64 {
    let mut candidate_sum = 0u64;
    for y in candidate.y..candidate.y + candidate.height {
        let start = y as usize * image_width + candidate.x as usize;
        let end = start + candidate.width as usize;
        candidate_sum += frame[start..end]
            .iter()
            .map(|value| *value as u64)
            .sum::<u64>();
    }
    let candidate_mean = candidate_sum as f64 / template.pixels.len() as f64;

    let mut score = 0u64;
    let mut template_index = 0usize;
    for y in candidate.y..candidate.y + candidate.height {
        let start = y as usize * image_width + candidate.x as usize;
        let end = start + candidate.width as usize;
        for pixel in &frame[start..end] {
            let template_centered = template.pixels[template_index] as f64 - template.mean;
            let candidate_centered = *pixel as f64 - candidate_mean;
            score += (template_centered - candidate_centered).abs() as u64;
            template_index += 1;
        }
    }
    score
}

fn tracking_confidence(best_score: u64, second_best: u64, patch_len: usize) -> f64 {
    let max_score = (patch_len as f64 * 255.0).max(1.0);
    let error_component = 1.0 - (best_score as f64 / max_score).clamp(0.0, 1.0);
    let margin_component = if second_best == u64::MAX {
        1.0
    } else {
        ((second_best.saturating_sub(best_score)) as f64 / second_best.max(1) as f64)
            .clamp(0.0, 1.0)
    };
    (error_component * 0.7 + margin_component * 0.3).clamp(0.0, 1.0)
}

fn patch_mean(pixels: &[u8]) -> f64 {
    if pixels.is_empty() {
        0.0
    } else {
        pixels.iter().map(|value| *value as f64).sum::<f64>() / pixels.len() as f64
    }
}

fn quantize_norm(value: f64) -> i64 {
    (value * 10_000.0).round() as i64
}

fn format_time_arg(time_ns: u64) -> String {
    format!("{:.6}", time_ns as f64 / 1_000_000_000.0)
}

fn tracking_cache_root() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(home).join(".cache")
        });
    base.join("ultimateslice").join("tracking")
}

fn read_cached_analysis(path: &Path) -> Option<CachedTrackingAnalysis> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<CachedTrackingAnalysis>(&bytes).ok()
}

fn write_cached_analysis(path: &Path, analysis: &CachedTrackingAnalysis) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(analysis).map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_synthetic_frame(width: usize, height: usize, x: usize, y: usize) -> Vec<u8> {
        let mut frame = vec![20u8; width * height];
        for row in 0..height {
            for col in 0..width {
                frame[row * width + col] =
                    frame[row * width + col].saturating_add(((row + col) % 5) as u8);
            }
        }
        for row in y..(y + 8).min(height) {
            for col in x..(x + 8).min(width) {
                frame[row * width + col] = 230;
            }
        }
        frame
    }

    #[test]
    fn tracking_job_cache_key_changes_with_region() {
        let mut a = TrackingJob::new(
            "tracker-a",
            "Face",
            "/tmp/source.mp4",
            0,
            0,
            1_000_000_000,
            TrackingRegion::default(),
        );
        let mut b = a.clone();
        b.analysis_region.center_x = 0.65;
        a.frame_step_ns = 200_000_000;
        b.frame_step_ns = 200_000_000;
        assert_ne!(a.cache_key(), b.cache_key());
    }

    #[test]
    fn track_motion_sequence_detects_translation() {
        let width = 64;
        let height = 40;
        let frames = (0..5)
            .map(|idx| make_synthetic_frame(width, height, 18 + idx * 2, 10 + idx))
            .collect::<Vec<_>>();
        let sequence = GrayFrameSequence {
            width,
            height,
            frames,
        };
        let mut job = TrackingJob::new(
            "tracker-1",
            "Subject",
            "/tmp/source.mp4",
            0,
            0,
            500_000_000,
            TrackingRegion {
                center_x: 22.0 / width as f64,
                center_y: 14.0 / height as f64,
                width: 4.0 / width as f64,
                height: 4.0 / height as f64,
                rotation_deg: 0.0,
            },
        );
        job.frame_step_ns = 100_000_000;
        job.search_radius_px = 6;
        let cancel_flag = AtomicBool::new(false);
        let mut progress_updates = Vec::new();

        let analysis = track_motion_in_frames(&sequence, &job, &cancel_flag, |processed, total| {
            progress_updates.push((processed, total));
        })
        .expect("tracking should succeed");

        assert_eq!(analysis.samples.len(), 5);
        assert!((analysis.samples[1].offset_x - 2.0 / width as f64).abs() < 0.02);
        assert!((analysis.samples[2].offset_x - 4.0 / width as f64).abs() < 0.02);
        assert!((analysis.samples[3].offset_y - 3.0 / height as f64).abs() < 0.03);
        assert!(analysis
            .samples
            .iter()
            .all(|sample| sample.scale_multiplier == 1.0));
        assert_eq!(progress_updates.last().copied(), Some((5, 5)));
    }

    #[test]
    fn track_motion_sequence_honors_cancel_flag() {
        let sequence = GrayFrameSequence {
            width: 32,
            height: 18,
            frames: vec![make_synthetic_frame(32, 18, 8, 4)],
        };
        let job = TrackingJob::new(
            "tracker-1",
            "Subject",
            "/tmp/source.mp4",
            0,
            0,
            100_000_000,
            TrackingRegion::default(),
        );
        let cancel_flag = AtomicBool::new(true);
        let result = track_motion_in_frames(&sequence, &job, &cancel_flag, |_processed, _total| {});
        assert_eq!(
            result,
            Err((TrackingFailure::Cancelled, "Tracking canceled".to_string()))
        );
    }

    #[test]
    fn apply_tracking_bindings_to_tracks_projects_samples_onto_clip_and_mask() {
        let mut source = Clip::new(
            "/tmp/source.mp4",
            2_000_000_000,
            2_000_000_000,
            crate::model::clip::ClipKind::Video,
        );
        source.id = "source-clip".to_string();
        let mut tracker = MotionTracker::new("Subject");
        tracker.id = "tracker-1".to_string();
        tracker.samples = vec![
            TrackingSample::identity(0),
            TrackingSample {
                time_ns: 1_000_000_000,
                offset_x: 0.2,
                offset_y: -0.1,
                scale_multiplier: 1.0,
                rotation_deg: 0.0,
                confidence: 1.0,
            },
        ];
        source.motion_trackers.push(tracker);

        let mut target = Clip::new(
            "/tmp/overlay.png",
            2_000_000_000,
            2_500_000_000,
            crate::model::clip::ClipKind::Image,
        );
        target.id = "target-clip".to_string();
        target.tracking_binding = Some(TrackingBinding::new("source-clip", "tracker-1"));
        let mut mask = ClipMask::new(crate::model::clip::MaskShape::Rectangle);
        mask.tracking_binding = Some(TrackingBinding::new("source-clip", "tracker-1"));
        target.masks.push(mask);

        let mut track = Track::new_video("Video");
        track.clips = vec![source, target];

        apply_tracking_bindings_to_tracks(std::slice::from_mut(&mut track));

        let resolved = track
            .clips
            .iter()
            .find(|clip| clip.id == "target-clip")
            .expect("target clip should remain present");
        assert_eq!(
            resolved
                .position_x_keyframes
                .iter()
                .map(|keyframe| keyframe.time_ns)
                .collect::<Vec<_>>(),
            vec![0, 500_000_000, 2_000_000_000]
        );
        assert!((resolved.position_x_keyframes[0].value - 0.1).abs() < 1e-6);
        assert!((resolved.position_x_keyframes[1].value - 0.2).abs() < 1e-6);
        assert!((resolved.position_y_keyframes[0].value + 0.05).abs() < 1e-6);
        assert!((resolved.position_y_keyframes[1].value + 0.1).abs() < 1e-6);

        let resolved_mask = resolved.masks.first().expect("mask should still exist");
        assert_eq!(
            resolved_mask
                .center_x_keyframes
                .iter()
                .map(|keyframe| keyframe.time_ns)
                .collect::<Vec<_>>(),
            vec![0, 500_000_000, 2_000_000_000]
        );
        assert!((resolved_mask.center_x_keyframes[0].value - 0.6).abs() < 1e-6);
        assert!((resolved_mask.center_x_keyframes[1].value - 0.7).abs() < 1e-6);
        assert!((resolved_mask.center_y_keyframes[0].value - 0.45).abs() < 1e-6);
        assert!((resolved_mask.center_y_keyframes[1].value - 0.4).abs() < 1e-6);
    }

    #[test]
    fn cached_tracking_analysis_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("analysis.json");
        let analysis = CachedTrackingAnalysis {
            analysis_region: TrackingRegion::default(),
            analysis_start_ns: 0,
            analysis_end_ns: Some(500_000_000),
            samples: vec![
                TrackingSample::identity(0),
                TrackingSample {
                    time_ns: 100_000_000,
                    offset_x: 0.1,
                    offset_y: -0.05,
                    scale_multiplier: 1.0,
                    rotation_deg: 0.0,
                    confidence: 0.8,
                },
            ],
        };
        write_cached_analysis(&path, &analysis).expect("cache write should succeed");
        let restored = read_cached_analysis(&path).expect("cache read should succeed");
        assert_eq!(restored, analysis);
    }
}
