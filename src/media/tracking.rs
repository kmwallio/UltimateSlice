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

// CACHE_VERSION bumps:
//   2: gray 160×90 → YUV444 320×180 (color matching, no template drift)
//   3: + motion prediction, confidence recovery, anchor-on-loss
//   4: SAD → normalized cross-correlation (different sample confidences)
const CACHE_VERSION: u32 = 4;
const ANALYSIS_WIDTH: usize = 320;
const ANALYSIS_HEIGHT: usize = 180;
/// Sentinel step meaning "use the source clip's native frame rate".
/// Non-zero values in the job override this.
const DEFAULT_FRAME_STEP_NS: u64 = 0;
/// Fallback frame rate used when the source's true rate can't be
/// probed. Picked conservatively (i.e. low) so the search radius still
/// catches motion if we miscount frames.
const FALLBACK_SOURCE_FPS: f64 = 24.0;
/// Search radius at ANALYSIS_WIDTH=320 — 24 px ≈ 7.5% of frame width
/// per sample step. Motion prediction effectively extends this by the
/// current velocity so the *reachable* per-frame motion is
/// `radius + |velocity|`; the radius itself only has to cover
/// acceleration (change in velocity) between consecutive frames. 24
/// is a balance: big enough that the prediction doesn't have to be
/// perfect to catch the subject, small enough that the candidate loop
/// stays fast (49² ≈ 2400 candidates per frame).
const DEFAULT_SEARCH_RADIUS_PX: u32 = 24;
const MIN_TEMPLATE_HALF_SIZE_PX: i32 = 4;
/// Cap to keep memory + compute bounded. 1200 frames × 320×180 YUV444
/// ≈ 207 MB peak RAM for the decoded sequence, and the SAD matcher
/// completes in a few seconds on a modern CPU. Clips longer than
/// `MAX_TRACKING_FRAMES / source_fps` seconds get proportionally
/// decimated.
const MAX_TRACKING_FRAMES: usize = 1200;

/// Confidence at-or-below this triggers a recovery pass with a wider
/// search radius centered on the last known-good position.  The NCC
/// confidence distribution on real footage is surprisingly wide —
/// "typical good" matches hover around 0.4–0.6, "uncertain" matches
/// sit around 0.25–0.4, and "clearly lost" dips to 0.25 or below. We
/// fire recovery on any uncertain-or-worse frame so the common path
/// stays on the recovered result when the normal search drifts onto
/// background texture.
const RECOVERY_CONFIDENCE_THRESHOLD: f64 = 0.30;
/// Hard cap on how far the matched rect is allowed to move from the
/// *previous* matched rect in a single frame. At 320 analysis width,
/// 32 px = 10% of frame per frame, which is already past what real
/// subjects move at frame rate. Jumps larger than this are almost
/// always the matcher teleporting to a distant local minimum — we
/// clamp them to preserve trajectory continuity even if the score
/// wanted otherwise.
const MAX_SINGLE_FRAME_JUMP_PX: i32 = 32;
/// Recovery search radius, applied when a frame's normal match drops
/// below the confidence threshold.  At ANALYSIS_WIDTH=320 this is an
/// 80 px half-width = 25% of frame width, enough to catch a hand whip
/// that briefly leaves the normal search window during a full 1/24 s
/// frame.  Cost: 161² ≈ 26k candidates per recovery call, about 11×
/// the normal search, so the recovery threshold is tuned to keep this
/// out of the common path.
const RECOVERY_SEARCH_RADIUS_PX: i32 = 80;
/// How many consecutive low-confidence frames we'll tolerate while
/// still chasing motion before anchoring the search at the last
/// known-good position.  Set high because on handheld footage the
/// confidence can dip for ~1 s (24 frames) during a fast whip-pan and
/// then recover — we'd rather keep chasing than freeze during that
/// window.  Once we do anchor, the subject has to come back into the
/// last good window for the tracker to re-engage.
const MAX_CONSECUTIVE_LOW_CONFIDENCE_FRAMES: u32 = 48;

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

