use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Condvar, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::media::hwaccel::{self, HwEncoderFamily};
use crate::ui_state::{HwEncoderMode, ProxyCodec};

/// Source pixel height at and above which a transcode counts as a "heavy"
/// job and acquires a permit from [`HeavyPermit`] before launching ffmpeg.
/// 2160 = 4K UHD; 6K / 8K sources are also heavy.
pub const HEAVY_SOURCE_HEIGHT_THRESHOLD: u32 = 2160;

/// Maximum number of "heavy" (4K+) ffmpeg transcodes allowed in flight at
/// once. Prevents the worker pool from oversaturating decoder/GPU resources
/// on high-resolution sources where 4 simultaneous decodes contend for the
/// same hardware. Light (sub-4K) jobs ignore this cap.
const MAX_HEAVY_PARALLEL_TRANSCODES: u32 = 2;

/// Counting semaphore implemented with [`Mutex`] + [`Condvar`]. Used to gate
/// concurrent heavy proxy transcodes without restructuring the work queue.
pub(crate) struct HeavyPermit {
    available: Mutex<u32>,
    cond: Condvar,
}

impl HeavyPermit {
    fn new(initial: u32) -> Self {
        Self {
            available: Mutex::new(initial),
            cond: Condvar::new(),
        }
    }

    /// Block until a permit is available, then take one. Returned guard
    /// releases the permit on drop.
    fn acquire(self: &Arc<Self>) -> HeavyPermitGuard {
        let mut g = self.available.lock().unwrap();
        while *g == 0 {
            g = self.cond.wait(g).unwrap();
        }
        *g -= 1;
        HeavyPermitGuard {
            owner: self.clone(),
        }
    }

    fn release(&self) {
        let mut g = self.available.lock().unwrap();
        *g += 1;
        self.cond.notify_one();
    }
}

pub(crate) struct HeavyPermitGuard {
    owner: Arc<HeavyPermit>,
}

impl Drop for HeavyPermitGuard {
    fn drop(&mut self) {
        self.owner.release();
    }
}

/// Result of a background proxy transcode.
pub struct ProxyResult {
    /// Composite cache key (see `proxy_key()`).
    pub cache_key: String,
    pub proxy_path: String,
    pub success: bool,
    /// True when this proxy was newly created in UltimateSlice's managed local cache root.
    pub owned_local: bool,
}

/// Background worker updates (progress + completion).
pub enum ProxyWorkerUpdate {
    Progress {
        cache_key: String,
        written_bytes: u64,
        estimated_bytes: u64,
    },
    Done(ProxyResult),
}

/// Progress snapshot for the status bar.
pub struct ProxyProgress {
    pub total: usize,
    pub completed: usize,
    pub in_flight: bool,
    pub byte_fraction: Option<f64>,
}

/// Scale factor for proxy transcodes. Caps the height (preserving aspect ratio
/// and never upscaling) so proxy decode cost is constant regardless of source
/// resolution — vital for very high-res sources like 6.2K GoPro footage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProxyScale {
    MaxHeight(u32),
}

impl ProxyScale {
    pub fn ffmpeg_scale_filter(&self) -> String {
        match self {
            // -2 picks an even width that preserves the source aspect ratio.
            // min(N,ih) prevents upscaling when the source is already shorter.
            ProxyScale::MaxHeight(h) => format!("scale=-2:'min({h},ih)':flags=lanczos"),
        }
    }

