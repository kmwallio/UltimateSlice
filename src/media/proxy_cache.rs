use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Scale factor for proxy transcodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProxyScale {
    Half,
    Quarter,
    Project { width: u32, height: u32 },
}

impl ProxyScale {
    pub fn ffmpeg_scale_filter(&self) -> String {
        match self {
            ProxyScale::Half => "scale=iw/2:ih/2".to_string(),
            ProxyScale::Quarter => "scale=iw/4:ih/4".to_string(),
            ProxyScale::Project { width, height } => format!(
                "scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2"
            ),
        }
    }

    fn suffix(&self) -> String {
        match self {
            ProxyScale::Half => "half".to_string(),
            ProxyScale::Quarter => "quarter".to_string(),
            ProxyScale::Project { width, height } => format!("proj{width}x{height}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProxyVariantSpec {
    pub source_path: String,
    pub scale: ProxyScale,
    pub lut_path: Option<String>,
    pub vidstab_enabled: bool,
    vidstab_smoothing_hundredths: i32,
}

impl ProxyVariantSpec {
    pub fn new(
        source_path: impl Into<String>,
        scale: ProxyScale,
        lut_path: Option<String>,
        vidstab_enabled: bool,
        vidstab_smoothing: f32,
    ) -> Self {
        Self {
            source_path: source_path.into(),
            scale,
            lut_path,
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

#[derive(Default)]
pub struct ProxyCleanupSummary {
    pub removed_local: usize,
    pub removed_sidecar: usize,
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

/// Extended composite key including vidstab stabilization state.
pub fn proxy_key_with_vidstab(
    source_path: &str,
    lut_path: Option<&str>,
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
    work_tx: Option<mpsc::Sender<(String, ProxyScale, Vec<String>, bool, bool, f32)>>,
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
        // (source_path, scale, lut_paths, sidecar_mirror, vidstab_enabled, vidstab_smoothing)
        let (work_tx, work_rx) =
            mpsc::channel::<(String, ProxyScale, Vec<String>, bool, bool, f32)>();

        // Pool of worker threads to transcode proxies in parallel.
        let work_rx = std::sync::Arc::new(std::sync::Mutex::new(work_rx));
        let num_workers = 4;
        for _ in 0..num_workers {
            let rx = work_rx.clone();
            let tx = result_tx.clone();
            let local_root = local_cache_root.clone();
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
                    )) => {
                        let lut_composite = if lut_paths.is_empty() {
                            None
                        } else {
                            Some(lut_paths.join("|"))
                        };
                        let key = proxy_key_with_vidstab(
                            &source_path,
                            lut_composite.as_deref(),
                            vidstab_enabled,
                            vidstab_smoothing,
                        );
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
        }
    }

    pub fn set_sidecar_mirror_enabled(&mut self, enabled: bool) {
        self.sidecar_mirror_enabled = enabled;
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
        let key = proxy_key_with_vidstab(source_path, lut_path, vidstab_enabled, vidstab_smoothing);
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
            vidstab_enabled,
            vidstab_smoothing,
            &self.local_cache_root,
            self.ffprobe_path.as_deref(),
        ) {
            self.proxies.insert(key, p);
            return;
        }
        self.pending.insert(key);
        self.total_requested += 1;
        log::info!(
            "ProxyCache: enqueuing proxy for {} (scale={:?})",
            source_path,
            scale
        );
        self.written_bytes.insert(
            proxy_key_with_vidstab(source_path, lut_path, vidstab_enabled, vidstab_smoothing),
            0,
        );
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
    /// - when proxy mode is enabled, preserve alongside-media `UltimateSlice.cache` files.
    /// - when proxy mode is disabled, clear alongside-media `UltimateSlice.cache` files too.
    pub fn cleanup_for_unload(&mut self, proxy_mode_enabled: bool) {
        self.cleanup_local_cache_for_unload();
        if proxy_mode_enabled {
            log::info!(
                "ProxyCache: preserving alongside-media proxy cache on unload/close because proxy mode is enabled"
            );
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
    let base_name = file_name.strip_suffix(".partial").unwrap_or(file_name);
    let no_ext = base_name.strip_suffix(".mp4")?;
    let (prefix, _) = no_ext.rsplit_once(".proxy_")?;
    let (stem_and_source, _) = prefix.rsplit_once("-v")?;
    let (_, source_hash_hex) = stem_and_source.rsplit_once("-s")?;
    u64::from_str_radix(source_hash_hex, 16).ok()
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
    let mut hasher = DefaultHasher::new();
    source_path.hash(&mut hasher);
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
    vidstab_enabled: bool,
    vidstab_smoothing: f32,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    source_path.hash(&mut hasher);
    scale.hash(&mut hasher);
    lut_path.unwrap_or("").hash(&mut hasher);
    vidstab_enabled.hash(&mut hasher);
    normalized_vidstab_smoothing_hundredths(vidstab_enabled, vidstab_smoothing).hash(&mut hasher);
    hasher.finish()
}

fn proxy_filename_for_variant(
    source_path: &str,
    scale: ProxyScale,
    lut_path: Option<&str>,
    vidstab_enabled: bool,
    vidstab_smoothing: f32,
) -> Option<String> {
    let stem = Path::new(source_path).file_stem()?.to_str()?;
    let source_hash = source_path_hash(source_path);
    let variant_hash = source_variant_hash(
        source_path,
        scale,
        lut_path,
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
    vidstab_enabled: bool,
    vidstab_smoothing: f32,
    local_root: &Path,
) -> Option<String> {
    let filename = proxy_filename_for_variant(
        source_path,
        scale,
        lut_path,
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
        spec.vidstab_enabled,
        spec.vidstab_smoothing(),
    )
}

fn existing_proxy_path_for(
    source_path: &str,
    scale: ProxyScale,
    lut_path: Option<&str>,
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
            vidstab_enabled,
            vidstab_smoothing,
            local_root,
        ),
        alongside_proxy_path_for(
            source_path,
            scale,
            lut_path,
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

fn run_transcode_command(
    ffmpeg: &str,
    source_path: &str,
    proxy_path: &str,
    filter: &str,
    cache_key: &str,
    estimated_size_bytes: Option<u64>,
    progress_tx: &mpsc::SyncSender<ProxyWorkerUpdate>,
) -> bool {
    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-progress")
        .arg("pipe:2")
        .arg("-nostats")
        .arg("-i")
        .arg(source_path)
        .arg("-vf")
        .arg(filter)
        .arg("-c:v")
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
        .arg("yuv420p")
        .arg("-c:a")
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

    let Ok(mut child) = cmd.spawn() else {
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
    for line in BufReader::new(stderr).lines().map_while(|r| r.ok()) {
        if let Some(v) = line.strip_prefix("total_size=") {
            if let Ok(bytes) = v.parse::<u64>() {
                let _ = progress_tx.send(ProxyWorkerUpdate::Progress {
                    cache_key: cache_key.to_string(),
                    written_bytes: bytes,
                    estimated_bytes: estimate,
                });
            }
        }
    }
    matches!(child.wait(), Ok(s) if s.success())
}

fn estimate_proxy_output_bitrate_bps(scale: ProxyScale, has_lut: bool) -> u64 {
    let base_video_bps = match scale {
        ProxyScale::Half => 1_600_000f64,
        ProxyScale::Quarter => 850_000f64,
        ProxyScale::Project { width, height } => {
            let pixel_scale =
                ((width.max(1) as f64 * height.max(1) as f64) / (1920.0 * 1080.0)).clamp(0.25, 4.0);
            (3_200_000f64 * pixel_scale).clamp(1_200_000.0, 12_000_000.0)
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
    if std::fs::create_dir_all(sidecar_dir).is_err() {
        return;
    }
    let temp_sidecar_path = format!("{sidecar_path}.partial");
    let _ = std::fs::remove_file(&temp_sidecar_path);
    if std::fs::copy(local_proxy_path, &temp_sidecar_path).is_err() {
        let _ = std::fs::remove_file(&temp_sidecar_path);
        return;
    }
    if !proxy_file_is_ready(&temp_sidecar_path, Some(ffprobe)) {
        let _ = std::fs::remove_file(&temp_sidecar_path);
        return;
    }
    if !finalize_output_file(&temp_sidecar_path, sidecar_path) {
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
        vidstab_enabled,
        vidstab_smoothing,
    );

    let ffmpeg = match crate::media::export::find_ffmpeg() {
        Ok(f) => f,
        Err(_) => return (local_proxy_path, false, false),
    };
    let estimated_size_bytes = estimate_proxy_size_bytes(source_path, scale, lut_key, &ffmpeg);
    let ffprobe = ffmpeg.replace("ffmpeg", "ffprobe");

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
        if run_transcode_command(
            &ffmpeg,
            source_path,
            &temp_proxy_path,
            &filter,
            cache_key,
            estimated_size_bytes,
            progress_tx,
        ) {
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
    fn sidecar_proxy_path_detection_matches_directory_name() {
        assert!(is_sidecar_proxy_path(
            "/tmp/project/UltimateSlice.cache/a.proxy_half.mp4"
        ));
        assert!(!is_sidecar_proxy_path(
            "/tmp/ultimateslice/proxies/a.proxy_half.mp4"
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
            .join("cleanup-policy-test.proxy_half.mp4")
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
            ProxyScale::Half,
            None,
            false,
            0.0,
            &local_root,
        )
        .unwrap();
        let stabilized_local = local_proxy_path_for(
            &source_path,
            ProxyScale::Half,
            None,
            true,
            0.45,
            &local_root,
        )
        .unwrap();
        let plain_sidecar =
            alongside_proxy_path_for(&source_path, ProxyScale::Half, None, false, 0.0).unwrap();
        let stabilized_sidecar =
            alongside_proxy_path_for(&source_path, ProxyScale::Half, None, true, 0.45).unwrap();
        let other_source_local = local_proxy_path_for(
            &other_source_path,
            ProxyScale::Half,
            None,
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
            ProxyScale::Half,
            None,
            false,
            0.0,
            &local_root,
        )
        .unwrap();
        let updated_sidecar =
            alongside_proxy_path_for(&source_path, ProxyScale::Half, None, false, 0.0).unwrap();
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
            ProxyScale::Half,
            None,
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
            ProxyScale::Half,
            None,
            false,
            0.0,
            &local_root,
            None,
        );
        assert_eq!(reused.as_deref(), Some(proxy_path.as_str()));

        let _ = std::fs::write(&source_path, b"source-modified-longer");
        let stale = existing_proxy_path_for(
            &source_path,
            ProxyScale::Half,
            None,
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
            legacy_alongside_proxy_path_for(&source_path, ProxyScale::Half, None).unwrap();
        if let Some(parent) = Path::new(&legacy_sidecar).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&legacy_sidecar, b"proxy");

        let reused = existing_proxy_path_for(
            &source_path,
            ProxyScale::Half,
            None,
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

        let keep_spec = ProxyVariantSpec::new(source_a.clone(), ProxyScale::Half, None, false, 0.0);
        let remove_spec =
            ProxyVariantSpec::new(source_a.clone(), ProxyScale::Quarter, None, false, 0.0);
        let unrelated_spec =
            ProxyVariantSpec::new(source_b.clone(), ProxyScale::Quarter, None, false, 0.0);

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
}
