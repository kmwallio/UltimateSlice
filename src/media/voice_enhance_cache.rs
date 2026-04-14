// SPDX-License-Identifier: GPL-3.0-or-later
//! Offline "Enhance Voice" prerender cache.
//!
//! When a clip has `voice_enhance` enabled, an ffmpeg subprocess
//! produces a sidecar media file with the **video stream copied**
//! and the **audio re-encoded** through the same filter chain that
//! `build_voice_enhance_filter` uses for export. The cached file is
//! then handed to `ProgramPlayer::resolve_source_path_for_clip`
//! exactly the same way the bg-removal and proxy paths work, so the
//! preview pipeline plays the cleaned audio without any
//! GStreamer-side audio processing.
//!
//! This module is modeled directly after `bg_removal_cache.rs`. The
//! same request/poll/paths conventions apply.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::SystemTime;

/// Default soft cap on the total disk space used by the voice enhance
/// cache when no preference is set. The cap is overrideable per-cache
/// at runtime via [`VoiceEnhanceCache::set_cache_cap_bytes`], driven
/// from `PreferencesState::voice_enhance_cache_cap_gib` in the
/// preferences UI. When `request()` finds the cache is over the cap
/// it evicts the least-recently-modified files until the total drops
/// back under. 2 GiB is enough for ~30 minutes of typical 4K H.264
/// source remuxed with new audio; raise it if your projects involve
/// longer clips and you have headroom on disk.
const DEFAULT_MAX_CACHE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

// ── Public types ───────────────────────────────────────────────────────────

enum WorkerUpdate {
    Done(WorkerResult),
}

struct WorkerResult {
    cache_key: String,
    output_path: String,
    success: bool,
}

struct VoiceEnhanceJob {
    cache_key: String,
    source_path: String,
    output_path: String,
    strength: f32,
}

pub struct VoiceEnhanceProgress {
    pub total: usize,
    pub completed: usize,
    pub in_flight: bool,
}

/// Per-clip cache status returned by [`VoiceEnhanceCache::status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceEnhanceStatus {
    /// Nothing has been requested for this `(source, strength)` yet.
    Idle,
    /// An ffmpeg job is currently running for this key.
    Pending,
    /// A cached file exists and the preview is using it.
    Ready,
    /// The most recent ffmpeg job failed; the user can retry via
    /// [`VoiceEnhanceCache::retry`].
    Failed,
}

// ── Cache ──────────────────────────────────────────────────────────────────

pub struct VoiceEnhanceCache {
    /// Completed enhanced media file paths, keyed by **cache_key**
    /// (`source fingerprint` + `strength`). Different strengths for the same
    /// source produce different cache entries.
    pub paths: HashMap<String, String>,
    /// Currently processing keys.
    pending: HashSet<String>,
    /// Failed keys (not retried in this session).
    failed: HashSet<String>,
    total_requested: usize,
    result_rx: mpsc::Receiver<WorkerUpdate>,
    work_tx: Option<mpsc::Sender<VoiceEnhanceJob>>,
    cache_root: PathBuf,
    /// Runtime-overridable cache size cap. Defaults to
    /// [`DEFAULT_MAX_CACHE_BYTES`]; the preferences UI sets it via
    /// [`Self::set_cache_cap_bytes`] from
    /// `PreferencesState::voice_enhance_cache_cap_gib`.
    cache_cap_bytes: u64,
}