    fn suffix(&self) -> String {
        match self {
            ProxyScale::MaxHeight(h) => format!("{h}p"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProxyVariantSpec {
    pub source_path: String,
    pub scale: ProxyScale,
    pub lut_path: Option<String>,
    /// Codec the variant was transcoded with. Carried so cleanup-pass
    /// path resolution matches the on-disk filename produced by the
    /// worker (which folds codec into `proxy_filename_for_variant`).
    pub codec: ProxyCodec,
    pub vidstab_enabled: bool,
    vidstab_smoothing_hundredths: i32,
}

impl ProxyVariantSpec {
    /// Construct a spec assuming the historical H.264 codec. Existing
    /// callers that don't carry codec context (legacy paths, tests) keep
    /// using this — see [`ProxyVariantSpec::with_codec`] for the
    /// codec-aware variant.
    pub fn new(
        source_path: impl Into<String>,
        scale: ProxyScale,
        lut_path: Option<String>,
        vidstab_enabled: bool,
        vidstab_smoothing: f32,
    ) -> Self {
        Self::with_codec(
            source_path,
            scale,
            lut_path,
            ProxyCodec::H264,
            vidstab_enabled,
            vidstab_smoothing,
        )
    }

    pub fn with_codec(
        source_path: impl Into<String>,
        scale: ProxyScale,
        lut_path: Option<String>,
        codec: ProxyCodec,
        vidstab_enabled: bool,
        vidstab_smoothing: f32,
    ) -> Self {
        Self {
            source_path: source_path.into(),
            scale,
            lut_path,
            codec,
            vidstab_enabled,
            vidstab_smoothing_hundredths: normalized_vidstab_smoothing_hundredths(
                vidstab_enabled,
                vidstab_smoothing,
            ),
        }
    }

    pub fn lut_key(&self) -> Option<&str> {
        self.lut_path.as_deref()
    }

    pub fn vidstab_smoothing(&self) -> f32 {
        self.vidstab_smoothing_hundredths as f32 / 100.0
    }

    fn source_hash(&self) -> u64 {
        source_path_hash(&self.source_path)
    }
}

/// Summary of a proxy-cache cleanup pass. Fields are read by callers via
/// pattern destructuring rather than by name, which the dead-code lint
/// doesn't see — annotate to keep the warnings out of the build.
#[derive(Default)]
#[allow(dead_code)]
pub struct ProxyCleanupSummary {
    pub removed_local: usize,
    pub removed_sidecar: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProxySidecarUsage {
    pub directories: Vec<PathBuf>,
    pub file_count: usize,
    pub size_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SourceSignature {
    len: u64,
    modified_secs: u64,
    modified_nanos: u32,
}

/// Build the composite cache key used in `ProxyCache::proxies` and `pending`.
/// Encodes the source path, any assigned LUT, and vidstab state so that a
/// change to any of these produces a distinct key (and therefore a distinct proxy).
pub fn proxy_key(source_path: &str, lut_path: Option<&str>) -> String {
    proxy_key_with_vidstab(source_path, lut_path, false, 0.0)
}

/// Extended composite key including vidstab stabilization state. Defaults
/// to H.264 codec for backwards compatibility — see
/// [`proxy_key_with_codec_and_vidstab`] for the codec-aware variant.
pub fn proxy_key_with_vidstab(
    source_path: &str,
    lut_path: Option<&str>,
    vidstab_enabled: bool,
    vidstab_smoothing: f32,
) -> String {
    proxy_key_with_codec_and_vidstab(
        source_path,
        lut_path,
        ProxyCodec::H264,
        vidstab_enabled,
        vidstab_smoothing,
    )
}

/// Authoritative composite cache key. H.264 — the historical default —
/// produces the same key shape as the original `proxy_key_with_vidstab`
/// so existing on-disk caches stay valid for users who never touch the
/// codec preference. HEVC appends a `|c:hevc` discriminator so it lives
/// at a distinct cache slot and gets its own transcode.
pub fn proxy_key_with_codec_and_vidstab(
    source_path: &str,
    lut_path: Option<&str>,
    codec: ProxyCodec,
    vidstab_enabled: bool,
    vidstab_smoothing: f32,
) -> String {
    let mut key = match lut_path {
        Some(lut) if !lut.is_empty() => format!("{}|lut:{}", source_path, lut),
        _ => source_path.to_string(),
    };
    if vidstab_enabled && vidstab_smoothing > 0.0 {
        key.push_str(&format!("|vs:{:.2}", vidstab_smoothing));
    }
    if matches!(codec, ProxyCodec::Hevc) {
        key.push_str("|c:hevc");
    }
    key
}

/// Asynchronous proxy media cache.
///
/// Uses a single background worker thread to transcode source media files
/// into lightweight H.264 proxy files via ffmpeg. Follows the same
/// request/poll/get pattern as `MediaProbeCache` and `ThumbnailCache`.
///
/// Proxy files can live either in the managed local cache root or in an
/// `UltimateSlice.cache/` directory next to the source file.
///
/// The map key is a composite of source path + optional LUT path (via
/// `proxy_key()`), so changing the proxy scale or a clip's LUT assignment
/// triggers a fresh transcode to a distinct output file.
pub struct ProxyCache {
    /// Map from composite key → proxy file path (completed only).
    pub proxies: HashMap<String, String>,
    /// Composite keys currently being transcoded or queued.
    pending: HashSet<String>,
    /// Keys whose transcoding failed — never re-enqueue these.
    failed: HashSet<String>,
    /// Total items ever requested in this session (for progress).
    total_requested: usize,
    result_rx: mpsc::Receiver<ProxyWorkerUpdate>,
    work_tx: Option<mpsc::Sender<(String, ProxyScale, Vec<String>, bool, bool, f32, ProxyCodec)>>,
    /// Per-job estimated bytes and written bytes for byte-based status progress.
    estimated_bytes: HashMap<String, u64>,
    written_bytes: HashMap<String, u64>,
    /// Managed local cache root (`$XDG_CACHE_HOME/ultimateslice/proxies` or `/tmp/...` fallback).
    local_cache_root: PathBuf,
    /// Optional ffprobe path used to validate on-disk proxy readiness.
    ffprobe_path: Option<String>,
    /// Local managed-cache files created by this process.
    runtime_owned_local_files: HashSet<String>,
    /// When true, successful local proxy transcodes are mirrored to
    /// alongside-media `UltimateSlice.cache` files too.
    sidecar_mirror_enabled: bool,
    /// User preference: which hardware encoder family (if any) to try before
    /// falling back to libx264 for proxy transcodes. Shared with worker
    /// threads via `RwLock` so live preference changes apply to *future*
    /// jobs without disturbing in-flight transcodes.
    hw_encoder_mode: Arc<RwLock<HwEncoderMode>>,
    /// User preference: which video codec to use for proxy files. H.264
    /// for compatibility, HEVC for smaller files / faster iGPU encode.
    proxy_codec: Arc<RwLock<ProxyCodec>>,
    /// Source paths that failed under HW encoding in this session and should
    /// be transcoded with libx264 directly to avoid the wasted HW attempt.
    hw_failed_sources: Arc<Mutex<HashSet<String>>>,
    /// Source paths that failed under HW *decode* in this session and should
    /// be re-decoded in software directly.
    hw_decode_failed_sources: Arc<Mutex<HashSet<String>>>,
    /// Process-global blacklist of HW *encoder* families that have failed
    /// at runtime during this session. Future picks skip the family
    /// straight to libx264/libx265 instead of repeating the wasted
    /// attempt for every source. Cleared when the encoder mode or codec
    /// preference changes (the new combination might just work).
    hw_encoder_blacklist: Arc<Mutex<HashSet<HwEncoderFamily>>>,
    /// Process-global blacklist of HW *decode* methods (`cuda`, `qsv`,
    /// `vaapi`) that have failed at runtime. Same semantics as
    /// `hw_encoder_blacklist` — saves wasted per-source HW attempts on
    /// machines where startup probes passed but runtime use still fails
    /// (rare; usually narrows down to a specific source codec edge case).
    hw_decode_blacklist: Arc<Mutex<HashSet<String>>>,
    /// Counting semaphore that limits concurrent transcodes of sources at
    /// or above [`HEAVY_SOURCE_HEIGHT_THRESHOLD`]. Workers that pick up a
    /// light source bypass the permit entirely. Held only to keep the
    /// Arc alive for the lifetime of the cache; the worker threads each
    /// own their own clone.
    #[allow(dead_code)]
    heavy_permit: Arc<HeavyPermit>,
}

impl ProxyCache {
    pub fn new() -> Self {
        let local_cache_root = local_proxy_root();
        let _ = std::fs::create_dir_all(&local_cache_root);
        let pruned = prune_stale_owned_entries(&local_cache_root, 24 * 60 * 60);
        if pruned > 0 {
            log::info!(
                "ProxyCache: pruned {} stale local proxy cache file(s) from {}",
                pruned,
                local_cache_root.display()
            );
        }
        let (result_tx, result_rx) = mpsc::sync_channel::<ProxyWorkerUpdate>(64);
        // (source_path, scale, lut_paths, sidecar_mirror, vidstab_enabled,
        //  vidstab_smoothing, codec_snapshot)
        // The codec is snapshotted at enqueue time so the worker's cache
        // key matches what `request_with_vidstab` already reserved in
        // `pending` — without this, a codec preference change between
        // enqueue and dequeue would orphan the work item.
        let (work_tx, work_rx) =
            mpsc::channel::<(String, ProxyScale, Vec<String>, bool, bool, f32, ProxyCodec)>();

        let hw_encoder_mode = Arc::new(RwLock::new(HwEncoderMode::default()));
        let proxy_codec = Arc::new(RwLock::new(ProxyCodec::default()));
        let hw_failed_sources: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        let hw_decode_failed_sources: Arc<Mutex<HashSet<String>>> =
            Arc::new(Mutex::new(HashSet::new()));
        let hw_encoder_blacklist: Arc<Mutex<HashSet<HwEncoderFamily>>> =
            Arc::new(Mutex::new(HashSet::new()));
        let hw_decode_blacklist: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        let heavy_permit = Arc::new(HeavyPermit::new(MAX_HEAVY_PARALLEL_TRANSCODES));

        // Pool of worker threads to transcode proxies in parallel.
        let work_rx = std::sync::Arc::new(std::sync::Mutex::new(work_rx));
        let num_workers = 4;
        for _ in 0..num_workers {
            let rx = work_rx.clone();
            let tx = result_tx.clone();
            let local_root = local_cache_root.clone();
            let hw_mode_ref = hw_encoder_mode.clone();
            let proxy_codec_ref = proxy_codec.clone();
            let hw_failed_ref = hw_failed_sources.clone();
            let hw_decode_failed_ref = hw_decode_failed_sources.clone();
            let enc_blacklist_ref = hw_encoder_blacklist.clone();
            let dec_blacklist_ref = hw_decode_blacklist.clone();
            let heavy_permit_ref = heavy_permit.clone();
            std::thread::spawn(move || loop {
                let item = {
                    let lock = rx.lock().unwrap();
                    lock.recv()
                };
                match item {
                    Ok((
                        source_path,
                        scale,
                        lut_paths,
                        sidecar_mirror_enabled,
                        vidstab_enabled,
                        vidstab_smoothing,
                        codec_snapshot,
                    )) => {
                        let lut_composite = if lut_paths.is_empty() {
                            None
                        } else {
                            Some(lut_paths.join("|"))
                        };
                        let key = proxy_key_with_codec_and_vidstab(
                            &source_path,
                            lut_composite.as_deref(),
                            codec_snapshot,
                            vidstab_enabled,
                            vidstab_smoothing,
                        );
                        let mode_snapshot = *hw_mode_ref.read().unwrap();
                        // codec_snapshot already arrived in the work tuple — keep
                        // proxy_codec_ref alive in the closure so the outer Arc
                        // doesn't get dropped, but read from the message.
                        let _ = &proxy_codec_ref;
                        let already_failed_enc = hw_failed_ref
                            .lock()
                            .map(|set| set.contains(&source_path))
                            .unwrap_or(false);
                        let session_blacklisted_enc = |fam: HwEncoderFamily| {
                            enc_blacklist_ref
                                .lock()
                                .map(|set| set.contains(&fam))
                                .unwrap_or(false)
                        };
                        let initial_family = if already_failed_enc {
                            None
                        } else {
                            hwaccel::pick_encoder(
                                codec_snapshot,
                                mode_snapshot,
                                hwaccel::detect(),
                                hwaccel::cuda_runtime_loadable(),
                            )
                            .filter(|fam| !session_blacklisted_enc(*fam))
                        };
                        let already_failed_dec = hw_decode_failed_ref
                            .lock()
                            .map(|set| set.contains(&source_path))
                            .unwrap_or(false);
                        let initial_decode_hwaccel = if already_failed_dec {
                            None
                        } else {
                            hwaccel::pick_decode_hwaccel(mode_snapshot).filter(|method| {
                                !dec_blacklist_ref
                                    .lock()
                                    .map(|set| set.contains(*method))
                                    .unwrap_or(false)
                            })
                        };
                        let (proxy_path, success, owned_local) = transcode_proxy(
                            &source_path,
                            scale,
                            &lut_paths,
                            vidstab_enabled,
                            vidstab_smoothing,
                            &local_root,
                            &key,
                            &tx,
                            sidecar_mirror_enabled,
                            initial_family,
                            codec_snapshot,
                            &hw_failed_ref,
                            initial_decode_hwaccel,
                            &hw_decode_failed_ref,
                            &enc_blacklist_ref,
                            &dec_blacklist_ref,
                            &heavy_permit_ref,
                        );
                        if tx
                            .send(ProxyWorkerUpdate::Done(ProxyResult {
                                cache_key: key,
                                proxy_path,
                                success,
                                owned_local,
                            }))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            });
        }

        Self {
            proxies: HashMap::new(),
            pending: HashSet::new(),
            failed: HashSet::new(),
            total_requested: 0,
            result_rx,
            work_tx: Some(work_tx),
            estimated_bytes: HashMap::new(),
            written_bytes: HashMap::new(),
            local_cache_root,
            ffprobe_path: find_ffprobe_path(),
            runtime_owned_local_files: HashSet::new(),
            sidecar_mirror_enabled: false,
            hw_encoder_mode,
            proxy_codec,
            hw_failed_sources,
            hw_decode_failed_sources,
            hw_encoder_blacklist,
            hw_decode_blacklist,
            heavy_permit,
        }
    }

    /// Update the proxy codec preference. Applies to *future* worker jobs.
    pub fn set_proxy_codec(&self, codec: ProxyCodec) {
        if let Ok(mut guard) = self.proxy_codec.write() {
            *guard = codec;
        }
        // Switching codec invalidates HW failure caches because a source
        // that failed under (h264, nvenc) might succeed under (hevc, nvenc).
        self.clear_hw_failure_state();
    }

    pub fn set_sidecar_mirror_enabled(&mut self, enabled: bool) {
        self.sidecar_mirror_enabled = enabled;
    }

    /// Update the hardware encoder preference. Applies to *future* worker
    /// jobs; in-flight transcodes keep the mode they were started with.
    pub fn set_hw_encoder_mode(&self, mode: HwEncoderMode) {
        if let Ok(mut guard) = self.hw_encoder_mode.write() {
            *guard = mode;
        }
        // Forget previous HW failures when the user changes mode — fresh
        // mode might succeed on sources that failed under the old one
        // (e.g. switching Vaapi → Nvenc after a missing /dev/dri/renderD128).
        self.clear_hw_failure_state();
    }

    /// Clear all per-session HW failure caches and process-global
    /// blacklists. Called whenever the user changes a preference that
    /// could turn a previously-broken HW path into a working one.
    fn clear_hw_failure_state(&self) {
        if let Ok(mut set) = self.hw_failed_sources.lock() {
            set.clear();
        }
        if let Ok(mut set) = self.hw_decode_failed_sources.lock() {
            set.clear();
        }
        if let Ok(mut set) = self.hw_encoder_blacklist.lock() {
            set.clear();
        }
        if let Ok(mut set) = self.hw_decode_blacklist.lock() {
            set.clear();
        }
    }

    /// Enqueue a proxy transcode for `source_path` (with optional LUT paths).
    /// No-op if already cached or pending for this exact (source, lut, scale) combination.
    pub fn request(&mut self, source_path: &str, scale: ProxyScale, lut_path: Option<&str>) {
        self.request_with_vidstab(source_path, scale, lut_path, false, 0.0);
    }

    /// Enqueue a proxy transcode with optional vidstab stabilization baked in.
    pub fn request_with_vidstab(
        &mut self,
        source_path: &str,
        scale: ProxyScale,
        lut_path: Option<&str>,
        vidstab_enabled: bool,
        vidstab_smoothing: f32,
    ) {
        // Snapshot the codec preference up front so the cache key, the
        // disk-check, and the work-queue message all agree even if the
        // user changes the preference between this call and the worker
        // dequeue.
        let codec_snapshot = self
            .proxy_codec
            .read()
            .map(|g| *g)
            .unwrap_or(ProxyCodec::H264);
        let key = proxy_key_with_codec_and_vidstab(
            source_path,
            lut_path,
            codec_snapshot,
            vidstab_enabled,
            vidstab_smoothing,
        );
        if self.proxies.contains_key(&key)
            || self.pending.contains(&key)
            || self.failed.contains(&key)
        {
            return;
        }
        // Check for pre-existing proxy on disk before spawning work.
        if let Some(p) = existing_proxy_path_for(
            source_path,
            scale,
            lut_path,
            codec_snapshot,
            vidstab_enabled,
            vidstab_smoothing,
            &self.local_cache_root,
            self.ffprobe_path.as_deref(),
        ) {
            self.proxies.insert(key, p);
            return;
        }
        self.pending.insert(key.clone());
        self.total_requested += 1;
        log::info!(
            "ProxyCache: enqueuing proxy for {} (scale={:?}, codec={})",
            source_path,
            scale,
            codec_snapshot.as_str()
        );
        self.written_bytes.insert(key, 0);
        if let Some(ref tx) = self.work_tx {
            // Split composite key back into individual paths for the worker.
            let lut_paths: Vec<String> = match lut_path {
                Some(composite) if !composite.is_empty() => {
                    composite.split('|').map(|s| s.to_string()).collect()
                }
                _ => Vec::new(),
            };
            let _ = tx.send((
                source_path.to_string(),
                scale,
                lut_paths,
                self.sidecar_mirror_enabled,
                vidstab_enabled,
                vidstab_smoothing,
                codec_snapshot,
            ));
        }
    }

    /// Drain completed background transcodes. Returns cache keys that were just resolved.
    pub fn poll(&mut self) -> Vec<String> {
        let mut resolved = Vec::new();
        while let Ok(update) = self.result_rx.try_recv() {
            match update {
                ProxyWorkerUpdate::Progress {
                    cache_key,
                    written_bytes,
                    estimated_bytes,
                } => {
                    self.written_bytes.insert(cache_key.clone(), written_bytes);
                    if estimated_bytes > 0 {
                        self.estimated_bytes.insert(cache_key, estimated_bytes);
                    }
                }
                ProxyWorkerUpdate::Done(result) => {
                    self.pending.remove(&result.cache_key);
                    if result.success {
                        log::info!("ProxyCache: proxy ready → {}", result.proxy_path);
                        self.proxies
                            .insert(result.cache_key.clone(), result.proxy_path.clone());
                        if let Some(estimate) = self.estimated_bytes.get(&result.cache_key).copied()
                        {
                            self.written_bytes
                                .insert(result.cache_key.clone(), estimate);
                        }
                        if result.owned_local {
                            self.runtime_owned_local_files
                                .insert(result.proxy_path.clone());
                            append_owned_entry(&self.local_cache_root, &result.proxy_path);
                        }
                    } else {
                        self.failed.insert(result.cache_key.clone());
                        self.estimated_bytes.remove(&result.cache_key);
                        self.written_bytes.remove(&result.cache_key);
                    }
                    resolved.push(result.cache_key);
                }
            }
        }
        resolved
    }

    /// Get the proxy path for a (source, lut) pair, if transcoded.
    pub fn get(&self, source_path: &str, lut_path: Option<&str>) -> Option<&String> {
        self.proxies.get(&proxy_key(source_path, lut_path))
    }

    /// Return the set of source paths that have at least one ready
    /// proxy entry in this cache. Recovers the source-path prefix by
    /// splitting each composite key on the first `|` (the separator
    /// used by `proxy_key` for LUT/vidstab variant suffixes). Useful
    /// for UI surfaces that want to flag "has proxy" without
    /// threading the full cache map through — e.g. the timeline
    /// widget's Proxy badge.
    pub fn ready_source_paths(&self) -> HashSet<String> {
        self.proxies
            .keys()
            .map(|k| match k.split_once('|') {
                Some((src, _)) => src.to_string(),
                None => k.clone(),
            })
            .collect()
    }

    /// Clear all in-memory cached and pending entries (disk files are preserved).
    /// Call this when the proxy scale changes so clips are re-requested at the new size.
    pub fn invalidate_all(&mut self) {
        self.proxies.clear();
        self.pending.clear();
        self.failed.clear();
        self.total_requested = 0;
        self.estimated_bytes.clear();
        self.written_bytes.clear();
    }

    /// Remove managed local proxy cache files on project unload/close.
    /// - For `$XDG_CACHE_HOME` managed roots, clears local managed files referenced
    ///   by this project/session.
    /// - For `/tmp` managed roots, also clears tracked project/session proxy files
    ///   under the managed root.
    pub fn cleanup_local_cache_for_unload(&mut self) {
        let mut candidates: HashSet<String> = HashSet::new();
        candidates.extend(
            self.proxies
                .values()
                .filter(|p| is_path_within_root(p, &self.local_cache_root))
                .cloned(),
        );
        candidates.extend(self.runtime_owned_local_files.iter().cloned());
        if candidates.is_empty() {
            return;
        }
        let mut removed: HashSet<String> = HashSet::new();
        for path in &candidates {
            if !is_path_within_root(path, &self.local_cache_root) {
                continue;
            }
            match std::fs::remove_file(path) {
                Ok(_) => {
                    remove_proxy_source_signature(path);
                    removed.insert(path.clone());
                }
                Err(_) => {
                    if !Path::new(path).exists() {
                        remove_proxy_source_signature(path);
                        removed.insert(path.clone());
                    }
                }
            }
        }
        if removed.is_empty() {
            return;
        }
        remove_owned_entries(&self.local_cache_root, &removed);
        self.runtime_owned_local_files
            .retain(|p| !removed.contains(p));
        self.proxies.retain(|_, p| !removed.contains(p));
        log::info!(
            "ProxyCache: removed {} managed cache file(s) on project unload/close",
            removed.len()
        );
    }

    /// Remove alongside-media proxy files (`.../UltimateSlice.cache/*.proxy_*.mp4`)
    /// tracked by this session/project and prune emptied sidecar cache dirs.
    pub fn cleanup_sidecar_cache_for_unload(&mut self) {
        let candidates: HashSet<String> = self
            .proxies
            .values()
            .filter(|p| is_sidecar_proxy_path(p))
            .cloned()
            .collect();
        if candidates.is_empty() {
            return;
        }
        let mut removed: HashSet<String> = HashSet::new();
        let mut touched_dirs: HashSet<PathBuf> = HashSet::new();
        for path in &candidates {
            match std::fs::remove_file(path) {
                Ok(_) => {
                    remove_proxy_source_signature(path);
                    removed.insert(path.clone());
                }
                Err(_) => {
                    if !Path::new(path).exists() {
                        remove_proxy_source_signature(path);
                        removed.insert(path.clone());
                    }
                }
            }
            if let Some(parent) = Path::new(path).parent() {
                touched_dirs.insert(parent.to_path_buf());
            }
        }
        if removed.is_empty() {
            return;
        }
        self.proxies.retain(|_, p| !removed.contains(p));
        for dir in touched_dirs {
            let _ = std::fs::remove_dir(dir);
        }
        log::info!(
            "ProxyCache: removed {} alongside-media cache file(s) on project unload/close",
            removed.len()
        );
    }

    /// Cleanup policy for project unload/close:
    /// - always clear managed local/tmp cache files tracked by this session/project.
    /// - when `preserve_sidecar_proxies` is true, keep alongside-media
    ///   `UltimateSlice.cache` files.
    /// - otherwise, clear tracked alongside-media proxy files too.
    pub fn cleanup_for_unload(&mut self, preserve_sidecar_proxies: bool) {
        self.cleanup_local_cache_for_unload();
        if preserve_sidecar_proxies {
            log::info!("ProxyCache: preserving alongside-media proxy cache on unload/close");
            return;
        }
        self.cleanup_sidecar_cache_for_unload();
    }

    /// Remove invalid or superseded proxy variants for the current project's
    /// desired proxy set. This only touches current-format proxy files that can
    /// be matched back to one of the provided source paths.
    pub fn cleanup_stale_variants(
        &mut self,
        desired_variants: &[ProxyVariantSpec],
    ) -> ProxyCleanupSummary {
        if desired_variants.is_empty() {
            return ProxyCleanupSummary::default();
        }
        let desired_local_paths: HashSet<String> = desired_variants
            .iter()
            .filter_map(|spec| local_proxy_path_for_spec(spec, &self.local_cache_root))
            .collect();
        let desired_sidecar_paths: HashSet<String> = desired_variants
            .iter()
            .filter_map(alongside_proxy_path_for_spec)
            .collect();
        let local_source_hashes: HashSet<u64> = desired_variants
            .iter()
            .map(ProxyVariantSpec::source_hash)
            .collect();
        let mut sidecar_dirs: HashMap<PathBuf, HashSet<u64>> = HashMap::new();
        for spec in desired_variants {
            if let Some(dir) = sidecar_proxy_dir_for(&spec.source_path) {
                sidecar_dirs
                    .entry(dir)
                    .or_default()
                    .insert(spec.source_hash());
            }
        }

        let removed_local = cleanup_proxy_dir_variants(
            &self.local_cache_root,
            &desired_local_paths,
            &local_source_hashes,
            self.ffprobe_path.as_deref(),
            false,
        );
        if !removed_local.is_empty() {
            remove_owned_entries(&self.local_cache_root, &removed_local);
        }

        let mut removed_sidecar: HashSet<String> = HashSet::new();
        for (dir, source_hashes) in sidecar_dirs {
            removed_sidecar.extend(cleanup_proxy_dir_variants(
                &dir,
                &desired_sidecar_paths,
                &source_hashes,
                self.ffprobe_path.as_deref(),
                true,
            ));
        }

        let mut removed_any = removed_local.clone();
        removed_any.extend(removed_sidecar.iter().cloned());
        if !removed_any.is_empty() {
            self.runtime_owned_local_files
                .retain(|path| !removed_local.contains(path));
            self.proxies.retain(|_, path| !removed_any.contains(path));
            log::info!(
                "ProxyCache: removed {} stale/unneeded proxy file(s) for current project ({} local, {} sidecar)",
                removed_any.len(),
                removed_local.len(),
                removed_sidecar.len()
            );
        }

        ProxyCleanupSummary {
            removed_local: removed_local.len(),
            removed_sidecar: removed_sidecar.len(),
        }
    }

    /// Current progress snapshot.
    pub fn progress(&self) -> ProxyProgress {
        let completed = self.proxies.len();
        let mut written = 0u64;
        let mut estimated = 0u64;
        for (key, est) in &self.estimated_bytes {
            if *est == 0 {
                continue;
            }
            estimated = estimated.saturating_add(*est);
            let bytes = self.written_bytes.get(key).copied().unwrap_or(0);
            written = written.saturating_add(bytes.min(*est));
        }
        let in_flight = !self.pending.is_empty();
        let byte_fraction = if estimated > 0 {
            let mut frac = (written as f64 / estimated as f64).clamp(0.0, 1.0);
            if in_flight {
                frac = frac.min(0.99);
            }
            Some(frac)
        } else {
            None
        };
        ProxyProgress {
            total: self.total_requested,
            completed: completed.min(self.total_requested),
            in_flight,
            byte_fraction,
        }
    }
}

/// Compute local managed cache root for proxies.
/// Prefers `$XDG_CACHE_HOME/ultimateslice/proxies`, falls back to `/tmp/ultimateslice/proxies`.
fn local_proxy_root() -> PathBuf {
    std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(|v| PathBuf::from(v).join("ultimateslice").join("proxies"))
        .unwrap_or_else(|| PathBuf::from("/tmp/ultimateslice/proxies"))
}

pub fn local_proxy_cache_dir() -> PathBuf {
    local_proxy_root()
}

pub fn sidecar_proxy_usage_for_sources(source_paths: &HashSet<String>) -> ProxySidecarUsage {
    let (directories, relevant_source_hashes) =
        collect_sidecar_proxy_dirs_and_source_hashes(source_paths);
    let mut usage = ProxySidecarUsage {
        directories: directories
            .iter()
            .filter(|dir| dir.exists())
            .cloned()
            .collect(),
        ..ProxySidecarUsage::default()
    };
    for dir in &directories {
        let (file_count, size_bytes) =
            proxy_cache_artifact_usage_for_dir(dir, &relevant_source_hashes);
        usage.file_count += file_count;
        usage.size_bytes += size_bytes;
    }
    usage
}

pub fn purge_sidecar_proxy_cache_for_sources(
    source_paths: &HashSet<String>,
) -> Result<ProxySidecarUsage, String> {
    let usage = sidecar_proxy_usage_for_sources(source_paths);
    let (directories, relevant_source_hashes) =
        collect_sidecar_proxy_dirs_and_source_hashes(source_paths);
    for dir in &directories {
        purge_proxy_cache_artifacts_for_dir(dir, &relevant_source_hashes)?;
    }
    Ok(usage)
}

fn ownership_index_path(root: &Path) -> PathBuf {
    root.join("ownership-index-v1.txt")
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn system_time_epoch_parts(time: SystemTime) -> Option<(u64, u32)> {
    let duration = time.duration_since(UNIX_EPOCH).ok()?;
    Some((duration.as_secs(), duration.subsec_nanos()))
}

fn path_modified_epoch_parts(path: &str) -> Option<(u64, u32)> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    system_time_epoch_parts(modified)
}

fn source_signature(source_path: &str) -> Option<SourceSignature> {
    let meta = std::fs::metadata(source_path).ok()?;
    let (modified_secs, modified_nanos) = system_time_epoch_parts(meta.modified().ok()?)?;
    Some(SourceSignature {
        len: meta.len(),
        modified_secs,
        modified_nanos,
    })
}

fn proxy_source_signature_path(proxy_path: &str) -> String {
    format!("{proxy_path}.source-meta")
}

fn read_proxy_source_signature(proxy_path: &str) -> Option<SourceSignature> {
    let payload = std::fs::read_to_string(proxy_source_signature_path(proxy_path)).ok()?;
    let mut parts = payload.trim().split('|');
    Some(SourceSignature {
        len: parts.next()?.parse().ok()?,
        modified_secs: parts.next()?.parse().ok()?,
        modified_nanos: parts.next()?.parse().ok()?,
    })
}

fn finalize_output_file(temp_path: &str, final_path: &str) -> bool {
    if std::fs::rename(temp_path, final_path).is_ok() {
        return true;
    }
    if Path::new(final_path).exists() {
        let _ = std::fs::remove_file(final_path);
        if std::fs::rename(temp_path, final_path).is_ok() {
            return true;
        }
    }
    false
}

fn remove_proxy_source_signature(proxy_path: &str) {
    let signature_path = proxy_source_signature_path(proxy_path);
    let _ = std::fs::remove_file(&signature_path);
    let _ = std::fs::remove_file(format!("{signature_path}.partial"));
}

fn write_proxy_source_signature(proxy_path: &str, source_path: &str) -> bool {
    let Some(signature) = source_signature(source_path) else {
        return false;
    };
    let signature_path = proxy_source_signature_path(proxy_path);
    let temp_signature_path = format!("{signature_path}.partial");
    let _ = std::fs::remove_file(&temp_signature_path);
    let payload = format!(
        "{}|{}|{}\n",
        signature.len, signature.modified_secs, signature.modified_nanos
    );
    if std::fs::write(&temp_signature_path, payload).is_err() {
        let _ = std::fs::remove_file(&temp_signature_path);
        return false;
    }
    if !finalize_output_file(&temp_signature_path, &signature_path) {
        let _ = std::fs::remove_file(&temp_signature_path);
        return false;
    }
    true
}

fn proxy_matches_current_source(proxy_path: &str, source_path: &str) -> bool {
    let Some(current_signature) = source_signature(source_path) else {
        return true;
    };
    if let Some(stored_signature) = read_proxy_source_signature(proxy_path) {
        return stored_signature == current_signature;
    }
    path_modified_epoch_parts(proxy_path)
        .map(|proxy_modified| {
            (
                current_signature.modified_secs,
                current_signature.modified_nanos,
            ) <= proxy_modified
        })
        .unwrap_or(true)
}

fn is_path_within_root(path: &str, root: &Path) -> bool {
    Path::new(path).starts_with(root)
}

fn is_sidecar_proxy_path(path: &str) -> bool {
    let p = Path::new(path);
    p.parent()
        .and_then(|dir| dir.file_name())
        .and_then(|name| name.to_str())
        == Some("UltimateSlice.cache")
}

fn read_owned_entries(root: &Path) -> Vec<(u64, String)> {
    let idx = ownership_index_path(root);
    let Ok(data) = std::fs::read_to_string(idx) else {
        return Vec::new();
    };
    data.lines()
        .filter_map(|line| {
            let (ts, path) = line.split_once('|')?;
            let created_at = ts.parse::<u64>().ok()?;
            Some((created_at, path.to_string()))
        })
        .collect()
}

fn write_owned_entries(root: &Path, entries: &[(u64, String)]) {
    if std::fs::create_dir_all(root).is_err() {
        return;
    }
    let payload = entries
        .iter()
        .map(|(ts, path)| format!("{ts}|{path}\n"))
        .collect::<String>();
    let _ = std::fs::write(ownership_index_path(root), payload);
}

fn append_owned_entry(root: &Path, path: &str) {
    if !is_path_within_root(path, root) {
        return;
    }
    let mut entries = read_owned_entries(root);
    if entries.iter().any(|(_, p)| p == path) {
        return;
    }
    entries.push((now_epoch_secs(), path.to_string()));
    write_owned_entries(root, &entries);
}

fn remove_owned_entries(root: &Path, removed_paths: &HashSet<String>) {
    if removed_paths.is_empty() {
        return;
    }
    let kept: Vec<(u64, String)> = read_owned_entries(root)
        .into_iter()
        .filter(|(_, path)| !removed_paths.contains(path))
        .collect();
    write_owned_entries(root, &kept);
}

fn prune_stale_owned_entries(root: &Path, max_age_secs: u64) -> usize {
    let now = now_epoch_secs();
    let mut removed_count = 0usize;
    let mut kept: Vec<(u64, String)> = Vec::new();
    for (created_at, path) in read_owned_entries(root) {
        let stale = now.saturating_sub(created_at) > max_age_secs;
        if stale && is_path_within_root(&path, root) {
            let _ = std::fs::remove_file(&path);
            remove_proxy_source_signature(&path);
            removed_count += 1;
            continue;
        }
        if Path::new(&path).exists() {
            kept.push((created_at, path));
        }
    }
    write_owned_entries(root, &kept);
    removed_count
}

fn proxy_source_hash_from_path(path: &Path) -> Option<u64> {
    let file_name = path.file_name()?.to_str()?;
    let file_name = file_name
        .strip_suffix(".source-meta.partial")
        .or_else(|| file_name.strip_suffix(".source-meta"))
        .unwrap_or(file_name);
    let base_name = file_name.strip_suffix(".partial").unwrap_or(file_name);
    let no_ext = base_name.strip_suffix(".mp4")?;
    let (prefix, _) = no_ext.rsplit_once(".proxy_")?;
    let (stem_and_source, _) = prefix.rsplit_once("-v")?;
    let (_, source_hash_hex) = stem_and_source.rsplit_once("-s")?;
    u64::from_str_radix(source_hash_hex, 16).ok()
}

fn is_proxy_cache_artifact_name(file_name: &str) -> bool {
    file_name.contains(".proxy_")
        && (file_name.ends_with(".mp4")
            || file_name.ends_with(".mp4.partial")
            || file_name.ends_with(".mp4.source-meta")
            || file_name.ends_with(".mp4.source-meta.partial"))
}

fn collect_sidecar_proxy_dirs_and_source_hashes(
    source_paths: &HashSet<String>,
) -> (Vec<PathBuf>, HashSet<u64>) {
    let mut directories: HashSet<PathBuf> = HashSet::new();
    let mut relevant_source_hashes: HashSet<u64> = HashSet::new();
    for source_path in source_paths {
        if source_path.is_empty() {
            continue;
        }
        relevant_source_hashes.insert(source_path_hash(source_path));
        if let Some(dir) = sidecar_proxy_dir_for(source_path) {
            directories.insert(dir);
        }
    }
    let mut directories: Vec<PathBuf> = directories.into_iter().collect();
    directories.sort_unstable();
    (directories, relevant_source_hashes)
}

fn proxy_cache_artifact_usage_for_dir(
    dir: &Path,
    relevant_source_hashes: &HashSet<u64>,
) -> (usize, u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut file_count = 0usize;
    let mut size_bytes = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if !is_proxy_cache_artifact_name(file_name) {
            continue;
        }
        let Some(source_hash) = proxy_source_hash_from_path(&path) else {
            continue;
        };
        if !relevant_source_hashes.contains(&source_hash) {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        file_count += 1;
        size_bytes = size_bytes.saturating_add(metadata.len());
    }
    (file_count, size_bytes)
}

fn purge_proxy_cache_artifacts_for_dir(
    dir: &Path,
    relevant_source_hashes: &HashSet<u64>,
) -> Result<(), String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if !is_proxy_cache_artifact_name(file_name) {
            continue;
        }
        let Some(source_hash) = proxy_source_hash_from_path(&path) else {
            continue;
        };
        if !relevant_source_hashes.contains(&source_hash) {
            continue;
        }
        if let Err(err) = std::fs::remove_file(&path) {
            if path.exists() {
                return Err(format!("failed to remove {}: {err}", path.display()));
            }
        }
    }
    let dir_empty = std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false);
    if dir_empty {
        std::fs::remove_dir(dir)
            .map_err(|err| format!("failed to remove {}: {err}", dir.display()))?;
    }
    Ok(())
}

fn cleanup_proxy_dir_variants(
    dir: &Path,
    desired_paths: &HashSet<String>,
    relevant_source_hashes: &HashSet<u64>,
    ffprobe_path: Option<&str>,
    prune_dir_when_empty: bool,
) -> HashSet<String> {
    let mut removed: HashSet<String> = HashSet::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return removed;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if file_name == "ownership-index-v1.txt" {
            continue;
        }
        let path_str = path.to_string_lossy().to_string();
        if file_name.ends_with(".partial") {
            let _ = std::fs::remove_file(&path);
            removed.insert(path_str);
            continue;
        }
        if !file_name.contains(".proxy_") || !file_name.ends_with(".mp4") {
            continue;
        }
        let Some(source_hash) = proxy_source_hash_from_path(&path) else {
            continue;
        };
        if !relevant_source_hashes.contains(&source_hash) {
            continue;
        }
        let should_remove =
            !proxy_file_is_ready(&path_str, ffprobe_path) || !desired_paths.contains(&path_str);
        if should_remove {
            let _ = std::fs::remove_file(&path);
            remove_proxy_source_signature(&path_str);
            removed.insert(path_str);
        }
    }
    if prune_dir_when_empty {
        let dir_empty = std::fs::read_dir(dir)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false);
        if dir_empty {
            let _ = std::fs::remove_dir(dir);
        }
    }
    removed
}

fn normalized_vidstab_smoothing_hundredths(vidstab_enabled: bool, vidstab_smoothing: f32) -> i32 {
    if !vidstab_enabled {
        0
    } else {
        (vidstab_smoothing.clamp(0.0, 1.0) * 100.0).round() as i32
    }
}

fn source_path_hash(source_path: &str) -> u64 {
    let mut hasher = crate::media::cache_key::CacheKeyHasher::new();
    hasher.add_source_path(source_path);
    hasher.finish()
}

fn legacy_lut_hash(lut_path: &str) -> u32 {
    let mut hasher = DefaultHasher::new();
    lut_path.hash(&mut hasher);
    hasher.finish() as u32
}

fn source_variant_hash(
    source_path: &str,
    scale: ProxyScale,
    lut_path: Option<&str>,
    codec: ProxyCodec,
    vidstab_enabled: bool,
    vidstab_smoothing: f32,
) -> u64 {
    let mut hasher = crate::media::cache_key::CacheKeyHasher::new();
    hasher
        .add_source_path(source_path)
        .add(scale)
        .add(lut_path.unwrap_or(""))
        .add(vidstab_enabled)
        .add(normalized_vidstab_smoothing_hundredths(
            vidstab_enabled,
            vidstab_smoothing,
        ));
    // Codec only participates when it deviates from the historical H.264
    // default. This keeps the variant hash stable for users who never
    // touch the new HEVC preference, so their existing cache survives.
    if matches!(codec, ProxyCodec::Hevc) {
        hasher.add(codec.as_str());
    }
    hasher.finish()
}

fn proxy_filename_for_variant(
    source_path: &str,
    scale: ProxyScale,
    lut_path: Option<&str>,
    codec: ProxyCodec,
    vidstab_enabled: bool,
    vidstab_smoothing: f32,
) -> Option<String> {
    let stem = Path::new(source_path).file_stem()?.to_str()?;
    let source_hash = source_path_hash(source_path);
    let variant_hash = source_variant_hash(
        source_path,
        scale,
        lut_path,
        codec,
        vidstab_enabled,
        vidstab_smoothing,
    );
    Some(format!(
        "{stem}-s{source_hash:016x}-v{variant_hash:016x}.proxy_{}.mp4",
        scale.suffix()
    ))
}

fn legacy_proxy_filename_for_variant(
    source_path: &str,
    scale: ProxyScale,
    lut_path: Option<&str>,
) -> Option<String> {
    let stem = Path::new(source_path).file_stem()?.to_str()?;
    let scale_suffix = scale.suffix();
    match lut_path {
        Some(lut) if !lut.is_empty() => Some(format!(
            "{stem}.proxy_{scale_suffix}_lut{:08x}.mp4",
            legacy_lut_hash(lut)
        )),
        _ => Some(format!("{stem}.proxy_{scale_suffix}.mp4")),
    }
}

/// Compute the proxy output path for a given source path, scale, and optional LUT,
/// stored under the managed local cache root.
fn local_proxy_path_for(
    source_path: &str,
    scale: ProxyScale,
    lut_path: Option<&str>,
    codec: ProxyCodec,
    vidstab_enabled: bool,
    vidstab_smoothing: f32,
    local_root: &Path,
) -> Option<String> {
    let filename = proxy_filename_for_variant(
        source_path,
        scale,
        lut_path,
        codec,
        vidstab_enabled,
        vidstab_smoothing,
    )?;
    Some(local_root.join(filename).to_string_lossy().into_owned())
}

fn legacy_local_proxy_path_for(
    source_path: &str,
    scale: ProxyScale,
    lut_path: Option<&str>,
    local_root: &Path,
) -> Option<String> {
    let filename = legacy_proxy_filename_for_variant(source_path, scale, lut_path)?;
    Some(local_root.join(filename).to_string_lossy().into_owned())
}

/// Compute the proxy output path beside the source media.
/// Pattern:
/// `<parent>/UltimateSlice.cache/<stem>-s<sourcehash>-v<varianthash>.proxy_<scale>.mp4`
fn alongside_proxy_path_for(
    source_path: &str,
    scale: ProxyScale,
    lut_path: Option<&str>,
    codec: ProxyCodec,
    vidstab_enabled: bool,
    vidstab_smoothing: f32,
) -> Option<String> {
    let src = Path::new(source_path);
    let parent = src.parent()?;
    let proxy_dir = parent.join("UltimateSlice.cache");
    let filename = proxy_filename_for_variant(
        source_path,
        scale,
        lut_path,
        codec,
        vidstab_enabled,
        vidstab_smoothing,
    )?;
    Some(proxy_dir.join(filename).to_string_lossy().into_owned())
}

fn legacy_alongside_proxy_path_for(
    source_path: &str,
    scale: ProxyScale,
    lut_path: Option<&str>,
) -> Option<String> {
    let src = Path::new(source_path);
    let parent = src.parent()?;
    let proxy_dir = parent.join("UltimateSlice.cache");
    let filename = legacy_proxy_filename_for_variant(source_path, scale, lut_path)?;
    Some(proxy_dir.join(filename).to_string_lossy().into_owned())
}

fn local_proxy_path_for_spec(spec: &ProxyVariantSpec, local_root: &Path) -> Option<String> {
    local_proxy_path_for(
        &spec.source_path,
        spec.scale,
        spec.lut_key(),
        spec.codec,
        spec.vidstab_enabled,
        spec.vidstab_smoothing(),
        local_root,
    )
}

fn sidecar_proxy_dir_for(source_path: &str) -> Option<PathBuf> {
    Path::new(source_path)
        .parent()
        .map(|parent| parent.join("UltimateSlice.cache"))
}

fn alongside_proxy_path_for_spec(spec: &ProxyVariantSpec) -> Option<String> {
    alongside_proxy_path_for(
        &spec.source_path,
        spec.scale,
        spec.lut_key(),
        spec.codec,
        spec.vidstab_enabled,
        spec.vidstab_smoothing(),
    )
}

fn existing_proxy_path_for(
    source_path: &str,
    scale: ProxyScale,
    lut_path: Option<&str>,
    codec: ProxyCodec,
    vidstab_enabled: bool,
    vidstab_smoothing: f32,
    local_root: &Path,
    ffprobe_path: Option<&str>,
) -> Option<String> {
    let candidate_paths = [
        local_proxy_path_for(
            source_path,
            scale,
            lut_path,
            codec,
            vidstab_enabled,
            vidstab_smoothing,
            local_root,
        ),
        alongside_proxy_path_for(
            source_path,
            scale,
            lut_path,
            codec,
            vidstab_enabled,
            vidstab_smoothing,
        ),
        legacy_local_proxy_path_for(source_path, scale, lut_path, local_root),
        legacy_alongside_proxy_path_for(source_path, scale, lut_path),
    ];
    for candidate in candidate_paths.into_iter().flatten() {
        if proxy_file_is_ready(&candidate, ffprobe_path)
            && proxy_matches_current_source(&candidate, source_path)
        {
            let _ = write_proxy_source_signature(&candidate, source_path);
            return Some(candidate);
        }
    }
    None
}

fn find_ffprobe_path() -> Option<String> {
    let ffmpeg = crate::media::export::find_ffmpeg().ok()?;
    let ffprobe = ffmpeg.replace("ffmpeg", "ffprobe");
    let usable = Command::new(&ffprobe)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if usable {
        Some(ffprobe)
    } else {
        None
    }
}

/// Run a quick `ffprobe` to read the source's first video-stream height.
/// Used to decide whether a transcode counts as a "heavy" job for the
/// HeavyPermit gate. Returns `None` on probe failure or unparseable output;
/// callers should treat unknown-height as light (don't acquire a permit).
fn probe_source_height(source_path: &str, ffprobe: &str) -> Option<u32> {
    let output = Command::new(ffprobe)
        .arg("-v")
        .arg("error")
        .arg("-select_streams")
        .arg("v:0")
        .arg("-show_entries")
        .arg("stream=height")
        .arg("-of")
        .arg("default=noprint_wrappers=1:nokey=1")
        .arg(source_path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .ok()
}

fn proxy_file_is_ready(path: &str, ffprobe_path: Option<&str>) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if meta.len() == 0 {
        return false;
    }
    let Some(ffprobe) = ffprobe_path else {
        return true;
    };
    let output = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=nokey=1:noprint_wrappers=1",
            path,
        ])
        .output();
    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let duration = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<f64>()
        .ok();
    duration.is_some_and(|d| d.is_finite() && d > 0.0)
}

#[allow(clippy::too_many_arguments)]
fn run_transcode_command(
    ffmpeg: &str,
    source_path: &str,
    proxy_path: &str,
    filter: &str,
    cache_key: &str,
    estimated_size_bytes: Option<u64>,
    progress_tx: &mpsc::SyncSender<ProxyWorkerUpdate>,
    hw_family: Option<HwEncoderFamily>,
    proxy_codec: ProxyCodec,
    hw_decode_method: Option<&str>,
) -> bool {
    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-progress")
        .arg("pipe:2")
        .arg("-nostats");
    // VA-API needs the hwaccel device declared *before* the input so the
    // decoded frames can be uploaded to the GPU later in the filter chain.
    // Use the render node we actually probed at startup (multi-GPU systems
    // can expose renderD129/130/etc. when /dev/dri/renderD128 isn't
    // accessible, and we already know this one opens cleanly).
    if matches!(hw_family, Some(HwEncoderFamily::Vaapi)) {
        if let Some(node) = hwaccel::detect().vaapi_render_node.as_ref() {
            cmd.arg("-vaapi_device").arg(node);
        }
    }
    // HW decode hint: tells FFmpeg to use the GPU/iGPU decoder for the
    // input. We deliberately do NOT set `-hwaccel_output_format`, so
    // decoded frames flow back to CPU memory and the existing software
    // filter chain (lanczos/lut3d/...) keeps working unchanged.
    //
    // SKIP `-hwaccel vaapi` regardless of encoder. Empirical finding on
    // Intel iGPUs (Xe2 / Lunar Lake): VA-API HEVC 10-bit decode is
    // *slower* than libavcodec SW decode on a modern multi-core CPU —
    // the iGPU path goes through a hybrid CPU+GPU implementation, and
    // the lut3d step in our proxy filter chain forces a GPU→CPU
    // roundtrip that wastes the work the GPU did. Measured on a 6:13
    // 5952×3968 10-bit HEVC source: 6:39 (sw decode + vaapi encode) vs
    // 14:12 (vaapi decode + vaapi encode), 2× regression. CUDA (NVDEC
    // on discrete NVIDIA) and QSV decode hints are still emitted —
    // their drivers handle the roundtrip more efficiently and NVDEC's
    // dedicated 10-bit HEVC silicon is genuinely faster than CPU.
    if let Some(method) = hw_decode_method {
        if method != "vaapi" {
            cmd.arg("-hwaccel").arg(method);
        }
    }
    cmd.arg("-i").arg(source_path);
    // VA-API requires explicit format conversion + hwupload at the tail of
    // the software filter chain so the encoder receives GPU surfaces.
    let effective_filter = match hw_family {
        Some(HwEncoderFamily::Vaapi) => format!("{filter},format=nv12,hwupload"),
        _ => filter.to_string(),
    };
    cmd.arg("-vf").arg(&effective_filter);
    match (hw_family, proxy_codec) {
        (Some(HwEncoderFamily::Vaapi), ProxyCodec::H264) => {
            cmd.arg("-c:v")
                .arg("h264_vaapi")
                // qp ~ CRF in spirit; 28 keeps proxy bitrate similar to libx264 CRF 28.
                .arg("-qp")
                .arg("28");
        }
        (Some(HwEncoderFamily::Vaapi), ProxyCodec::Hevc) => {
            cmd.arg("-c:v").arg("hevc_vaapi").arg("-qp").arg("28");
        }
        (Some(HwEncoderFamily::Nvenc), ProxyCodec::H264) => {
            cmd.arg("-c:v")
                .arg("h264_nvenc")
                .arg("-preset")
                .arg("p1")
                .arg("-tune")
                .arg("ll")
                .arg("-rc")
                .arg("constqp")
                .arg("-cq")
                .arg("28")
                .arg("-pix_fmt")
                .arg("yuv420p");
        }
        (Some(HwEncoderFamily::Nvenc), ProxyCodec::Hevc) => {
            cmd.arg("-c:v")
                .arg("hevc_nvenc")
                .arg("-preset")
                .arg("p1")
                .arg("-tune")
                .arg("ll")
                .arg("-rc")
                .arg("constqp")
                .arg("-cq")
                .arg("28")
                .arg("-pix_fmt")
                .arg("yuv420p");
        }
        (None, ProxyCodec::H264) => {
            cmd.arg("-c:v")
                .arg("libx264")
                .arg("-preset")
                .arg("ultrafast")
                .arg("-tune")
                .arg("fastdecode")
                .arg("-crf")
                .arg("28")
                .arg("-bf")
                .arg("0")
                .arg("-refs")
                .arg("1")
                .arg("-pix_fmt")
                .arg("yuv420p");
        }
        (None, ProxyCodec::Hevc) => {
            cmd.arg("-c:v")
                .arg("libx265")
                .arg("-preset")
                .arg("ultrafast")
                .arg("-x265-params")
                .arg("log-level=error")
                .arg("-crf")
                .arg("28")
                .arg("-pix_fmt")
                .arg("yuv420p");
        }
    }
    cmd.arg("-c:a")
        .arg("aac")
        .arg("-b:a")
        .arg("128k")
        .arg("-movflags")
        .arg("+faststart")
        .arg("-f")
        .arg("mp4")
        .arg(proxy_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let enc_label = match (hw_family, proxy_codec) {
        (Some(HwEncoderFamily::Nvenc), ProxyCodec::H264) => "h264_nvenc",
        (Some(HwEncoderFamily::Nvenc), ProxyCodec::Hevc) => "hevc_nvenc",
        (Some(HwEncoderFamily::Vaapi), ProxyCodec::H264) => "h264_vaapi",
        (Some(HwEncoderFamily::Vaapi), ProxyCodec::Hevc) => "hevc_vaapi",
        (None, ProxyCodec::H264) => "libx264",
        (None, ProxyCodec::Hevc) => "libx265",
    };
    let dec_label = match hw_decode_method {
        Some("vaapi") => {
            "sw (vaapi decode skipped — slower than CPU for HEVC 10-bit on Intel iGPUs)"
        }
        Some(method) => method,
        None => "sw",
    };
    log::info!(
        "ProxyCache: spawning ffmpeg for {} (decode={dec_label}, encode={enc_label})",
        source_path
    );
    let attempt_started = std::time::Instant::now();

    let Ok(mut child) = cmd.spawn() else {
        log::warn!(
            "ProxyCache: failed to spawn ffmpeg for {} (decode={dec_label}, encode={enc_label})",
            source_path
        );
        return false;
    };
    let Some(stderr) = child.stderr.take() else {
        let _ = child.wait();
        return false;
    };

    let estimate = estimated_size_bytes.unwrap_or(0);
    if estimate > 0 {
        let _ = progress_tx.send(ProxyWorkerUpdate::Progress {
            cache_key: cache_key.to_string(),
            written_bytes: 0,
            estimated_bytes: estimate,
        });
    }
    // Keep a small ring buffer of non-progress stderr lines so we can include
    // them in the failure log and tell HW-path errors apart from anything else.
    let mut recent_stderr: Vec<String> = Vec::with_capacity(8);
    for line in BufReader::new(stderr).lines().map_while(|r| r.ok()) {
        if let Some(v) = line.strip_prefix("total_size=") {
            if let Ok(bytes) = v.parse::<u64>() {
                let _ = progress_tx.send(ProxyWorkerUpdate::Progress {
                    cache_key: cache_key.to_string(),
                    written_bytes: bytes,
                    estimated_bytes: estimate,
                });
            }
            continue;
        }
        // Filter out the periodic ffmpeg `-progress pipe:2` key=value rows
        // (frame=, fps=, bitrate=, ...) so the buffer only retains real
        // diagnostics like decoder/encoder errors.
        if line.contains('=') && !line.contains(' ') {
            continue;
        }
        if !line.trim().is_empty() {
            if recent_stderr.len() == 8 {
                recent_stderr.remove(0);
            }
            recent_stderr.push(line);
        }
    }
    let success = matches!(child.wait(), Ok(s) if s.success());
    let elapsed = attempt_started.elapsed();
    if success {
        log::info!(
            "ProxyCache: ffmpeg ok for {} (decode={dec_label}, encode={enc_label}) in {:.1}s",
            source_path,
            elapsed.as_secs_f64()
        );
    } else {
        let tail = if recent_stderr.is_empty() {
            String::from("(no stderr captured)")
        } else {
            recent_stderr.join(" | ")
        };
        log::warn!(
            "ProxyCache: ffmpeg FAILED for {} (decode={dec_label}, encode={enc_label}) after {:.1}s: {}",
            source_path,
            elapsed.as_secs_f64(),
            tail
        );
    }
    success
}

fn estimate_proxy_output_bitrate_bps(scale: ProxyScale, has_lut: bool) -> u64 {
    // Estimate H.264 bitrate based on the proxy's pixel area, normalized against
    // 1080p (~3.2 Mbps for 16:9). The width is approximated as 16/9 * height
    // since MaxHeight preserves source aspect — close enough for an estimate.
    let base_video_bps = match scale {
        ProxyScale::MaxHeight(h) => {
            let height_f = h.max(1) as f64;
            let approx_pixels = height_f * height_f * (16.0 / 9.0);
            let pixel_scale = (approx_pixels / (1920.0 * 1080.0)).clamp(0.25, 4.0);
            (3_200_000f64 * pixel_scale).clamp(600_000.0, 12_000_000.0)
        }
    };
    let lut_scale = if has_lut { 1.1 } else { 1.0 };
    let video_bps = (base_video_bps * lut_scale) as u64;
    video_bps.saturating_add(128_000)
}

fn probe_duration_seconds(ffprobe: &str, source_path: &str) -> Option<f64> {
    let output = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=nokey=1:noprint_wrappers=1",
            source_path,
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let duration = text.trim().parse::<f64>().ok()?;
    if duration.is_finite() && duration > 0.0 {
        Some(duration)
    } else {
        None
    }
}

fn estimate_proxy_size_bytes(
    source_path: &str,
    scale: ProxyScale,
    lut_path: Option<&str>,
    ffmpeg: &str,
) -> Option<u64> {
    let ffprobe = ffmpeg.replace("ffmpeg", "ffprobe");
    let duration_secs = probe_duration_seconds(&ffprobe, source_path)?;
    let bitrate_bps =
        estimate_proxy_output_bitrate_bps(scale, lut_path.is_some_and(|p| !p.is_empty()));
    Some(((bitrate_bps as f64 * duration_secs) / 8.0).round() as u64)
}

fn mirror_local_proxy_to_sidecar(
    local_proxy_path: &str,
    sidecar_path: &str,
    source_path: &str,
    ffprobe: &str,
) {
    if sidecar_path == local_proxy_path {
        return;
    }
    let sidecar_dir = Path::new(sidecar_path).parent().unwrap_or(Path::new("."));
    if let Err(e) = std::fs::create_dir_all(sidecar_dir) {
        // Most common real cause: the source's parent directory isn't
        // writable — read-only mount (cinema RAW DIT drives often lock
        // the volume), network share without write perms, sandboxed app
        // paths, etc. Surface so users don't wonder why some sources
        // get an alongside-media `UltimateSlice.cache/` and others don't.
        log::warn!(
            "ProxyCache: cannot create alongside-media cache dir {} for {}: {}. \
             Proxy will live only in the managed local cache. Common causes: \
             read-only source media (mount as rw or import to a writable drive), \
             missing write permissions on the source directory, or sandboxed FS \
             that hides the source parent.",
            sidecar_dir.display(),
            source_path,
            e
        );
        return;
    }
    let temp_sidecar_path = format!("{sidecar_path}.partial");
    let _ = std::fs::remove_file(&temp_sidecar_path);
    if let Err(e) = std::fs::copy(local_proxy_path, &temp_sidecar_path) {
        log::warn!(
            "ProxyCache: failed to copy local proxy {} → {}: {}. \
             Proxy will live only in the managed local cache. \
             Common causes: target volume out of space, permission error, \
             quota exceeded.",
            local_proxy_path,
            temp_sidecar_path,
            e
        );
        let _ = std::fs::remove_file(&temp_sidecar_path);
        return;
    }
    if !proxy_file_is_ready(&temp_sidecar_path, Some(ffprobe)) {
        log::warn!(
            "ProxyCache: mirrored copy at {} failed ffprobe readiness check; \
             discarding alongside-media file (local proxy at {} is unaffected). \
             This usually indicates the source volume truncated or corrupted the \
             copy mid-write.",
            temp_sidecar_path,
            local_proxy_path
        );
        let _ = std::fs::remove_file(&temp_sidecar_path);
        return;
    }
    if !finalize_output_file(&temp_sidecar_path, sidecar_path) {
        log::warn!(
            "ProxyCache: failed to atomically rename mirrored proxy {} → {}; \
             alongside-media cache will not be populated for this source.",
            temp_sidecar_path,
            sidecar_path
        );
        let _ = std::fs::remove_file(&temp_sidecar_path);
        return;
    }
    let _ = write_proxy_source_signature(sidecar_path, source_path);
    log::debug!(
        "ProxyCache: mirrored local proxy to alongside-media cache path={}",
        sidecar_path
    );
}

/// Run ffmpeg to create a proxy file.
/// Returns `(proxy_path, success, owned_local)`.
#[allow(clippy::too_many_arguments)]
fn transcode_proxy(
    source_path: &str,
    scale: ProxyScale,
    lut_paths: &[String],
    vidstab_enabled: bool,
    vidstab_smoothing: f32,
    local_root: &Path,
    cache_key: &str,
    progress_tx: &mpsc::SyncSender<ProxyWorkerUpdate>,
    sidecar_mirror_enabled: bool,
    initial_hw_family: Option<HwEncoderFamily>,
    proxy_codec: ProxyCodec,
    hw_failed_sources: &Arc<Mutex<HashSet<String>>>,
    initial_decode_hwaccel: Option<&'static str>,
    hw_decode_failed_sources: &Arc<Mutex<HashSet<String>>>,
    hw_encoder_blacklist: &Arc<Mutex<HashSet<HwEncoderFamily>>>,
    hw_decode_blacklist: &Arc<Mutex<HashSet<String>>>,
    heavy_permit: &Arc<HeavyPermit>,
) -> (String, bool, bool) {
    let lut_composite = if lut_paths.is_empty() {
        None
    } else {
        Some(lut_paths.join("|"))
    };
    let lut_key = lut_composite.as_deref();
    let local_proxy_path = match local_proxy_path_for(
        source_path,
        scale,
        lut_key,
        proxy_codec,
        vidstab_enabled,
        vidstab_smoothing,
        local_root,
    ) {
        Some(p) => p,
        None => return (String::new(), false, false),
    };
    let sidecar_proxy_path = alongside_proxy_path_for(
        source_path,
        scale,
        lut_key,
        proxy_codec,
        vidstab_enabled,
        vidstab_smoothing,
    );

    let ffmpeg = match crate::media::export::find_ffmpeg() {
        Ok(f) => f,
        Err(_) => return (local_proxy_path, false, false),
    };
    let estimated_size_bytes = estimate_proxy_size_bytes(source_path, scale, lut_key, &ffmpeg);
    let ffprobe = ffmpeg.replace("ffmpeg", "ffprobe");

    // Heavy-job gate: throttle 4K+ transcodes to keep the worker pool from
    // saturating GPU/decoder resources. Held for the full transcode_proxy
    // duration so the slow decode dominates the held window. Sources whose
    // height we can't probe are treated as light (no permit).
    let _heavy_permit_guard = match probe_source_height(source_path, &ffprobe) {
        Some(h) if h >= HEAVY_SOURCE_HEIGHT_THRESHOLD => {
            log::info!(
                "ProxyCache: {} is heavy ({h}p), waiting for heavy-job permit",
                source_path
            );
            Some(heavy_permit.acquire())
        }
        _ => None,
    };

    // Build the -vf filter string: scale, then vidstab (if enabled), then LUT chain.
    let mut filter = scale.ffmpeg_scale_filter().to_string();

    // Run vidstab two-pass analysis and add transform filter if successful.
    let mut vidstab_trf_path: Option<String> = None;
    if vidstab_enabled && vidstab_smoothing > 0.0 {
        let trf = format!("/tmp/ultimateslice-proxy-vidstab-{:016x}.trf", {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            source_path.hash(&mut h);
            h.finish()
        });
        let shakiness = ((vidstab_smoothing * 10.0).round() as i32).clamp(1, 10);
        log::info!(
            "ProxyCache: running vidstab analysis for {} (shakiness={})",
            source_path,
            shakiness
        );
        let analysis_ok = std::process::Command::new(&ffmpeg)
            .arg("-y")
            .arg("-i")
            .arg(source_path)
            .arg("-vf")
            .arg(format!(
                "{},vidstabdetect=shakiness={shakiness}:result={trf}",
                scale.ffmpeg_scale_filter()
            ))
            .arg("-f")
            .arg("null")
            .arg("-")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if analysis_ok && Path::new(&trf).exists() {
            let smoothing = ((vidstab_smoothing * 30.0).round() as i32).clamp(1, 30);
            filter.push_str(&format!(
                ",vidstabtransform=input={trf}:smoothing={smoothing}:zoom=0:optzoom=1,unsharp=5:5:0.8:3:3:0.4"
            ));
            vidstab_trf_path = Some(trf);
        } else {
            log::warn!(
                "ProxyCache: vidstab analysis failed for {}, skipping stabilization",
                source_path
            );
            let _ = std::fs::remove_file(&trf);
        }
    }

    for lut in lut_paths {
        if !lut.is_empty() {
            let escaped = lut.replace('\\', "\\\\").replace(':', "\\:");
            filter.push_str(&format!(",lut3d={escaped}"));
        }
    }

    let attempts = sidecar_proxy_path
        .as_ref()
        .map(|p| vec![(local_proxy_path.clone(), true), (p.clone(), false)])
        .unwrap_or_else(|| vec![(local_proxy_path.clone(), true)]);

    for (proxy_path, owned_local) in attempts {
        let proxy_dir = Path::new(&proxy_path).parent().unwrap_or(Path::new("."));
        if std::fs::create_dir_all(proxy_dir).is_err() {
            if owned_local {
                log::info!(
                        "ProxyCache: local cache path unavailable, falling back to alongside-media cache for {}",
                        source_path
                    );
            }
            continue;
        }
        let temp_proxy_path = format!("{proxy_path}.partial");
        let _ = std::fs::remove_file(&temp_proxy_path);
        // Attempt 1: best-effort HW encode + HW decode (each independently
        // gated on availability + per-source failure history).
        let mut attempt_hw_enc = initial_hw_family;
        let mut attempt_hw_dec = initial_decode_hwaccel;
        let mut command_ok = run_transcode_command(
            &ffmpeg,
            source_path,
            &temp_proxy_path,
            &filter,
            cache_key,
            estimated_size_bytes,
            progress_tx,
            attempt_hw_enc,
            proxy_codec,
            attempt_hw_dec,
        );
        // Attempt 2: if HW encode was active, blame the encoder first
        // (more commonly fragile on 10-bit / unusual input) and retry with
        // libx264 while keeping the decoder choice.
        if !command_ok && attempt_hw_enc.is_some() {
            log::warn!(
                "ProxyCache: hw encoder ({}) failed for {}, falling back to libx264",
                attempt_hw_enc.map(|f| f.as_str()).unwrap_or("unknown"),
                source_path
            );
            if let Ok(mut set) = hw_failed_sources.lock() {
                set.insert(source_path.to_string());
            }
            // Process-global blacklist: this encoder family failed at
            // runtime, future per-source picks will skip it from the start.
            if let Some(fam) = attempt_hw_enc {
                if let Ok(mut set) = hw_encoder_blacklist.lock() {
                    if set.insert(fam) {
                        log::info!(
                            "ProxyCache: session-blacklisting hw encoder family `{}` after runtime failure",
                            fam.as_str()
                        );
                    }
                }
            }
            attempt_hw_enc = None;
            let _ = std::fs::remove_file(&temp_proxy_path);
            command_ok = run_transcode_command(
                &ffmpeg,
                source_path,
                &temp_proxy_path,
                &filter,
                cache_key,
                estimated_size_bytes,
                progress_tx,
                attempt_hw_enc,
                proxy_codec,
                attempt_hw_dec,
            );
        }
        // Attempt 3: if HW decode was active and we still failed, fall
        // back to fully-software decode + encode.
        if !command_ok && attempt_hw_dec.is_some() {
            log::warn!(
                "ProxyCache: hw decoder ({}) failed for {}, falling back to software decode",
                attempt_hw_dec.unwrap_or("unknown"),
                source_path
            );
            if let Ok(mut set) = hw_decode_failed_sources.lock() {
                set.insert(source_path.to_string());
            }
            if let Some(method) = attempt_hw_dec {
                if let Ok(mut set) = hw_decode_blacklist.lock() {
                    if set.insert(method.to_string()) {
                        log::info!(
                            "ProxyCache: session-blacklisting hw decoder `{method}` after runtime failure"
                        );
                    }
                }
            }
            attempt_hw_dec = None;
            let _ = std::fs::remove_file(&temp_proxy_path);
            command_ok = run_transcode_command(
                &ffmpeg,
                source_path,
                &temp_proxy_path,
                &filter,
                cache_key,
                estimated_size_bytes,
                progress_tx,
                attempt_hw_enc,
                proxy_codec,
                attempt_hw_dec,
            );
        }
        if command_ok {
            if !proxy_file_is_ready(&temp_proxy_path, Some(&ffprobe)) {
                let _ = std::fs::remove_file(&temp_proxy_path);
                continue;
            }
            if !finalize_output_file(&temp_proxy_path, &proxy_path) {
                let _ = std::fs::remove_file(&temp_proxy_path);
                continue;
            }
            if !proxy_file_is_ready(&proxy_path, Some(&ffprobe)) {
                let _ = std::fs::remove_file(&proxy_path);
                remove_proxy_source_signature(&proxy_path);
                continue;
            }
            let _ = write_proxy_source_signature(&proxy_path, source_path);
            if owned_local && sidecar_mirror_enabled {
                if let Some(sidecar_path) = sidecar_proxy_path.as_deref() {
                    mirror_local_proxy_to_sidecar(&proxy_path, sidecar_path, source_path, &ffprobe);
                }
            }
            if let Some(ref trf) = vidstab_trf_path {
                let _ = std::fs::remove_file(trf);
            }
            return (proxy_path, true, owned_local);
        }
        let _ = std::fs::remove_file(&temp_proxy_path);
        if owned_local {
            log::info!(
                "ProxyCache: local cache transcode failed, retrying alongside-media cache for {}",
                source_path
            );
        }
    }
    // Clean up vidstab .trf file if it was created.
    if let Some(ref trf) = vidstab_trf_path {
        let _ = std::fs::remove_file(trf);
    }
    (local_proxy_path, false, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_local_proxy_root() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "ultimateslice-proxy-local-test-{}-{}",
            std::process::id(),
            nanos
        ))
    }

