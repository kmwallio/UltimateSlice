use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;
use std::path::Path;
use std::sync::mpsc;

/// Result of a background proxy transcode.
pub struct ProxyResult {
    /// Composite cache key (see `proxy_key()`).
    pub cache_key: String,
    pub proxy_path: String,
    pub success: bool,
}

/// Progress snapshot for the status bar.
pub struct ProxyProgress {
    pub total: usize,
    pub completed: usize,
    pub in_flight: bool,
}

/// Scale factor for proxy transcodes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ProxyScale {
    Half,
    Quarter,
}

impl ProxyScale {
    pub fn ffmpeg_scale_filter(&self) -> &'static str {
        match self {
            ProxyScale::Half => "scale=iw/2:ih/2",
            ProxyScale::Quarter => "scale=iw/4:ih/4",
        }
    }

    fn suffix(&self) -> &'static str {
        match self {
            ProxyScale::Half => "half",
            ProxyScale::Quarter => "quarter",
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
/// Proxy files are stored in a `.ultimateslice_proxies/` directory next to
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
    /// Total items ever requested in this session (for progress).
    total_requested: usize,
    result_rx: mpsc::Receiver<ProxyResult>,
    work_tx: Option<mpsc::Sender<(String, ProxyScale, Option<String>)>>,
}

impl ProxyCache {
    pub fn new() -> Self {
        let (result_tx, result_rx) = mpsc::sync_channel::<ProxyResult>(32);
        let (work_tx, work_rx) = mpsc::channel::<(String, ProxyScale, Option<String>)>();

        // Pool of worker threads to transcode proxies in parallel.
        let work_rx = std::sync::Arc::new(std::sync::Mutex::new(work_rx));
        let num_workers = 4;
        for _ in 0..num_workers {
            let rx = work_rx.clone();
            let tx = result_tx.clone();
            std::thread::spawn(move || {
                loop {
                    let item = {
                        let lock = rx.lock().unwrap();
                        lock.recv()
                    };
                    match item {
                        Ok((source_path, scale, lut_path)) => {
                            let key = proxy_key(&source_path, lut_path.as_deref());
                            let (proxy_path, success) = transcode_proxy(&source_path, scale, lut_path.as_deref());
                            if tx.send(ProxyResult { cache_key: key, proxy_path, success }).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        Self {
            proxies: HashMap::new(),
            pending: HashSet::new(),
            total_requested: 0,
            result_rx,
            work_tx: Some(work_tx),
        }
    }

    /// Enqueue a proxy transcode for `source_path` (with optional `lut_path`).
    /// No-op if already cached or pending for this exact (source, lut, scale) combination.
    pub fn request(&mut self, source_path: &str, scale: ProxyScale, lut_path: Option<&str>) {
        let key = proxy_key(source_path, lut_path);
        if self.proxies.contains_key(&key) || self.pending.contains(&key) {
            return;
        }
        // Check for pre-existing proxy on disk before spawning work.
        if let Some(p) = proxy_path_for(source_path, scale, lut_path) {
            if Path::new(&p).exists() {
                self.proxies.insert(key, p);
                return;
            }
        }
        self.pending.insert(key);
        self.total_requested += 1;
        if let Some(ref tx) = self.work_tx {
            let _ = tx.send((source_path.to_string(), scale, lut_path.map(|s| s.to_string())));
        }
    }

    /// Drain completed background transcodes. Returns cache keys that were just resolved.
    pub fn poll(&mut self) -> Vec<String> {
        let mut resolved = Vec::new();
        while let Ok(result) = self.result_rx.try_recv() {
            self.pending.remove(&result.cache_key);
            if result.success {
                self.proxies.insert(result.cache_key.clone(), result.proxy_path);
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
        self.total_requested = 0;
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

/// Compute the proxy output path for a given source path, scale, and optional LUT.
/// Pattern: `<parent>/.ultimateslice_proxies/<stem>.proxy_<scale>[_lut<hash>].mp4`
fn proxy_path_for(source_path: &str, scale: ProxyScale, lut_path: Option<&str>) -> Option<String> {
    let src = Path::new(source_path);
    let parent = src.parent()?;
    let stem = src.file_stem()?.to_str()?;
    let proxy_dir = parent.join(".ultimateslice_proxies");
    let lut_suffix = match lut_path {
        Some(lut) if !lut.is_empty() => format!("_lut{}", lut_hash(lut)),
        _ => String::new(),
    };
    let filename = format!("{}.proxy_{}{}.mp4", stem, scale.suffix(), lut_suffix);
    Some(proxy_dir.join(filename).to_string_lossy().into_owned())
}

/// Run ffmpeg to create a proxy file. Returns (proxy_path, success).
fn transcode_proxy(source_path: &str, scale: ProxyScale, lut_path: Option<&str>) -> (String, bool) {
    let proxy_path = match proxy_path_for(source_path, scale, lut_path) {
        Some(p) => p,
        None => return (String::new(), false),
    };

    // Ensure proxy directory exists.
    let proxy_dir = Path::new(&proxy_path).parent().unwrap_or(Path::new("."));
    if std::fs::create_dir_all(proxy_dir).is_err() {
        return (proxy_path, false);
    }

    let ffmpeg = match crate::media::export::find_ffmpeg() {
        Ok(f) => f,
        Err(_) => return (proxy_path, false),
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

    let status = std::process::Command::new(&ffmpeg)
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(source_path)
        .arg("-vf")
        .arg(&filter)
        .arg("-c:v")
        .arg("libx264")
        .arg("-preset")
        .arg("ultrafast")
        .arg("-crf")
        .arg("28")
        .arg("-c:a")
        .arg("aac")
        .arg("-b:a")
        .arg("128k")
        .arg("-movflags")
        .arg("+faststart")
        .arg(&proxy_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match status {
        Ok(s) if s.success() => (proxy_path, true),
        _ => {
            // Clean up partial file on failure.
            let _ = std::fs::remove_file(&proxy_path);
            (proxy_path, false)
        }
    }
}