impl VoiceEnhanceCache {
    pub fn new() -> Self {
        let (result_tx, result_rx) = mpsc::sync_channel::<WorkerUpdate>(32);
        let (work_tx, work_rx) = mpsc::channel::<VoiceEnhanceJob>();
        let work_rx = std::sync::Arc::new(std::sync::Mutex::new(work_rx));

        // One worker thread is plenty — ffmpeg saturates a CPU core
        // already and we don't want to thrash disk with parallel jobs.
        let rx = work_rx.clone();
        let tx = result_tx;
        std::thread::spawn(move || loop {
            let job = {
                let lock = rx.lock().unwrap();
                lock.recv()
            };
            match job {
                Ok(job) => {
                    let success =
                        run_voice_enhance(&job.source_path, &job.output_path, job.strength);
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

    /// Request a voice-enhance prerender for `source_path` at the given
    /// `strength`. Returns immediately. Call [`poll`] periodically to
    /// pick up completed results.
    pub fn request(&mut self, source_path: &str, strength: f32) {
        let key = cache_key(source_path, strength);

        // Already known to be ready? Touch its mtime so the LRU
        // eviction below sees it as recently-used, then return.
        if self.paths.contains_key(&key) {
            if let Some(p) = self.paths.get(&key) {
                touch_mtime(p);
            }
            return;
        }
        // In-flight or known-bad? Skip.
        if self.pending.contains(&key) || self.failed.contains(&key) {
            return;
        }

        // Pre-existing on-disk result from a previous session.
        let output_path = self.output_path_for_key(&key);
        if Path::new(&output_path).exists() && file_is_ready(&output_path) {
            log::info!("VoiceEnhanceCache: found existing file for key={}", key);
            touch_mtime(&output_path);
            self.paths.insert(key, output_path);
            return;
        } else if Path::new(&output_path).exists() {
            // Stale / zero-byte file from a crashed previous run.
            let _ = std::fs::remove_file(&output_path);
        }

        // Best-effort: keep the cache under the size cap before
        // queuing another job. Cheap when already under, otherwise
        // does an O(n log n) directory scan + a few unlink syscalls.
        self.evict_if_oversized();

        self.total_requested += 1;
        self.pending.insert(key.clone());
        if let Some(ref tx) = self.work_tx {
            let _ = tx.send(VoiceEnhanceJob {
                cache_key: key,
                source_path: source_path.to_string(),
                output_path,
                strength,
            });
        }
    }

    /// Update the in-memory cache size cap (in bytes) used by the
    /// next `request()`'s LRU eviction pass. Driven from the
    /// `PreferencesState::voice_enhance_cache_cap_gib` preference. A
    /// new cap takes effect on the next request, so the user can
    /// shrink the cap mid-session and the next prerender request will
    /// honour it.
    pub fn set_cache_cap_bytes(&mut self, bytes: u64) {
        self.cache_cap_bytes = bytes.max(64 * 1024 * 1024); // floor at 64 MiB to avoid pathological caps
    }

    /// Walk the cache directory and delete the least-recently-modified
    /// files until total disk usage is back under the current
    /// `cache_cap_bytes`. Files currently held in `self.paths` are
    /// NOT exempt — if a file gets evicted, the in-memory entry is
    /// removed too so the next `request()` will treat it as missing
    /// and re-prerender.
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
        // Oldest mtime first → evict first.
        files.sort_by_key(|(_, _, mtime)| *mtime);
        log::info!(
            "VoiceEnhanceCache: cache size {} > cap {}, evicting LRU files",
            total,
            cap
        );
        let mut bytes_freed: u64 = 0;
        for (path, len, _mtime) in files {
            if total.saturating_sub(bytes_freed) <= cap {
                break;
            }
            // Drop any in-memory `paths` entry that points at this
            // file so the next `request()` rebuilds rather than
            // returning a deleted path.
            let path_str = path.to_string_lossy().to_string();
            self.paths.retain(|_, v| v != &path_str);
            if let Err(e) = std::fs::remove_file(&path) {
                log::warn!(
                    "VoiceEnhanceCache: failed to evict {}: {}",
                    path.display(),
                    e
                );
                continue;
            }
            bytes_freed += len;
            log::info!(
                "VoiceEnhanceCache: evicted {} ({} bytes)",
                path.display(),
                len
            );
        }
    }

    /// Non-blocking poll for completed jobs. Returns the list of cache
    /// keys that became newly ready since the last poll.
    pub fn poll(&mut self) -> Vec<String> {
        let mut resolved = Vec::new();
        while let Ok(update) = self.result_rx.try_recv() {
            match update {
                WorkerUpdate::Done(result) => {
                    self.pending.remove(&result.cache_key);
                    if result.success && Path::new(&result.output_path).exists() {
                        log::info!(
                            "VoiceEnhanceCache: completed key={} path={}",
                            result.cache_key,
                            result.output_path
                        );
                        self.paths
                            .insert(result.cache_key.clone(), result.output_path);
                        resolved.push(result.cache_key);
                    } else {
                        log::warn!("VoiceEnhanceCache: failed key={}", result.cache_key);
                        self.failed.insert(result.cache_key);
                    }
                }
            }
        }
        resolved
    }

    pub fn progress(&self) -> VoiceEnhanceProgress {
        VoiceEnhanceProgress {
            total: self.total_requested,
            completed: self.paths.len(),
            in_flight: !self.pending.is_empty(),
        }
    }

    pub fn invalidate_all(&mut self) {
        self.paths.clear();
        self.failed.clear();
        self.total_requested = 0;
    }

    /// Look up the cached output for `(source_path, strength)`, if ready.
    pub fn get_path(&self, source_path: &str, strength: f32) -> Option<&String> {
        self.paths.get(&cache_key(source_path, strength))
    }

    /// Per-clip cache status used by the Inspector to render an inline
    /// hint next to the strength slider. The four states map cleanly
    /// onto a short status string:
    ///
    /// - `Ready` — sidecar exists, preview is using it.
    /// - `Pending` — ffmpeg job is in flight.
    /// - `Failed` — ffmpeg job failed (use `retry` to re-queue).
    /// - `Idle` — nothing has been requested yet for this key.
    pub fn status(&self, source_path: &str, strength: f32) -> VoiceEnhanceStatus {
        let key = cache_key(source_path, strength);
        if self.paths.contains_key(&key) {
            VoiceEnhanceStatus::Ready
        } else if self.pending.contains(&key) {
            VoiceEnhanceStatus::Pending
        } else if self.failed.contains(&key) {
            VoiceEnhanceStatus::Failed
        } else {
            VoiceEnhanceStatus::Idle
        }
    }

    /// Drop a key from the `failed` set so the next `request()` will
    /// re-queue the job. Returns `true` if the key actually was failed.
    /// Used by the Inspector "Retry" button.
    pub fn retry(&mut self, source_path: &str, strength: f32) -> bool {
        let key = cache_key(source_path, strength);
        self.failed.remove(&key)
    }

    fn output_path_for_key(&self, key: &str) -> String {
        // Use .mp4 — ffmpeg can stream-copy most video codecs into mp4
        // and AAC audio is universally decodable in GStreamer.
        self.cache_root
            .join(format!("{key}.mp4"))
            .to_string_lossy()
            .to_string()
    }
}

impl Drop for VoiceEnhanceCache {
    fn drop(&mut self) {
        self.work_tx.take();
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Stable cache key for `(source_path, source_mtime, strength)`.
///
/// Strength is rounded to 1% so tiny float wobble in the slider doesn't
/// cause cache thrash. Source mtime (Unix seconds since epoch) is folded
/// into the hash so re-encoding or replacing the source file in place
/// invalidates the cache automatically — without that, the cached
/// enhanced audio would silently come from a stale render of the old
/// source. Stat failures fall back to mtime=0, which still produces a
/// stable key for missing files (the prerender will then fail and the
/// key will be marked as `failed`).
pub fn cache_key(source_path: &str, strength: f32) -> String {
    let s = (strength.clamp(0.0, 1.0) * 100.0).round() as u32;
    let mut hasher = crate::media::cache_key::CacheKeyHasher::new();
    hasher.add_source_fingerprint(source_path).add(s);
    format!("ve_{:016x}_{s:03}", hasher.finish())
}

fn dirs_cache_root() -> PathBuf {
    crate::media::cache_support::cache_root_dir("voice_enhance")
}

fn file_is_ready(path: &str) -> bool {
    crate::media::cache_support::file_has_content(path)
}

/// Bump a file's access + modification time to the current wall clock,
/// best-effort. Used by the LRU eviction so that files which are
/// actively being looked up by `request()` (cache hits) get pushed back
/// to the head of the eviction queue. Failures are silently ignored —
/// at worst the file looks stale and gets evicted prematurely.
fn touch_mtime(path: &str) {
    use std::ffi::CString;
    if let Ok(c_path) = CString::new(path) {
        // SAFETY: libc::utime with NULL buf sets both atime and mtime
        // to "now". c_path is a valid NUL-terminated C string for the
        // duration of the call.
        unsafe {
            let _ = libc::utime(c_path.as_ptr(), std::ptr::null());
        }
    }
}

/// Run the ffmpeg subprocess for one voice-enhance job. The video
/// stream is copied (no re-encode), the audio runs through the same
/// HPF + afftdn + EQ + acompressor chain that the export side uses.
fn run_voice_enhance(source_path: &str, output_path: &str, strength: f32) -> bool {
    let filter = build_voice_enhance_filter_string(strength);
    log::info!(
        "VoiceEnhanceCache: ffmpeg src={} -> out={} strength={:.2}",
        source_path,
        output_path,
        strength
    );
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-loglevel",
            "warning",
            "-i",
            source_path,
            "-map",
            "0:v?",
            "-map",
            "0:a?",
            "-c:v",
            "copy",
            "-af",
            &filter,
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-movflags",
            "+faststart",
            output_path,
        ])
        .status();
    match status {
        Ok(s) if s.success() => true,
        Ok(s) => {
            log::warn!("VoiceEnhanceCache: ffmpeg exited with {s:?}");
            false
        }
        Err(e) => {
            log::warn!("VoiceEnhanceCache: ffmpeg spawn error: {e}");
            false
        }
    }
}

/// Build the audio filter chain string. Mirrors the curve in
/// `build_voice_enhance_filter` (`src/media/export.rs`) so the slider
/// feels identical between preview and export. **Update both at once**
/// if you change the curve.
fn build_voice_enhance_filter_string(strength: f32) -> String {
    let s = strength.clamp(0.0, 1.0) as f64;
    let nr_db = 6.0 + 18.0 * s;
    let mud_g = -1.0 - 2.0 * s;
    let pres_g = 1.0 + 4.0 * s;
    let comp_ratio = 2.0 + 3.0 * s;
    let makeup = 1.0 + 2.0 * s;
    format!(
        "highpass=f=80,\
         afftdn=nr={nr_db:.1}:nf=-25,\
         equalizer=f=300:t=q:w=1.0:g={mud_g:.2},\
         equalizer=f=4000:t=q:w=1.5:g={pres_g:.2},\
         acompressor=threshold=0.05:ratio={comp_ratio:.2}:attack=20:release=250:makeup={makeup:.2}"
    )
}