/// Resolve the native frame period of a source clip from its probed
/// metadata. Returns ns-per-frame, falling back to
/// `FALLBACK_SOURCE_FPS` when the source can't be probed or reports a
/// nonsensical rate.
///
/// This is how the tracker translates "sample every frame" into a
/// concrete step — callers probe the source once (synchronously) when
/// they enqueue a tracking job and use the result to populate
/// `TrackingJob::frame_step_ns`. Doing it at enqueue time (rather
/// than inside `analyze_tracking_job`) keeps `TrackingJob::cache_key`
/// deterministic: two jobs for the same source with the same region
/// produce the same key regardless of probe timing.
pub fn source_frame_step_ns(source_path: &str) -> u64 {
    let metadata = crate::media::probe_cache::probe_media_metadata(source_path);
    if let (Some(num), Some(den)) = (metadata.frame_rate_num, metadata.frame_rate_den) {
        if num > 0 && den > 0 {
            let num = num as u64;
            let den = den as u64;
            // frame_period_ns = den / num seconds → (den * 1e9) / num
            return (den.saturating_mul(1_000_000_000)) / num;
        }
    }
    (1_000_000_000.0 / FALLBACK_SOURCE_FPS) as u64
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
        // frame_step_ns == 0 means "unresolved" — the caller should
        // have populated this via `source_frame_step_ns` before
        // enqueuing the job. If it didn't, fall back to the default
        // fps so we still produce a usable step.
        let requested = if self.frame_step_ns == 0 {
            (1_000_000_000.0 / FALLBACK_SOURCE_FPS) as u64
        } else {
            self.frame_step_ns
        };
        let duration = self.analysis_duration_ns();
        if duration == 0 {
            return requested;
        }
        // Cap total samples to MAX_TRACKING_FRAMES. If the requested
        // step would overshoot, widen it to hit the cap exactly.
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
    // Position values past ±1.0 push the clip off-canvas; the rendering
    // pipeline (preview compositor + export ffmpeg graph) handles the
    // overflow by cropping/padding past the frame edges.  Use the shared
    // `POSITION_MIN`/`POSITION_MAX` bounds so attaching a tracker to a clip
    // that already has off-canvas position values doesn't silently snap it
    // back inside the canvas.
    use crate::model::transform_bounds::{POSITION_MAX, POSITION_MIN};
    let position_x_keyframes = make_linear_keyframes(&times, |time_ns| {
        let base = Clip::evaluate_keyframed_value(keyframes_x, time_ns, default_x);
        let tracked =
            tracking_sample_for_target_local_ns(target_timeline_start_ns, source, tracker, time_ns)
                .map(|sample| sample.offset_x * strength)
                .unwrap_or(0.0);
        (base + tracked + binding.offset_x).clamp(POSITION_MIN, POSITION_MAX)
    });
    let position_y_keyframes = make_linear_keyframes(&times, |time_ns| {
        let base = Clip::evaluate_keyframed_value(keyframes_y, time_ns, default_y);
        let tracked =
            tracking_sample_for_target_local_ns(target_timeline_start_ns, source, tracker, time_ns)
                .map(|sample| sample.offset_y * strength)
                .unwrap_or(0.0);
        (base + tracked + binding.offset_y).clamp(POSITION_MIN, POSITION_MAX)
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
        (base + tracked + binding.rotation_offset_deg).clamp(
            crate::model::transform_bounds::ROTATE_MIN_DEG,
            crate::model::transform_bounds::ROTATE_MAX_DEG,
        )
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
        crate::model::transform_bounds::SCALE_MIN,
        crate::model::transform_bounds::SCALE_MAX,
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
        crate::model::transform_bounds::SCALE_MIN,
        crate::model::transform_bounds::SCALE_MAX,
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

// ─── Auto-crop & track helper ──────────────────────────────────────────────

/// Inputs to [`compute_auto_crop_binding_values`].
///
/// All fields are caller-supplied — this helper is pure math so it can be
/// unit tested without constructing a full `Clip` or `Project`.
#[derive(Debug, Clone, Copy)]
pub struct AutoCropInputs {
    /// User-drawn tracking region in clip-local normalized coordinates
    /// (`center_x`/`center_y` in `[0, 1]`, `width`/`height` are *half-widths*
    /// in `[0, 0.5]`, matching [`TrackingRegion`]).
    pub region: TrackingRegion,
    /// Source clip dimensions in pixels.
    pub source_width: u32,
    pub source_height: u32,
    /// Project canvas dimensions in pixels.
    pub project_width: u32,
    pub project_height: u32,
    /// Extra headroom around the region as a fraction (e.g. `0.1` = 10%
    /// margin). Clamped to `[0, 0.5]`.
    pub padding: f64,
}

/// The derived transform values for auto-cropping to a tracked region.
///
/// These are plugged into a [`TrackingBinding`] (with `apply_translation`
/// + `apply_scale` set to `true`) so the existing tracker-resolution
/// pipeline converts them into `scale` + `position_x/y` keyframes on the
/// clip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoCropBindingValues {
    pub scale_multiplier: f64,
    pub offset_x: f64,
    pub offset_y: f64,
}

/// Compute a `(scale_multiplier, offset_x, offset_y)` triple that, when
/// applied via a [`TrackingBinding`] on the clip's own transform, centers
/// the user-drawn region in the project frame at the project's aspect
/// ratio. See `docs/user/inspector.md` for the user-facing description.
///
/// Derivation:
///
/// 1. **Letterbox fit.** At `clip.scale == 1.0` the source is fit into the
///    project frame preserving source aspect, so actual source content
///    occupies `content_w × content_h` project-pixels with
///    `content_w = min(project_w, project_h * src_aspect)` and symmetric
///    for height. `cw_frac = content_w / project_w`,
///    `ch_frac = content_h / project_h`.
///
/// 2. **Minimum scale to eliminate letterbox bars** (critical for the
///    cross-aspect reframe case — 16:9 → 9:16 vertical projects):
///    `s_fill = max(1/cw_frac, 1/ch_frac)`.
///
/// 3. **Tight zoom so the region fills the frame with `padding` margin**:
///    `s_tight = min(1/(2*rw*cw_frac*(1+p)), 1/(2*rh*ch_frac*(1+p)))`. The
///    smaller denominator wins so both region dimensions fit.
///
/// 4. Final scale = `max(s_fill, s_tight)` clamped to `[1.0, SCALE_MAX]`.
///    For same-aspect projects `s_fill == 1` and `s_tight` dominates
///    (classic tight-crop-to-region); for cross-aspect projects `s_fill`
///    typically dominates and the region is centered in the reframed crop.
///
/// 5. **Centering offsets** are derived from the direct-canvas-translation
///    overlay formula used by clips with a `tracking_binding`
///    (`(W*(1+pos_x) - w)/2`, see
///    `src/media/export.rs::direct_canvas_origin`). The algebra to place
///    the region's center at the project's center at the chosen scale
///    gives `offset_x = cw_frac * scale * (1 - 2*region.center_x)`.
pub fn compute_auto_crop_binding_values(inputs: &AutoCropInputs) -> AutoCropBindingValues {
    use crate::model::transform_bounds::{POSITION_MAX, POSITION_MIN, SCALE_MAX, SCALE_MIN};

    let source_width = inputs.source_width.max(1) as f64;
    let source_height = inputs.source_height.max(1) as f64;
    let project_width = inputs.project_width.max(1) as f64;
    let project_height = inputs.project_height.max(1) as f64;

    let src_aspect = source_width / source_height;
    let proj_aspect = project_width / project_height;
    // (src_aspect / proj_aspect) > 1 ⇔ source wider than project.
    let source_wider = src_aspect > proj_aspect;

    // Letterbox-fit source into project canvas.
    let (content_w, content_h) = if source_wider {
        // Source is wider than project: fit by width, bars on top/bottom.
        (project_width, project_width / src_aspect)
    } else {
        // Source is taller: fit by height, bars left/right.
        (project_height * src_aspect, project_height)
    };
    let cw_frac = (content_w / project_width).clamp(f64::EPSILON, 1.0);
    let ch_frac = (content_h / project_height).clamp(f64::EPSILON, 1.0);

    // Region half-widths clamped away from zero to avoid div-by-zero if a
    // user clicks a "region" without actually drawing one.
    let rw = inputs.region.width.clamp(0.01, 0.5);
    let rh = inputs.region.height.clamp(0.01, 0.5);
    let pad = inputs.padding.clamp(0.0, 0.5);

    // Minimum scale that eliminates the letterbox bars.
    let s_fill = (1.0 / cw_frac).max(1.0 / ch_frac);

    // Tight zoom so the region (full width = 2*rw) fills the project with
    // `padding` margin in the tighter dimension.
    let s_tight_w = 1.0 / (2.0 * rw * cw_frac * (1.0 + pad));
    let s_tight_h = 1.0 / (2.0 * rh * ch_frac * (1.0 + pad));
    let s_tight = s_tight_w.min(s_tight_h);

    // Combine: honour both "fill the frame" and "frame the region".
    // `.max(1.0)` forbids zooming out (auto-*crop* never letterboxes more
    // than the source already does).
    let scale = s_fill.max(s_tight).max(1.0).clamp(SCALE_MIN, SCALE_MAX);

    // Centering offsets in pos_x/y direct-canvas space.
    let offset_x =
        (cw_frac * scale * (1.0 - 2.0 * inputs.region.center_x)).clamp(POSITION_MIN, POSITION_MAX);
    let offset_y =
        (ch_frac * scale * (1.0 - 2.0 * inputs.region.center_y)).clamp(POSITION_MIN, POSITION_MAX);

    AutoCropBindingValues {
        scale_multiplier: scale,
        offset_x,
        offset_y,
    }
}

/// Convenience wrapper: compute the values and wrap them in a
/// [`TrackingBinding`] ready to assign to `clip.tracking_binding`.
pub fn compute_auto_crop_binding(
    source_clip_id: impl Into<String>,
    tracker_id: impl Into<String>,
    inputs: &AutoCropInputs,
) -> TrackingBinding {
    let values = compute_auto_crop_binding_values(inputs);
    TrackingBinding {
        source_clip_id: source_clip_id.into(),
        tracker_id: tracker_id.into(),
        apply_translation: true,
        apply_scale: true,
        apply_rotation: false,
        offset_x: values.offset_x,
        offset_y: values.offset_y,
        scale_multiplier: values.scale_multiplier,
        rotation_offset_deg: 0.0,
        strength: 1.0,
        smoothing: 0.0,
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
        if let Err(e) = std::fs::create_dir_all(&cache_root) {
            log::warn!(
                "tracking: failed to create cache dir {}: {e}",
                cache_root.display()
            );
        }

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
            if let Err(e) = tx.send(TrackingWorkerJob {
                cache_key: cache_key.clone(),
                job,
                cancel_flag,
                cache_path,
            }) {
                log::warn!(
                    "tracking: failed to enqueue work for {cache_key}: worker channel disconnected ({e})"
                );
            }
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
        // Cache file may not exist yet (invalidating before any analysis ran).
        // ENOENT is the expected case here, so we don't surface failures.
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
                // Best-effort sweep — a single failure shouldn't abort the
                // rest. The user can re-trigger invalidate_all if needed.
                if let Err(e) = std::fs::remove_file(entry.path()) {
                    log::warn!(
                        "tracking: failed to remove cache file {}: {e}",
                        entry.path().display()
                    );
                }
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

/// A single decoded analysis frame in YUV 4:4:4 (each plane has the
/// same width × height as the frame itself). Stored as three separate
/// buffers so the matcher can walk each channel with a stride equal to
/// `width`.
#[derive(Debug, Clone)]
struct YuvFrame {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

/// Template patch captured from the first analysis frame — kept as a
/// copy of the Y/U/V bytes inside the tracker region, plus per-channel
/// mean and mean-centered L2 energy `sqrt(sum((pixel - mean)²))`. The
/// energy is precomputed once because it's the denominator of every
/// NCC evaluation; without caching it we'd pay N² work per candidate.
///
/// The template is **not** updated between frames (unlike the
/// pre-color tracker) to prevent drift.
#[derive(Debug, Clone)]
struct TemplatePatch {
    y_pixels: Vec<u8>,
    y_mean: f64,
    y_energy: f64,
    u_pixels: Vec<u8>,
    u_mean: f64,
    u_energy: f64,
    v_pixels: Vec<u8>,
    v_mean: f64,
    v_energy: f64,
}

#[derive(Debug, Clone)]
struct FrameSequence {
    width: usize,
    height: usize,
    frames: Vec<YuvFrame>,
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

    let frames = extract_yuv_frames(job, &worker_job.cancel_flag)?;
    let analysis =
        track_motion_in_frames(&frames, job, &worker_job.cancel_flag, |processed, total| {
            // Periodic progress update — main thread may have dropped the
            // receiver if the user closed the project. A dropped progress
            // message is not fatal: the worker continues, the cache file
            // is still written on completion, and a future poll picks it up.
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
    sequence: &FrameSequence,
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
    // Template is captured ONCE from the first frame and reused for
    // every subsequent search — this is the key drift fix.  The old
    // tracker rewrote `template` at the end of each iteration from
    // whatever it matched, so a single-pixel search error cascaded
    // into ever-worse templates.
    let template = extract_template(&sequence.frames[0], sequence.width, current_rect)?;
    let effective_step_ns = job.effective_frame_step_ns();
    let total_frames = sequence.frames.len();
    let mut samples = Vec::with_capacity(total_frames);
    samples.push(TrackingSample::identity(job.analysis_start_ns));
    report_progress(1, total_frames);

    // Motion-prediction state: per-frame velocity in analysis pixels.
    // Seeded to zero; updated after each match from the delta between
    // the matched rect and the previous `current_rect`. Predicting the
    // next search center from this velocity effectively doubles the
    // tracker's tolerance to constant-velocity motion without doubling
    // the search radius (which would quadruple the compute).
    let mut velocity_x: i32 = 0;
    let mut velocity_y: i32 = 0;
    // Last rect whose match came back with high confidence. Recovery
    // searches center on this (not on a low-confidence drift) so a
    // brief bad match can't pull the tracker off-subject permanently.
    let mut last_good_rect = current_rect;
    let mut consecutive_low_confidence = 0u32;
    let normal_radius = job.effective_search_radius_px() as i32;

    for (frame_index, frame) in sequence.frames.iter().enumerate().skip(1) {
        if cancel_flag.load(Ordering::Relaxed) {
            return Err((TrackingFailure::Cancelled, "Tracking canceled".to_string()));
        }
        // 1. Predicted search center = current_rect + velocity,
        //    clamped to frame bounds so the candidate loop doesn't
        //    spend most of its iterations on invalid positions.
        let predicted_rect = translate_rect_clamped(
            current_rect,
            velocity_x,
            velocity_y,
            sequence.width as i32,
            sequence.height as i32,
        );

        // 2. Normal search around the prediction.
        let (mut matched_rect, mut confidence) = find_best_match(
            frame,
            sequence.width,
            sequence.height,
            &template,
            predicted_rect,
            normal_radius,
        )?;

        // 3. If the match is suspicious, run a wider recovery search
        //    centered on the LAST GOOD position (not the current, not
        //    the prediction — those may already be off-subject).
        //    Trigger is <= so a match sitting exactly on the
        //    threshold still gets a second chance at recovery.
        if confidence <= RECOVERY_CONFIDENCE_THRESHOLD
            && consecutive_low_confidence < MAX_CONSECUTIVE_LOW_CONFIDENCE_FRAMES
        {
            let (recovery_rect, recovery_confidence) = find_best_match(
                frame,
                sequence.width,
                sequence.height,
                &template,
                last_good_rect,
                RECOVERY_SEARCH_RADIUS_PX,
            )?;
            if recovery_confidence > confidence {
                matched_rect = recovery_rect;
                confidence = recovery_confidence;
            }
        }

        // 3b. Hard trajectory-continuity cap: clamp the per-frame
        //     delta to MAX_SINGLE_FRAME_JUMP_PX. At 320 analysis
        //     width this is 32 px = 10% of frame, which is faster
        //     than virtually any real subject moves at source frame
        //     rate. Anything bigger is the matcher teleporting to a
        //     distant local minimum — we preserve the direction but
        //     cap the magnitude. Recovery is still free to find a
        //     distant subject *over multiple frames* because the
        //     prediction accumulates; it just can't do it in one.
        let dx = matched_rect.x - current_rect.x;
        let dy = matched_rect.y - current_rect.y;
        if dx.abs() > MAX_SINGLE_FRAME_JUMP_PX || dy.abs() > MAX_SINGLE_FRAME_JUMP_PX {
            matched_rect = PixelRect {
                x: current_rect.x + dx.clamp(-MAX_SINGLE_FRAME_JUMP_PX, MAX_SINGLE_FRAME_JUMP_PX),
                y: current_rect.y + dy.clamp(-MAX_SINGLE_FRAME_JUMP_PX, MAX_SINGLE_FRAME_JUMP_PX),
                width: matched_rect.width,
                height: matched_rect.height,
            };
            // Don't promote this to a "good" frame — the clamp means
            // the actual best-score position was rejected, so the
            // confidence here is misleading.
            confidence = confidence.min(RECOVERY_CONFIDENCE_THRESHOLD);
        }

        // 4. Velocity update and anchoring logic. Three regimes:
        //    - "trustworthy" (confidence ≥ GOOD): update velocity via
        //      EMA, update last_good_rect, reset lost counter.
        //    - "uncertain" (GOOD > confidence ≥ RECOVERY): use the
        //      match but don't update last_good_rect. Still update
        //      velocity — on real footage the tracker often dips
        //      into this range during fast pans, and freezing the
        //      velocity there breaks prediction exactly when it's
        //      needed most.
        //    - "clearly lost" (confidence < RECOVERY): decay velocity
        //      so we don't march off-subject, increment the lost
        //      counter, and anchor after too many consecutive losses.
        //
        // Crucially, the anchor decision runs BEFORE the sample is
        // pushed so the sample reflects the anchor position, not the
        // bad pre-anchor match. Otherwise sample[i] would hold the
        // bad position and sample[i+1] (searched around the anchor)
        // would look like a giant teleport.
        const GOOD_CONFIDENCE: f64 = 0.4;
        let new_vx = matched_rect.x - current_rect.x;
        let new_vy = matched_rect.y - current_rect.y;
        if confidence >= GOOD_CONFIDENCE {
            velocity_x = (velocity_x + new_vx) / 2;
            velocity_y = (velocity_y + new_vy) / 2;
            last_good_rect = matched_rect;
            consecutive_low_confidence = 0;
        } else if confidence >= RECOVERY_CONFIDENCE_THRESHOLD {
            velocity_x = (velocity_x + new_vx) / 2;
            velocity_y = (velocity_y + new_vy) / 2;
            consecutive_low_confidence = consecutive_low_confidence.saturating_add(1);
        } else {
            velocity_x /= 2;
            velocity_y /= 2;
            consecutive_low_confidence = consecutive_low_confidence.saturating_add(1);
            if consecutive_low_confidence >= MAX_CONSECUTIVE_LOW_CONFIDENCE_FRAMES {
                // Anchor: freeze the search center at the last good
                // position for THIS sample onward. Also clamp the
                // per-frame delta so the anchor itself doesn't
                // produce a teleport — it moves toward last_good
                // but no faster than the jump budget allows.
                let adx = (last_good_rect.x - current_rect.x)
                    .clamp(-MAX_SINGLE_FRAME_JUMP_PX, MAX_SINGLE_FRAME_JUMP_PX);
                let ady = (last_good_rect.y - current_rect.y)
                    .clamp(-MAX_SINGLE_FRAME_JUMP_PX, MAX_SINGLE_FRAME_JUMP_PX);
                matched_rect = PixelRect {
                    x: current_rect.x + adx,
                    y: current_rect.y + ady,
                    width: matched_rect.width,
                    height: matched_rect.height,
                };
                velocity_x = 0;
                velocity_y = 0;
            }
        }

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
        report_progress(frame_index + 1, total_frames);
    }

    Ok(CachedTrackingAnalysis {
        analysis_region: job.analysis_region,
        analysis_start_ns: job.analysis_start_ns,
        analysis_end_ns: Some(job.analysis_end_ns),
        samples,
    })
}

/// Translate a rect by `(dx, dy)` and clamp it to the frame bounds so
/// the search loop doesn't iterate on candidates that are mostly
/// out-of-bounds.
fn translate_rect_clamped(
    rect: PixelRect,
    dx: i32,
    dy: i32,
    image_width: i32,
    image_height: i32,
) -> PixelRect {
    let max_x = (image_width - rect.width).max(0);
    let max_y = (image_height - rect.height).max(0);
    PixelRect {
        x: (rect.x + dx).clamp(0, max_x),
        y: (rect.y + dy).clamp(0, max_y),
        width: rect.width,
        height: rect.height,
    }
}

fn extract_yuv_frames(
    job: &TrackingJob,
    cancel_flag: &AtomicBool,
) -> Result<FrameSequence, (TrackingFailure, String)> {
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
    // YUV444P keeps all three channels at full analysis resolution
    // (important for chroma-driven subjects like stickers). The
    // `scale` filter uses `bicubic` + `full_chroma_int` for sharper
    // chroma downsampling than the old `bilinear` gray path.
    let filter = format!(
        "fps={fps:.6},scale={ANALYSIS_WIDTH}:{ANALYSIS_HEIGHT}:flags=bicubic+full_chroma_int,format=yuv444p"
    );
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
            "yuv444p",
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
            // ffmpeg child cleanup on cancel: kill+wait may race if the
            // process already exited; either way we just want it gone.
            let _ = child.kill();
            let _ = child.wait();
            return Err((TrackingFailure::Cancelled, "Tracking canceled".to_string()));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => {
                // Same race as above — kill+wait are best-effort.
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
    let plane_size = ANALYSIS_WIDTH * ANALYSIS_HEIGHT;
    let frame_size = plane_size * 3; // YUV444: Y + U + V, each full resolution
    if bytes.len() < frame_size {
        return Err((
            TrackingFailure::Failed,
            "Tracking decode produced no frames".to_string(),
        ));
    }
    let frames = bytes
        .chunks_exact(frame_size)
        .map(|chunk| YuvFrame {
            y: chunk[..plane_size].to_vec(),
            u: chunk[plane_size..2 * plane_size].to_vec(),
            v: chunk[2 * plane_size..].to_vec(),
        })
        .collect::<Vec<_>>();
    if frames.is_empty() {
        return Err((
            TrackingFailure::Failed,
            "Tracking decode produced no complete frames".to_string(),
        ));
    }
    Ok(FrameSequence {
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

/// Copy the pixel region for a single plane into a contiguous buffer
/// so the matcher can walk it linearly.
fn extract_plane_patch(plane: &[u8], image_width: usize, rect: PixelRect) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((rect.width * rect.height) as usize);
    for y in rect.y..rect.y + rect.height {
        let start = y as usize * image_width + rect.x as usize;
        let end = start + rect.width as usize;
        pixels.extend_from_slice(&plane[start..end]);
    }
    pixels
}

fn extract_template(
    frame: &YuvFrame,
    image_width: usize,
    rect: PixelRect,
) -> Result<TemplatePatch, (TrackingFailure, String)> {
    if rect.width <= 0 || rect.height <= 0 {
        return Err((
            TrackingFailure::Failed,
            "Tracking template rect is empty".to_string(),
        ));
    }
    let y_pixels = extract_plane_patch(&frame.y, image_width, rect);
    let u_pixels = extract_plane_patch(&frame.u, image_width, rect);
    let v_pixels = extract_plane_patch(&frame.v, image_width, rect);
    let y_mean = patch_mean(&y_pixels);
    let u_mean = patch_mean(&u_pixels);
    let v_mean = patch_mean(&v_pixels);
    let y_energy = plane_mean_centered_energy(&y_pixels, y_mean);
    let u_energy = plane_mean_centered_energy(&u_pixels, u_mean);
    let v_energy = plane_mean_centered_energy(&v_pixels, v_mean);
    Ok(TemplatePatch {
        y_pixels,
        y_mean,
        y_energy,
        u_pixels,
        u_mean,
        u_energy,
        v_pixels,
        v_mean,
        v_energy,
    })
}

/// L2 norm of the mean-centered template: `sqrt(Σ (p - mean)²)`.
/// Precomputed so every NCC evaluation is O(patch_len), not O(2*patch_len).
fn plane_mean_centered_energy(pixels: &[u8], mean: f64) -> f64 {
    let mut sum_sq = 0.0f64;
    for p in pixels {
        let d = *p as f64 - mean;
        sum_sq += d * d;
    }
    sum_sq.sqrt()
}

fn find_best_match(
    frame: &YuvFrame,
    image_width: usize,
    image_height: usize,
    template: &TemplatePatch,
    current_rect: PixelRect,
    search_radius_px: i32,
) -> Result<(PixelRect, f64), (TrackingFailure, String)> {
    let mut best_rect = current_rect;
    // Scores are NCC in [-1, 1] summed across three planes, so the
    // total is in [-3, 3]. Start below the minimum so any valid
    // candidate replaces it.
    let mut best_score = f64::NEG_INFINITY;
    let mut second_best = f64::NEG_INFINITY;
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
            let score = yuv_ncc(frame, image_width, template, candidate);
            if score > best_score {
                second_best = best_score;
                best_score = score;
                best_rect = candidate;
            } else if score > second_best {
                second_best = score;
            }
        }
    }
    if best_score.is_infinite() {
        return Err((
            TrackingFailure::Failed,
            "Tracking search failed to find a valid candidate window".to_string(),
        ));
    }
    Ok((best_rect, tracking_confidence_ncc(best_score, second_best)))
}

/// Normalized cross-correlation for a single plane. Returns a value in
/// `[-1, 1]` where 1 is a perfect match, 0 is uncorrelated, -1 is
/// inverted. Unlike mean-centered SAD, NCC is invariant to both
/// additive brightness offsets AND multiplicative contrast changes,
/// so mild exposure / lighting shifts between the first frame and
/// later frames don't tank the score. Most importantly for our case,
/// the score curve is much steeper around the true match — a 1 px
/// drift from the correct position drops NCC visibly, where SAD would
/// have looked similar — so the confidence signal actually
/// discriminates "on-subject" from "close but wrong."
fn plane_ncc(
    plane: &[u8],
    image_width: usize,
    template_pixels: &[u8],
    template_mean: f64,
    template_energy: f64,
    candidate: PixelRect,
) -> f64 {
    let patch_len = (candidate.width * candidate.height) as usize;
    if patch_len == 0 || template_pixels.len() != patch_len {
        return -1.0;
    }
    // First pass: candidate mean.
    let mut candidate_sum = 0u64;
    for y in candidate.y..candidate.y + candidate.height {
        let start = y as usize * image_width + candidate.x as usize;
        let end = start + candidate.width as usize;
        candidate_sum += plane[start..end]
            .iter()
            .map(|value| *value as u64)
            .sum::<u64>();
    }
    let candidate_mean = candidate_sum as f64 / patch_len as f64;

    // Second pass: numerator Σ(T_centered * C_centered) and candidate energy.
    let mut num = 0.0f64;
    let mut cand_sum_sq = 0.0f64;
    let mut template_index = 0usize;
    for y in candidate.y..candidate.y + candidate.height {
        let start = y as usize * image_width + candidate.x as usize;
        let end = start + candidate.width as usize;
        for pixel in &plane[start..end] {
            let t_centered = template_pixels[template_index] as f64 - template_mean;
            let c_centered = *pixel as f64 - candidate_mean;
            num += t_centered * c_centered;
            cand_sum_sq += c_centered * c_centered;
            template_index += 1;
        }
    }
    let candidate_energy = cand_sum_sq.sqrt();
    let denom = template_energy * candidate_energy;
    if denom < 1e-9 {
        // Degenerate case: either the template is flat (zero energy)
        // or the candidate is flat. We can't compute a meaningful
        // correlation, so score it as "weakly uncorrelated" rather
        // than lying with a +1 / -1 extreme.
        return 0.0;
    }
    (num / denom).clamp(-1.0, 1.0)
}

/// Sum the per-plane NCC scores across Y, U, and V. Range is `[-3, 3]`.
fn yuv_ncc(
    frame: &YuvFrame,
    image_width: usize,
    template: &TemplatePatch,
    candidate: PixelRect,
) -> f64 {
    let y = plane_ncc(
        &frame.y,
        image_width,
        &template.y_pixels,
        template.y_mean,
        template.y_energy,
        candidate,
    );
    let u = plane_ncc(
        &frame.u,
        image_width,
        &template.u_pixels,
        template.u_mean,
        template.u_energy,
        candidate,
    );
    let v = plane_ncc(
        &frame.v,
        image_width,
        &template.v_pixels,
        template.v_mean,
        template.v_energy,
        candidate,
    );
    y + u + v
}

/// Translate the summed NCC score (`[-3, 3]`) into a `[0, 1]`
/// confidence. We:
///   1. Normalize the best score to `[0, 1]` by clamping the negative
///      range away (a "match" that correlates negatively is nonsense
///      for our purpose — we only care about positive correlation).
///   2. Weight in a margin component: the gap between the best and
///      second-best score. Wide gaps mean the matcher is confident in
///      one specific position; narrow gaps mean multiple candidates
///      look alike and the match is ambiguous.
fn tracking_confidence_ncc(best_score: f64, second_best: f64) -> f64 {
    // Best NCC sum is in [-3, 3]. Map positive range to [0, 1].
    let best_norm = (best_score / 3.0).clamp(0.0, 1.0);
    let margin = if second_best.is_infinite() {
        1.0
    } else {
        ((best_score - second_best) / 3.0).clamp(0.0, 1.0)
    };
    (best_norm * 0.7 + margin * 0.3).clamp(0.0, 1.0)
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

    /// Produce a synthetic grayscale plane with a bright square at
    /// `(x, y)`. Used as the shared base for Y/U/V planes in tests so
    /// the matcher can detect translation across all three channels.
    fn make_synthetic_plane(width: usize, height: usize, x: usize, y: usize) -> Vec<u8> {
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

    /// Wrap the same grayscale plane as all three YUV channels. Good
    /// enough for the existing translation tests — the matcher still
    /// scores across Y+U+V so the test exercises the full pipeline.
    fn make_synthetic_yuv(width: usize, height: usize, x: usize, y: usize) -> YuvFrame {
        let plane = make_synthetic_plane(width, height, x, y);
        YuvFrame {
            y: plane.clone(),
            // Shift U/V slightly so they carry an independent signal;
            // keeps the test sensitive to plane mix-ups while staying
            // deterministic.
            u: plane.iter().map(|p| p.saturating_add(8)).collect(),
            v: plane.iter().map(|p| p.saturating_sub(8)).collect(),
        }
    }

    /// Colored region test: the background has low-amplitude texture
    /// in all channels, and the tracked object is distinctively
    /// different in U/V but nearly identical in Y. A pure-luma matcher
    /// would slide off onto background texture; this exercises the
    /// color path explicitly. The low-amplitude background texture
    /// ensures the mean-centered SAD actually distinguishes positions
    /// (completely uniform backgrounds degenerate to score 0 under
    /// mean-centered matching, because all centered values are 0).
    fn make_color_distinct_yuv(width: usize, height: usize, x: usize, y: usize) -> YuvFrame {
        // Deterministic 1-2 bit checker texture to break the uniform
        // degeneracy of mean-centered SAD without adding enough signal
        // to out-vote the strong chroma target.
        let mut ybuf = vec![0u8; width * height];
        let mut ubuf = vec![0u8; width * height];
        let mut vbuf = vec![0u8; width * height];
        for row in 0..height {
            for col in 0..width {
                let idx = row * width + col;
                let noise = (((row * 7 + col * 13) % 5) as u8).saturating_sub(2);
                ybuf[idx] = 128u8.saturating_add(noise);
                ubuf[idx] = 128u8.saturating_add(noise);
                vbuf[idx] = 128u8.saturating_add(noise);
            }
        }
        // Stamp a 4×4 object in the centre of the template window (so
        // the template captures a mix of target and surround — this
        // keeps the mean-centered SAD non-degenerate). Y is nearly
        // unchanged so only the chroma path can lock on.
        for row in (y + 2)..(y + 6).min(height) {
            for col in (x + 2)..(x + 6).min(width) {
                let idx = row * width + col;
                ybuf[idx] = 130;
                ubuf[idx] = 210; // strong chroma anomaly
                vbuf[idx] = 50;
            }
        }
        YuvFrame {
            y: ybuf,
            u: ubuf,
            v: vbuf,
        }
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
        // Patterned target so NCC has real signal to lock onto —
        // uniform targets produce zero-energy templates that NCC
        // can't discriminate (division by zero → 0.0 for every
        // candidate, regardless of position).
        let frames = (0..5)
            .map(|idx| make_patterned_yuv(width, height, 18 + idx * 2, 10 + idx))
            .collect::<Vec<_>>();
        let sequence = FrameSequence {
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
    fn track_motion_sequence_detects_chroma_only_target() {
        // Target is uniform in luma but distinct in U/V — the old
        // grayscale-only matcher would have no signal to lock onto.
        // The new YUV matcher should track it cleanly.
        let width = 64;
        let height = 40;
        let frames = (0..5)
            .map(|idx| make_color_distinct_yuv(width, height, 18 + idx * 3, 10 + idx * 2))
            .collect::<Vec<_>>();
        let sequence = FrameSequence {
            width,
            height,
            frames,
        };
        let mut job = TrackingJob::new(
            "tracker-chroma",
            "ColorSubject",
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
        job.search_radius_px = 8;
        let cancel_flag = AtomicBool::new(false);
        let analysis = track_motion_in_frames(&sequence, &job, &cancel_flag, |_, _| {})
            .expect("chroma-only tracking should succeed");
        assert_eq!(analysis.samples.len(), 5);
        // Frame idx 2 moved 6 px right, 4 px down from initial.
        assert!(
            (analysis.samples[2].offset_x - 6.0 / width as f64).abs() < 0.03,
            "offset_x sample[2] = {}",
            analysis.samples[2].offset_x
        );
        assert!(
            (analysis.samples[2].offset_y - 4.0 / height as f64).abs() < 0.03,
            "offset_y sample[2] = {}",
            analysis.samples[2].offset_y
        );
    }

    /// Build a frame where the tracked "object" is a patterned 8×8
    /// square (checker) instead of a uniform block — keeps the
    /// mean-centered SAD non-degenerate so tests actually exercise
    /// the matcher's signal path.
    fn make_patterned_yuv(width: usize, height: usize, x: usize, y: usize) -> YuvFrame {
        // Textured background so background candidates don't score 0
        // under mean-centered matching.
        let mut y_plane = vec![0u8; width * height];
        let mut u_plane = vec![0u8; width * height];
        let mut v_plane = vec![0u8; width * height];
        for row in 0..height {
            for col in 0..width {
                let idx = row * width + col;
                let noise = (((row * 11 + col * 7) % 7) as u8).saturating_sub(3);
                y_plane[idx] = 80u8.saturating_add(noise);
                u_plane[idx] = 128u8.saturating_add(noise);
                v_plane[idx] = 128u8.saturating_add(noise);
            }
        }
        // Patterned object: alternating bright/dark + colored
        // checker so the template has rich structure that the
        // matcher can lock on to even when pixels are only partially
        // overlapped.
        for row in y..(y + 8).min(height) {
            for col in x..(x + 8).min(width) {
                let idx = row * width + col;
                let on = ((row + col) & 1) == 0;
                y_plane[idx] = if on { 220 } else { 30 };
                u_plane[idx] = if on { 210 } else { 60 };
                v_plane[idx] = if on { 60 } else { 210 };
            }
        }
        YuvFrame {
            y: y_plane,
            u: u_plane,
            v: v_plane,
        }
    }

    #[test]
    fn track_motion_sequence_follows_fast_motion_via_prediction() {
        // Subject moves 10 px per frame in x — faster than a 6-px
        // search radius allows without motion prediction. The
        // prediction heuristic (velocity estimate + predicted center)
        // should still keep the matcher locked on.
        let width = 128;
        let height = 40;
        let frames = (0..6)
            .map(|idx| make_patterned_yuv(width, height, 18 + idx * 10, 14))
            .collect::<Vec<_>>();
        let sequence = FrameSequence {
            width,
            height,
            frames,
        };
        let mut job = TrackingJob::new(
            "tracker-fast",
            "Fast",
            "/tmp/source.mp4",
            0,
            0,
            500_000_000,
            TrackingRegion {
                center_x: 22.0 / width as f64,
                center_y: 18.0 / height as f64,
                width: 4.0 / width as f64,
                height: 4.0 / height as f64,
                rotation_deg: 0.0,
            },
        );
        job.frame_step_ns = 100_000_000;
        // Deliberately small — without motion prediction the matcher
        // could only shift 6 px per frame, losing a 10-px/frame
        // subject after the first step.
        job.search_radius_px = 6;
        let cancel_flag = AtomicBool::new(false);
        let analysis = track_motion_in_frames(&sequence, &job, &cancel_flag, |_, _| {})
            .expect("fast-motion tracking should succeed");
        assert_eq!(analysis.samples.len(), 6);
        // Frame 5 moved 50 px — the tracker should still report ~50/width.
        let expected = 50.0 / width as f64;
        let actual = analysis.samples[5].offset_x;
        assert!(
            (actual - expected).abs() < 0.05,
            "offset_x sample[5] = {actual}, expected ~{expected}"
        );
    }

    #[test]
    fn track_motion_sequence_honors_cancel_flag() {
        let sequence = FrameSequence {
            width: 32,
            height: 18,
            frames: vec![make_synthetic_yuv(32, 18, 8, 4)],
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

    // ─── compute_auto_crop_binding_values ─────────────────────────────

    fn region(cx: f64, cy: f64, half_w: f64, half_h: f64) -> TrackingRegion {
        TrackingRegion {
            center_x: cx,
            center_y: cy,
            width: half_w,
            height: half_h,
            rotation_deg: 0.0,
        }
    }

    #[test]
    fn auto_crop_same_aspect_centered_region_produces_zero_offsets() {
        // 16:9 source in a 16:9 project, region covering 50% of the source
        // (full width = 2 * 0.25), centered. Expect tight zoom, zero offset.
        let values = compute_auto_crop_binding_values(&AutoCropInputs {
            region: region(0.5, 0.5, 0.25, 0.25),
            source_width: 1920,
            source_height: 1080,
            project_width: 1920,
            project_height: 1080,
            padding: 0.1,
        });
        // s_fill = 1, s_tight = 1/(0.5*1*1.1) ≈ 1.818 → scale ≈ 1.818.
        assert!(
            (values.scale_multiplier - 1.818).abs() < 0.01,
            "scale={}",
            values.scale_multiplier
        );
        assert!(values.offset_x.abs() < 1e-9);
        assert!(values.offset_y.abs() < 1e-9);
    }

    #[test]
    fn auto_crop_same_aspect_full_frame_region_clamps_to_unit_scale() {
        // Full-frame region in same-aspect project: scale should clamp to
        // 1.0 (no zoom) and offsets should be zero.
        let values = compute_auto_crop_binding_values(&AutoCropInputs {
            region: region(0.5, 0.5, 0.5, 0.5),
            source_width: 1920,
            source_height: 1080,
            project_width: 1920,
            project_height: 1080,
            padding: 0.1,
        });
        assert!(
            (values.scale_multiplier - 1.0).abs() < 1e-9,
            "scale={}",
            values.scale_multiplier
        );
        assert!(values.offset_x.abs() < 1e-9);
        assert!(values.offset_y.abs() < 1e-9);
    }

    #[test]
    fn auto_crop_same_aspect_offset_region_centers_it() {
        // Region at the top-left quadrant of a same-aspect source.  We
        // expect positive offsets (clip shifted toward origin so that the
        // top-left region ends up centered in the project frame).
        let values = compute_auto_crop_binding_values(&AutoCropInputs {
            region: region(0.25, 0.25, 0.15, 0.15),
            source_width: 1920,
            source_height: 1080,
            project_width: 1920,
            project_height: 1080,
            padding: 0.0,
        });
        // cw_frac=ch_frac=1; scale = 1/(2*0.15*1) ≈ 3.333.
        // offset_x = 1 * scale * (1 - 2*0.25) = scale * 0.5.
        let expected_scale = 1.0 / 0.30_f64;
        assert!((values.scale_multiplier - expected_scale).abs() < 1e-6);
        assert!((values.offset_x - expected_scale * 0.5).abs() < 1e-6);
        assert!((values.offset_y - expected_scale * 0.5).abs() < 1e-6);
    }

    #[test]
    fn auto_crop_horizontal_to_vertical_fills_project() {
        // 1920x1080 (16:9) source in a 1080x1920 (9:16) project with a
        // region centered in the source.  Expect s_fill to dominate
        // (scale ≥ 1/ch_frac ≈ 3.16) so the reframed crop has no
        // letterbox bars.
        let values = compute_auto_crop_binding_values(&AutoCropInputs {
            region: region(0.5, 0.5, 0.2, 0.2),
            source_width: 1920,
            source_height: 1080,
            project_width: 1080,
            project_height: 1920,
            padding: 0.0,
        });
        // cw_frac = 1.0 (source wider than project)
        // ch_frac = (1080*9/16) / 1920 = 607.5 / 1920 ≈ 0.31641
        // s_fill = max(1, 1/0.31641) ≈ 3.1605
        // s_tight_w = 1/(2*0.2*1) = 2.5; s_tight_h = 1/(2*0.2*0.31641) ≈ 7.9
        // Final scale = max(3.16, 2.5) = 3.16, then clamped to SCALE_MAX=4.
        let expected_scale = 1920.0 / 607.5; // = 1/ch_frac
        assert!(
            (values.scale_multiplier - expected_scale).abs() < 0.01,
            "scale={}",
            values.scale_multiplier
        );
        // Region is centered → offsets are zero.
        assert!(values.offset_x.abs() < 1e-9);
        assert!(values.offset_y.abs() < 1e-9);
    }

    #[test]
    fn auto_crop_horizontal_to_vertical_offset_region_pans_horizontally() {
        // Same 16:9 → 9:16 reframe, but region biased to the right side of
        // the source.  Expect negative offset_x (clip shifted left so the
        // right-biased region appears centered).
        let values = compute_auto_crop_binding_values(&AutoCropInputs {
            region: region(0.75, 0.5, 0.15, 0.15),
            source_width: 1920,
            source_height: 1080,
            project_width: 1080,
            project_height: 1920,
            padding: 0.0,
        });
        // cw_frac = 1, so offset_x = 1 * scale * (1 - 2*0.75) = -0.5 * scale.
        assert!(values.offset_x < 0.0);
        assert!(values.offset_y.abs() < 1e-9);
        let expected_offset_x = -0.5 * values.scale_multiplier;
        assert!(
            (values.offset_x - expected_offset_x).abs() < 1e-6,
            "offset_x={}, expected={}",
            values.offset_x,
            expected_offset_x
        );
    }

    #[test]
    fn auto_crop_vertical_to_horizontal_fills_project() {
        // 1080x1920 (9:16) source → 1920x1080 (16:9) project, centered
        // region.  Mirror of the horizontal→vertical test: s_fill should
        // dominate via the width axis.
        let values = compute_auto_crop_binding_values(&AutoCropInputs {
            region: region(0.5, 0.5, 0.2, 0.2),
            source_width: 1080,
            source_height: 1920,
            project_width: 1920,
            project_height: 1080,
            padding: 0.0,
        });
        // Source is taller than project, so fit-by-height:
        //   content_h = project_height = 1080, content_w = 1080*(9/16) = 607.5
        //   cw_frac ≈ 0.31641, ch_frac = 1.0
        // s_fill = max(1/0.31641, 1) ≈ 3.16.
        let expected_scale = 1920.0 / 607.5;
        assert!(
            (values.scale_multiplier - expected_scale).abs() < 0.01,
            "scale={}",
            values.scale_multiplier
        );
        assert!(values.offset_x.abs() < 1e-9);
        assert!(values.offset_y.abs() < 1e-9);
    }

    #[test]
    fn auto_crop_respects_scale_max_clamp() {
        // Tiny region → math would demand scale > SCALE_MAX; verify clamp.
        let values = compute_auto_crop_binding_values(&AutoCropInputs {
            region: region(0.5, 0.5, 0.02, 0.02),
            source_width: 1920,
            source_height: 1080,
            project_width: 1920,
            project_height: 1080,
            padding: 0.0,
        });
        assert!(
            (values.scale_multiplier - crate::model::transform_bounds::SCALE_MAX).abs() < 1e-9,
            "scale={}",
            values.scale_multiplier
        );
    }

    #[test]
    fn auto_crop_binding_wrapper_sets_expected_fields() {
        let binding = compute_auto_crop_binding(
            "clip-id",
            "tracker-id",
            &AutoCropInputs {
                region: region(0.5, 0.5, 0.25, 0.25),
                source_width: 1920,
                source_height: 1080,
                project_width: 1920,
                project_height: 1080,
                padding: 0.1,
            },
        );
        assert_eq!(binding.source_clip_id, "clip-id");
        assert_eq!(binding.tracker_id, "tracker-id");
        assert!(binding.apply_translation);
        assert!(binding.apply_scale);
        assert!(!binding.apply_rotation);
        assert!((binding.strength - 1.0).abs() < 1e-9);
        assert!((binding.scale_multiplier - 1.818).abs() < 0.01);
    }
}