    fn test_source_file(name: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "ultimateslice-proxy-source-test-{}-{}",
            std::process::id(),
            nanos
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(name);
        let _ = std::fs::write(&path, b"source");
        path.to_string_lossy().to_string()
    }

    fn test_sidecar_file_path() -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let unique = format!(
            "ultimateslice-proxy-cache-test-{}-{}",
            std::process::id(),
            nanos
        );
        let sidecar_dir = std::env::temp_dir()
            .join(unique)
            .join("UltimateSlice.cache");
        let _ = std::fs::create_dir_all(&sidecar_dir);
        let path = sidecar_dir.join("clip.proxy_half.mp4");
        let _ = std::fs::write(&path, b"test");
        path.to_string_lossy().to_string()
    }

    #[test]
    fn proxy_key_h264_default_matches_legacy_shape() {
        // Regression guard: existing on-disk caches were keyed without the
        // codec discriminator. proxy_key_with_vidstab (the historical
        // entry point) must keep producing the same shape so users who
        // never touch the codec preference don't get forced re-transcodes.
        let key_legacy = proxy_key_with_vidstab("/tmp/a.mp4", None, false, 0.0);
        let key_h264 =
            proxy_key_with_codec_and_vidstab("/tmp/a.mp4", None, ProxyCodec::H264, false, 0.0);
        assert_eq!(key_legacy, key_h264);
        assert_eq!(key_legacy, "/tmp/a.mp4");
    }

    #[test]
    fn proxy_key_codec_separates_h264_from_hevc_paths() {
        let key_h264 = proxy_key_with_codec_and_vidstab(
            "/tmp/a.mp4",
            Some("/luts/lut.cube"),
            ProxyCodec::H264,
            false,
            0.0,
        );
        let key_hevc = proxy_key_with_codec_and_vidstab(
            "/tmp/a.mp4",
            Some("/luts/lut.cube"),
            ProxyCodec::Hevc,
            false,
            0.0,
        );
        assert_ne!(
            key_h264, key_hevc,
            "switching codec must produce a distinct cache key"
        );
        // H.264 omits the codec suffix; HEVC adds `|c:hevc`.
        assert!(!key_h264.contains("|c:"));
        assert!(key_hevc.ends_with("|c:hevc"));

        // The on-disk path must also differ so the worker doesn't
        // overwrite an existing H.264 file with a fresh HEVC transcode
        // at the same location.
        let local_root = std::env::temp_dir();
        let path_h264 = local_proxy_path_for(
            "/tmp/a.mp4",
            ProxyScale::MaxHeight(1080),
            None,
            ProxyCodec::H264,
            false,
            0.0,
            &local_root,
        );
        let path_hevc = local_proxy_path_for(
            "/tmp/a.mp4",
            ProxyScale::MaxHeight(1080),
            None,
            ProxyCodec::Hevc,
            false,
            0.0,
            &local_root,
        );
        assert!(path_h264.is_some() && path_hevc.is_some());
        assert_ne!(path_h264, path_hevc);
    }

    #[test]
    fn max_height_filter_caps_height_preserves_aspect_and_avoids_upscale() {
        // -2 forces an even auto-computed width that preserves source aspect.
        // min(N,ih) clamps the height to the target only when source is taller.
        assert_eq!(
            ProxyScale::MaxHeight(1080).ffmpeg_scale_filter(),
            "scale=-2:'min(1080,ih)':flags=lanczos"
        );
        assert_eq!(
            ProxyScale::MaxHeight(640).ffmpeg_scale_filter(),
            "scale=-2:'min(640,ih)':flags=lanczos"
        );
    }

    #[test]
    fn max_height_suffix_uses_height_p_form() {
        // Suffix lands in the on-disk filename, e.g. clip-s....proxy_1080p.mp4.
        // Distinct heights must produce distinct suffixes so cache keys stay unique.
        let s1080 = ProxyScale::MaxHeight(1080);
        let s640 = ProxyScale::MaxHeight(640);
        assert_eq!(s1080.suffix(), "1080p");
        assert_eq!(s640.suffix(), "640p");
    }

    #[test]
    fn ready_source_paths_recovers_source_prefix_from_composite_keys() {
        let mut cache = ProxyCache::new();
        // Bare source-only key (no LUT, no vidstab)
        cache.proxies.insert(
            "/tmp/clip-a.mp4".to_string(),
            "/tmp/proxy-a.mp4".to_string(),
        );
        // Source + LUT variant
        cache.proxies.insert(
            "/tmp/clip-b.mp4|lut:/tmp/look.cube".to_string(),
            "/tmp/proxy-b.mp4".to_string(),
        );
        // Source + LUT + vidstab variant (same source as previous, should collapse)
        cache.proxies.insert(
            "/tmp/clip-b.mp4|lut:/tmp/look.cube|vs:0.50".to_string(),
            "/tmp/proxy-b-vs.mp4".to_string(),
        );
        // Source + vidstab only (no LUT)
        cache.proxies.insert(
            "/tmp/clip-c.mp4|vs:0.30".to_string(),
            "/tmp/proxy-c.mp4".to_string(),
        );

        let ready = cache.ready_source_paths();
        assert!(ready.contains("/tmp/clip-a.mp4"));
        assert!(ready.contains("/tmp/clip-b.mp4"));
        assert!(ready.contains("/tmp/clip-c.mp4"));
        assert_eq!(ready.len(), 3);
    }

    #[test]
    fn ready_source_paths_is_empty_when_no_proxies_ready() {
        let cache = ProxyCache::new();
        assert!(cache.ready_source_paths().is_empty());
    }

    #[test]
    fn sidecar_proxy_path_detection_matches_directory_name() {
        assert!(is_sidecar_proxy_path(
            "/tmp/project/UltimateSlice.cache/a.proxy_1080p.mp4"
        ));
        assert!(!is_sidecar_proxy_path(
            "/tmp/ultimateslice/proxies/a.proxy_1080p.mp4"
        ));
    }

    #[test]
    fn cleanup_for_unload_removes_sidecar_when_proxy_mode_disabled() {
        let sidecar_path = test_sidecar_file_path();
        let source_path = test_source_file("clip-a.mp4");
        assert!(write_proxy_source_signature(&sidecar_path, &source_path));
        let mut cache = ProxyCache::new();
        cache.proxies.insert("k".to_string(), sidecar_path.clone());
        cache.cleanup_for_unload(false);
        assert!(!Path::new(&sidecar_path).exists());
        assert!(!Path::new(&proxy_source_signature_path(&sidecar_path)).exists());
        if let Some(parent) = Path::new(&sidecar_path).parent().and_then(|p| p.parent()) {
            let _ = std::fs::remove_dir_all(parent);
        }
        if let Some(parent) = Path::new(&source_path).parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn cleanup_for_unload_preserves_sidecar_when_proxy_mode_enabled() {
        let sidecar_path = test_sidecar_file_path();
        let mut cache = ProxyCache::new();
        let local_path = cache
            .local_cache_root
            .join("cleanup-policy-test.proxy_1080p.mp4")
            .to_string_lossy()
            .to_string();
        let _ = std::fs::create_dir_all(&cache.local_cache_root);
        let _ = std::fs::write(&local_path, b"test");
        let source_path = test_source_file("clip-a.mp4");
        assert!(write_proxy_source_signature(&sidecar_path, &source_path));
        assert!(write_proxy_source_signature(&local_path, &source_path));
        cache
            .proxies
            .insert("side".to_string(), sidecar_path.clone());
        cache
            .proxies
            .insert("local".to_string(), local_path.clone());
        cache.cleanup_for_unload(true);
        assert!(Path::new(&sidecar_path).exists());
        assert!(Path::new(&proxy_source_signature_path(&sidecar_path)).exists());
        assert!(!Path::new(&local_path).exists());
        assert!(!Path::new(&proxy_source_signature_path(&local_path)).exists());
        if let Some(parent) = Path::new(&sidecar_path).parent().and_then(|p| p.parent()) {
            let _ = std::fs::remove_dir_all(parent);
        }
        if let Some(parent) = Path::new(&source_path).parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn proxy_paths_stay_stable_for_source_edits_but_change_for_variant_state() {
        let local_root = test_local_proxy_root();
        let _ = std::fs::create_dir_all(&local_root);
        let source_path = test_source_file("clip-a.mp4");
        let other_source_path = test_source_file("clip-b.mp4");
        let plain_local = local_proxy_path_for(
            &source_path,
            ProxyScale::MaxHeight(1080),
            None,
            ProxyCodec::H264,
            false,
            0.0,
            &local_root,
        )
        .unwrap();
        let stabilized_local = local_proxy_path_for(
            &source_path,
            ProxyScale::MaxHeight(1080),
            None,
            ProxyCodec::H264,
            true,
            0.45,
            &local_root,
        )
        .unwrap();
        let plain_sidecar = alongside_proxy_path_for(
            &source_path,
            ProxyScale::MaxHeight(1080),
            None,
            ProxyCodec::H264,
            false,
            0.0,
        )
        .unwrap();
        let stabilized_sidecar = alongside_proxy_path_for(
            &source_path,
            ProxyScale::MaxHeight(1080),
            None,
            ProxyCodec::H264,
            true,
            0.45,
        )
        .unwrap();
        let other_source_local = local_proxy_path_for(
            &other_source_path,
            ProxyScale::MaxHeight(1080),
            None,
            ProxyCodec::H264,
            false,
            0.0,
            &local_root,
        )
        .unwrap();
        assert_ne!(plain_local, stabilized_local);
        assert_ne!(plain_sidecar, stabilized_sidecar);
        assert_ne!(plain_local, other_source_local);

        let _ = std::fs::write(&source_path, b"source-modified-longer");
        let updated_local = local_proxy_path_for(
            &source_path,
            ProxyScale::MaxHeight(1080),
            None,
            ProxyCodec::H264,
            false,
            0.0,
            &local_root,
        )
        .unwrap();
        let updated_sidecar = alongside_proxy_path_for(
            &source_path,
            ProxyScale::MaxHeight(1080),
            None,
            ProxyCodec::H264,
            false,
            0.0,
        )
        .unwrap();
        assert_eq!(plain_local, updated_local);
        assert_eq!(plain_sidecar, updated_sidecar);

        if let Some(parent) = Path::new(&source_path).parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
        if let Some(parent) = Path::new(&other_source_path).parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
        let _ = std::fs::remove_dir_all(&local_root);
    }

    #[test]
    fn existing_proxy_path_uses_source_signature_to_reuse_or_invalidate() {
        let local_root = test_local_proxy_root();
        let _ = std::fs::create_dir_all(&local_root);
        let source_path = test_source_file("clip-a.mp4");
        let proxy_path = local_proxy_path_for(
            &source_path,
            ProxyScale::MaxHeight(1080),
            None,
            ProxyCodec::H264,
            false,
            0.0,
            &local_root,
        )
        .unwrap();
        if let Some(parent) = Path::new(&proxy_path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&proxy_path, b"proxy");
        assert!(write_proxy_source_signature(&proxy_path, &source_path));

        let reused = existing_proxy_path_for(
            &source_path,
            ProxyScale::MaxHeight(1080),
            None,
            ProxyCodec::H264,
            false,
            0.0,
            &local_root,
            None,
        );
        assert_eq!(reused.as_deref(), Some(proxy_path.as_str()));

        let _ = std::fs::write(&source_path, b"source-modified-longer");
        let stale = existing_proxy_path_for(
            &source_path,
            ProxyScale::MaxHeight(1080),
            None,
            ProxyCodec::H264,
            false,
            0.0,
            &local_root,
            None,
        );
        assert!(stale.is_none());

        if let Some(parent) = Path::new(&source_path).parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
        let _ = std::fs::remove_dir_all(&local_root);
    }

    #[test]
    fn existing_proxy_path_reuses_legacy_sidecar_proxy_names() {
        let local_root = test_local_proxy_root();
        let _ = std::fs::create_dir_all(&local_root);
        let source_path = test_source_file("clip-a.mp4");
        let legacy_sidecar =
            legacy_alongside_proxy_path_for(&source_path, ProxyScale::MaxHeight(1080), None)
                .unwrap();
        if let Some(parent) = Path::new(&legacy_sidecar).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&legacy_sidecar, b"proxy");

        let reused = existing_proxy_path_for(
            &source_path,
            ProxyScale::MaxHeight(1080),
            None,
            ProxyCodec::H264,
            false,
            0.0,
            &local_root,
            None,
        );
        assert_eq!(reused.as_deref(), Some(legacy_sidecar.as_str()));
        assert!(Path::new(&proxy_source_signature_path(&legacy_sidecar)).exists());

        if let Some(parent) = Path::new(&source_path).parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
        let _ = std::fs::remove_dir_all(&local_root);
    }

    #[test]
    fn cleanup_stale_variants_only_touches_current_source_hashes() {
        let local_root = test_local_proxy_root();
        let _ = std::fs::create_dir_all(&local_root);
        let source_a = test_source_file("clip-a.mp4");
        let source_b = test_source_file("clip-b.mp4");

        let keep_spec = ProxyVariantSpec::new(
            source_a.clone(),
            ProxyScale::MaxHeight(1080),
            None,
            false,
            0.0,
        );
        let remove_spec = ProxyVariantSpec::new(
            source_a.clone(),
            ProxyScale::MaxHeight(640),
            None,
            false,
            0.0,
        );
        let unrelated_spec = ProxyVariantSpec::new(
            source_b.clone(),
            ProxyScale::MaxHeight(640),
            None,
            false,
            0.0,
        );

        let keep_local = local_proxy_path_for_spec(&keep_spec, &local_root).unwrap();
        let remove_local = local_proxy_path_for_spec(&remove_spec, &local_root).unwrap();
        let unrelated_local = local_proxy_path_for_spec(&unrelated_spec, &local_root).unwrap();
        let keep_side = alongside_proxy_path_for_spec(&keep_spec).unwrap();
        let remove_side = alongside_proxy_path_for_spec(&remove_spec).unwrap();
        let unrelated_side = alongside_proxy_path_for_spec(&unrelated_spec).unwrap();

        for path in [
            &keep_local,
            &remove_local,
            &unrelated_local,
            &keep_side,
            &remove_side,
            &unrelated_side,
        ] {
            if let Some(parent) = Path::new(path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(path, b"proxy");
            let source_path = if path.contains("clip-a") {
                &source_a
            } else {
                &source_b
            };
            let _ = write_proxy_source_signature(path, source_path);
        }

        let mut cache = ProxyCache::new();
        cache.local_cache_root = local_root.clone();
        cache.ffprobe_path = None;
        let summary = cache.cleanup_stale_variants(&[keep_spec]);

        assert_eq!(summary.removed_local, 1);
        assert_eq!(summary.removed_sidecar, 1);
        assert!(Path::new(&keep_local).exists());
        assert!(!Path::new(&remove_local).exists());
        assert!(Path::new(&unrelated_local).exists());
        assert!(Path::new(&keep_side).exists());
        assert!(!Path::new(&remove_side).exists());
        assert!(Path::new(&unrelated_side).exists());
        assert!(Path::new(&proxy_source_signature_path(&keep_local)).exists());
        assert!(!Path::new(&proxy_source_signature_path(&remove_local)).exists());
        assert!(Path::new(&proxy_source_signature_path(&unrelated_local)).exists());
        assert!(Path::new(&proxy_source_signature_path(&keep_side)).exists());
        assert!(!Path::new(&proxy_source_signature_path(&remove_side)).exists());
        assert!(Path::new(&proxy_source_signature_path(&unrelated_side)).exists());

        if let Some(parent) = Path::new(&source_a).parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
        if let Some(parent) = Path::new(&source_b).parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
        let _ = std::fs::remove_dir_all(&local_root);
    }

    #[test]
    fn sidecar_proxy_usage_for_sources_counts_matching_proxy_artifacts() {
        let source_a = test_source_file("clip-a.mp4");
        let source_b = test_source_file("clip-b.mp4");
        let side_a = alongside_proxy_path_for(
            &source_a,
            ProxyScale::MaxHeight(1080),
            None,
            ProxyCodec::H264,
            false,
            0.0,
        )
        .unwrap();
        let side_b = alongside_proxy_path_for(
            &source_b,
            ProxyScale::MaxHeight(640),
            None,
            ProxyCodec::H264,
            false,
            0.0,
        )
        .unwrap();

        for (path, source_path) in [(&side_a, &source_a), (&side_b, &source_b)] {
            if let Some(parent) = Path::new(path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(path, b"proxy");
            assert!(write_proxy_source_signature(path, source_path));
        }

        let mut sources = HashSet::new();
        sources.insert(source_a.clone());
        let usage = sidecar_proxy_usage_for_sources(&sources);

        assert_eq!(usage.directories.len(), 1);
        assert_eq!(usage.file_count, 2);
        assert!(usage.size_bytes > 0);

        if let Some(parent) = Path::new(&source_a).parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
        if let Some(parent) = Path::new(&source_b).parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn purge_sidecar_proxy_cache_for_sources_only_removes_matching_artifacts() {
        let source_a = test_source_file("clip-a.mp4");
        let source_b = test_source_file("clip-b.mp4");
        let side_a = alongside_proxy_path_for(
            &source_a,
            ProxyScale::MaxHeight(1080),
            None,
            ProxyCodec::H264,
            false,
            0.0,
        )
        .unwrap();
        let side_b = alongside_proxy_path_for(
            &source_b,
            ProxyScale::MaxHeight(640),
            None,
            ProxyCodec::H264,
            false,
            0.0,
        )
        .unwrap();

        for (path, source_path) in [(&side_a, &source_a), (&side_b, &source_b)] {
            if let Some(parent) = Path::new(path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(path, b"proxy");
            assert!(write_proxy_source_signature(path, source_path));
        }

        let mut sources = HashSet::new();
        sources.insert(source_a.clone());
        let removed = purge_sidecar_proxy_cache_for_sources(&sources).expect("purge succeeds");

        assert_eq!(removed.file_count, 2);
        assert!(!Path::new(&side_a).exists());
        assert!(!Path::new(&proxy_source_signature_path(&side_a)).exists());
        assert!(Path::new(&side_b).exists());
        assert!(Path::new(&proxy_source_signature_path(&side_b)).exists());

        if let Some(parent) = Path::new(&source_a).parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
        if let Some(parent) = Path::new(&source_b).parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}
