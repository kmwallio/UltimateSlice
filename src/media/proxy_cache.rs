use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
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

/// Progress snapshot for the status bar.
pub struct ProxyProgress {
    pub total: usize,
    pub completed: usize,
    pub in_flight: bool,
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
/// Encodes both the source path and any assigned LUT so that a scale or LUT
/// change produces a distinct key (and therefore a distinct proxy file).
pub fn proxy_key(source_path: &str, lut_path: Option<&str>) -> String {
    match lut_path {
        Some(lut) if !lut.is_empty() => format!("{}|lut:{}", source_path, lut),
        _ => source_path.to_string(),
    }
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
    result_rx: mpsc::Receiver<ProxyResult>,
    work_tx: Option<mpsc::Sender<(String, ProxyScale, Option<String>)>>,
    /// Managed local cache root (`$XDG_CACHE_HOME/ultimateslice/proxies` or `/tmp/...` fallback).
    local_cache_root: PathBuf,
    /// Local managed-cache files created by this process.
    runtime_owned_local_files: HashSet<String>,
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
        let (result_tx, result_rx) = mpsc::sync_channel::<ProxyResult>(32);
        let (work_tx, work_rx) = mpsc::channel::<(String, ProxyScale, Option<String>)>();

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
                    Ok((source_path, scale, lut_path)) => {
                        let key = proxy_key(&source_path, lut_path.as_deref());
                        let (proxy_path, success, owned_local) =
                            transcode_proxy(&source_path, scale, lut_path.as_deref(), &local_root);
                        if tx
                            .send(ProxyResult {
                                cache_key: key,
                                proxy_path,
                                success,
                                owned_local,
                            })
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
            local_cache_root,
            runtime_owned_local_files: HashSet::new(),
        }
    }

    /// Enqueue a proxy transcode for `source_path` (with optional `lut_path`).
    /// No-op if already cached or pending for this exact (source, lut, scale) combination.
    pub fn request(&mut self, source_path: &str, scale: ProxyScale, lut_path: Option<&str>) {
        let key = proxy_key(source_path, lut_path);
        if self.proxies.contains_key(&key)
            || self.pending.contains(&key)
            || self.failed.contains(&key)
        {
            return;
        }
        // Check for pre-existing proxy on disk before spawning work.
        if let Some(p) =
            existing_proxy_path_for(source_path, scale, lut_path, &self.local_cache_root)
        {
            if std::fs::metadata(&p).map_or(false, |m| m.len() > 0) {
                self.proxies.insert(key, p);
                return;
            }
        }
        self.pending.insert(key);
        self.total_requested += 1;
        if let Some(ref tx) = self.work_tx {
            let _ = tx.send((
                source_path.to_string(),
                scale,
                lut_path.map(|s| s.to_string()),
            ));
        }
    }

    /// Drain completed background transcodes. Returns cache keys that were just resolved.
    pub fn poll(&mut self) -> Vec<String> {
        let mut resolved = Vec::new();
        while let Ok(result) = self.result_rx.try_recv() {
            self.pending.remove(&result.cache_key);
            if result.success {
                self.proxies
                    .insert(result.cache_key.clone(), result.proxy_path.clone());
                if result.owned_local {
                    self.runtime_owned_local_files
                        .insert(result.proxy_path.clone());
                    append_owned_entry(&self.local_cache_root, &result.proxy_path);
                }
            } else {
                self.failed.insert(result.cache_key.clone());
            }
            resolved.push(result.cache_key);
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
    }

    /// Remove managed local proxy cache files on project unload/close.
    /// - For `$XDG_CACHE_HOME` managed roots, clears local managed files referenced
    ///   by this project/session.
    /// - For `/tmp` managed roots, removes only files this process created.
    pub fn cleanup_local_cache_for_unload(&mut self) {
        let root_is_tmp = self.local_cache_root.starts_with("/tmp");
        let mut candidates: HashSet<String> = HashSet::new();
        if root_is_tmp {
            candidates.extend(self.runtime_owned_local_files.iter().cloned());
        } else {
            candidates.extend(
                self.proxies
                    .values()
                    .filter(|p| is_path_within_root(p, &self.local_cache_root))
                    .cloned(),
            );
            candidates.extend(self.runtime_owned_local_files.iter().cloned());
        }
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

    /// Current progress snapshot.
    pub fn progress(&self) -> ProxyProgress {
        let completed = self.proxies.len();
        ProxyProgress {
            total: self.total_requested,
            completed: completed.min(self.total_requested),
            in_flight: !self.pending.is_empty(),
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
) -> Option<String> {
    if let Some(local) = local_proxy_path_for(source_path, scale, lut_path, local_root) {
        if std::fs::metadata(&local).map_or(false, |m| m.len() > 0) {
            return Some(local);
        }
    }
    if let Some(side) = alongside_proxy_path_for(source_path, scale, lut_path) {
        if std::fs::metadata(&side).map_or(false, |m| m.len() > 0) {
            return Some(side);
        }
    }
    None
}

fn run_transcode_command(ffmpeg: &str, source_path: &str, proxy_path: &str, filter: &str) -> bool {
    let status = std::process::Command::new(ffmpeg)
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
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
        .arg(proxy_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    matches!(status, Ok(s) if s.success())
}

/// Run ffmpeg to create a proxy file.
/// Returns `(proxy_path, success, owned_local)`.
fn transcode_proxy(
    source_path: &str,
    scale: ProxyScale,
    lut_path: Option<&str>,
    local_root: &Path,
) -> (String, bool, bool) {
    let local_proxy_path = match local_proxy_path_for(source_path, scale, lut_path, local_root) {
        Some(p) => p,
        None => return (String::new(), false, false),
    };
    let sidecar_proxy_path = alongside_proxy_path_for(source_path, scale, lut_path);

    let ffmpeg = match crate::media::export::find_ffmpeg() {
        Ok(f) => f,
        Err(_) => return (local_proxy_path, false, false),
    };

    // Build the -vf filter string: scale, then optional lut3d.
    let mut filter = scale.ffmpeg_scale_filter().to_string();
    if let Some(lut) = lut_path {
        if !lut.is_empty() {
            // Escape colons in the path for ffmpeg filter syntax.
            let escaped = lut.replace('\\', "\\\\").replace(':', "\\:");
            filter.push_str(&format!(",lut3d={escaped}"));
        }
    }

    let attempts = sidecar_proxy_path
        .map(|p| vec![(local_proxy_path.clone(), true), (p, false)])
        .unwrap_or_else(|| vec![(local_proxy_path.clone(), true)]);

    for (proxy_path, owned_local) in attempts {
        let proxy_dir = Path::new(&proxy_path).parent().unwrap_or(Path::new("."));
        if std::fs::create_dir_all(proxy_dir).is_err() {
            if owned_local {
                log::warn!(
                    "ProxyCache: local cache path unavailable, falling back to alongside-media cache for {}",
                    source_path
                );
            }
            continue;
        }
        if run_transcode_command(&ffmpeg, source_path, &proxy_path, &filter) {
            if std::fs::metadata(&proxy_path).map_or(true, |m| m.len() == 0) {
                let _ = std::fs::remove_file(&proxy_path);
                continue;
            }
            return (proxy_path, true, owned_local);
        }
        let _ = std::fs::remove_file(&proxy_path);
        if owned_local {
            log::warn!(
                "ProxyCache: local cache transcode failed, retrying alongside-media cache for {}",
                source_path
            );
        }
    }
    (local_proxy_path, false, false)
}
