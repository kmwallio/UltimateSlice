use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// Result of a background proxy transcode.
pub struct ProxyResult {
    pub source_path: String,
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
}

/// Asynchronous proxy media cache.
///
/// Uses a single background worker thread to transcode source media files
/// into lightweight H.264 proxy files via ffmpeg. Follows the same
/// request/poll/get pattern as `MediaProbeCache` and `ThumbnailCache`.
///
/// Proxy files are stored in a `.ultimateslice_proxies/` directory next to
/// the source file.
pub struct ProxyCache {
    /// Map from source path → proxy file path (completed only).
    pub proxies: HashMap<String, String>,
    /// Source paths currently being transcoded or queued.
    pending: HashSet<String>,
    /// Total items ever requested in this session (for progress).
    total_requested: usize,
    result_rx: mpsc::Receiver<ProxyResult>,
    work_tx: Option<mpsc::Sender<(String, ProxyScale)>>,
}

impl ProxyCache {
    pub fn new() -> Self {
        let (result_tx, result_rx) = mpsc::sync_channel::<ProxyResult>(32);
        let (work_tx, work_rx) = mpsc::channel::<(String, ProxyScale)>();

        // Single worker thread processes transcodes sequentially.
        std::thread::spawn(move || {
            while let Ok((source_path, scale)) = work_rx.recv() {
                let (proxy_path, success) = transcode_proxy(&source_path, scale);
                if result_tx
                    .send(ProxyResult {
                        source_path,
                        proxy_path,
                        success,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        Self {
            proxies: HashMap::new(),
            pending: HashSet::new(),
            total_requested: 0,
            result_rx,
            work_tx: Some(work_tx),
        }
    }

    /// Enqueue a proxy transcode for `source_path`. No-op if already cached or pending.
    pub fn request(&mut self, source_path: &str, scale: ProxyScale) {
        if self.proxies.contains_key(source_path) || self.pending.contains(source_path) {
            // Check if a proxy file already exists on disk from a previous session.
            if !self.proxies.contains_key(source_path) && !self.pending.contains(source_path) {
                if let Some(p) = proxy_path_for(source_path) {
                    if Path::new(&p).exists() {
                        self.proxies.insert(source_path.to_string(), p);
                        return;
                    }
                }
            }
            return;
        }
        // Check for pre-existing proxy on disk before spawning work.
        if let Some(p) = proxy_path_for(source_path) {
            if Path::new(&p).exists() {
                self.proxies.insert(source_path.to_string(), p);
                return;
            }
        }
        self.pending.insert(source_path.to_string());
        self.total_requested += 1;
        if let Some(ref tx) = self.work_tx {
            let _ = tx.send((source_path.to_string(), scale));
        }
    }

    /// Drain completed background transcodes. Returns source paths that were just resolved.
    pub fn poll(&mut self) -> Vec<String> {
        let mut resolved = Vec::new();
        while let Ok(result) = self.result_rx.try_recv() {
            self.pending.remove(&result.source_path);
            if result.success {
                self.proxies
                    .insert(result.source_path.clone(), result.proxy_path);
            }
            resolved.push(result.source_path);
        }
        resolved
    }

    /// Get the proxy path for a source, if transcoded.
    pub fn get(&self, source_path: &str) -> Option<&String> {
        self.proxies.get(source_path)
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

/// Compute the proxy output path for a given source path.
/// Returns `<parent>/.ultimateslice_proxies/<stem>.proxy.mp4`.
fn proxy_path_for(source_path: &str) -> Option<String> {
    let src = Path::new(source_path);
    let parent = src.parent()?;
    let stem = src.file_stem()?.to_str()?;
    let proxy_dir = parent.join(".ultimateslice_proxies");
    Some(proxy_dir.join(format!("{stem}.proxy.mp4")).to_string_lossy().into_owned())
}

/// Run ffmpeg to create a proxy file. Returns (proxy_path, success).
fn transcode_proxy(source_path: &str, scale: ProxyScale) -> (String, bool) {
    let proxy_path = match proxy_path_for(source_path) {
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

    let filter = scale.ffmpeg_scale_filter();

    let status = std::process::Command::new(&ffmpeg)
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
