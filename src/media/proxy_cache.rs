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
/// Proxy files are stored in a `UltimateSlice.cache/` directory next to
/// the source file.
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
        let (work_tx, work_rx) = mpsc::channel::<(String, ProxyScale, Vec<String>, bool, bool, f32)>();

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
                    Ok((source_path, scale, lut_paths, sidecar_mirror_enabled, vidstab_enabled, vidstab_smoothing)) => {
                        let lut_composite = if lut_paths.is_empty() { None } else { Some(lut_paths.join("|")) };
                        let key = proxy_key_with_vidstab(&source_path, lut_composite.as_deref(), vidstab_enabled, vidstab_smoothing);
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
        self.written_bytes
            .insert(proxy_key_with_vidstab(source_path, lut_path, vidstab_enabled, vidstab_smoothing), 0);
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
                        log::info!(
                            "ProxyCache: proxy ready → {}",
                            result.proxy_path
                        );
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
                    removed.insert(path.clone());
                }
                Err(_) => {
                    if !Path::new(path).exists() {
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
                    removed.insert(path.clone());
                }
                Err(_) => {
                    if !Path::new(path).exists() {
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

/// Compute a short hash suffix for a LUT path to embed in the proxy filename.
fn lut_hash(lut_path: &str) -> String {
    let mut h = DefaultHasher::new();
    lut_path.hash(&mut h);
    format!("{:08x}", h.finish() as u32)
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

fn source_identity_hash(source_path: &str, scale: ProxyScale, lut_path: Option<&str>) -> u64 {
    let mut hasher = DefaultHasher::new();
    source_path.hash(&mut hasher);
    scale.hash(&mut hasher);
    lut_path.unwrap_or("").hash(&mut hasher);
    if let Ok(meta) = std::fs::metadata(source_path) {
        meta.len().hash(&mut hasher);
        if let Ok(modified) = meta.modified() {
            if let Ok(ts) = modified.duration_since(UNIX_EPOCH) {
                ts.as_secs().hash(&mut hasher);
                ts.subsec_nanos().hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// Compute the proxy output path for a given source path, scale, and optional LUT,
/// stored under the managed local cache root.
fn local_proxy_path_for(
    source_path: &str,
    scale: ProxyScale,
    lut_path: Option<&str>,
    local_root: &Path,
) -> Option<String> {
    let stem = Path::new(source_path).file_stem()?.to_str()?;
    let fp = source_identity_hash(source_path, scale, lut_path);
    let filename = format!("{stem}-{fp:016x}.proxy_{}.mp4", scale.suffix());
    Some(local_root.join(filename).to_string_lossy().into_owned())
}

/// Compute the proxy output path beside the source media.
/// Pattern: `<parent>/UltimateSlice.cache/<stem>.proxy_<scale>[_lut<hash>].mp4`
fn alongside_proxy_path_for(
    source_path: &str,
    scale: ProxyScale,
    lut_path: Option<&str>,
) -> Option<String> {
    let src = Path::new(source_path);
    let parent = src.parent()?;
    let stem = src.file_stem()?.to_str()?;
    let proxy_dir = parent.join("UltimateSlice.cache");
    let lut_suffix = match lut_path {
        Some(lut) if !lut.is_empty() => format!("_lut{}", lut_hash(lut)),
        _ => String::new(),
    };
    let filename = format!("{}.proxy_{}{}.mp4", stem, scale.suffix(), lut_suffix);
    Some(proxy_dir.join(filename).to_string_lossy().into_owned())
}

fn existing_proxy_path_for(
    source_path: &str,
    scale: ProxyScale,
    lut_path: Option<&str>,
    local_root: &Path,
    ffprobe_path: Option<&str>,
) -> Option<String> {
    if let Some(local) = local_proxy_path_for(source_path, scale, lut_path, local_root) {
        if proxy_file_is_ready(&local, ffprobe_path) {
            return Some(local);
        }
    }
    if let Some(side) = alongside_proxy_path_for(source_path, scale, lut_path) {
        if proxy_file_is_ready(&side, ffprobe_path) {
            return Some(side);
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

fn mirror_local_proxy_to_sidecar(local_proxy_path: &str, sidecar_path: &str, ffprobe: &str) {
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
    if std::fs::rename(&temp_sidecar_path, sidecar_path).is_err() {
        let _ = std::fs::remove_file(&temp_sidecar_path);
        return;
    }
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
    let lut_composite = if lut_paths.is_empty() { None } else { Some(lut_paths.join("|")) };
    let lut_key = lut_composite.as_deref();
    let local_proxy_path = match local_proxy_path_for(source_path, scale, lut_key, local_root) {
        Some(p) => p,
        None => return (String::new(), false, false),
    };
    let sidecar_proxy_path = alongside_proxy_path_for(source_path, scale, lut_key);

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
        let trf = format!("/tmp/ultimateslice-proxy-vidstab-{:016x}.trf",
            {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                source_path.hash(&mut h);
                h.finish()
            });
        let shakiness = ((vidstab_smoothing * 10.0).round() as i32).clamp(1, 10);
        log::info!("ProxyCache: running vidstab analysis for {} (shakiness={})", source_path, shakiness);
        let analysis_ok = std::process::Command::new(&ffmpeg)
            .arg("-y")
            .arg("-i").arg(source_path)
            .arg("-vf").arg(format!(
                "{},vidstabdetect=shakiness={shakiness}:result={trf}",
                scale.ffmpeg_scale_filter()
            ))
            .arg("-f").arg("null").arg("-")
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
            log::warn!("ProxyCache: vidstab analysis failed for {}, skipping stabilization", source_path);
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
            if std::fs::rename(&temp_proxy_path, &proxy_path).is_err() {
                let _ = std::fs::remove_file(&temp_proxy_path);
                continue;
            }
            if !proxy_file_is_ready(&proxy_path, Some(&ffprobe)) {
                let _ = std::fs::remove_file(&proxy_path);
                continue;
            }
            if owned_local && sidecar_mirror_enabled {
                if let Some(sidecar_path) = sidecar_proxy_path.as_deref() {
                    mirror_local_proxy_to_sidecar(&proxy_path, sidecar_path, &ffprobe);
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
        let mut cache = ProxyCache::new();
        cache.proxies.insert("k".to_string(), sidecar_path.clone());
        cache.cleanup_for_unload(false);
        assert!(!Path::new(&sidecar_path).exists());
        if let Some(parent) = Path::new(&sidecar_path).parent().and_then(|p| p.parent()) {
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
        cache
            .proxies
            .insert("side".to_string(), sidecar_path.clone());
        cache
            .proxies
            .insert("local".to_string(), local_path.clone());
        cache.cleanup_for_unload(true);
        assert!(Path::new(&sidecar_path).exists());
        assert!(!Path::new(&local_path).exists());
        if let Some(parent) = Path::new(&sidecar_path).parent().and_then(|p| p.parent()) {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}
