use std::path::PathBuf;

pub(crate) fn cache_root_dir(cache_name: &str) -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".cache")
        });
    base.join("ultimateslice").join(cache_name)
}

/// True when a `.partial` cache file looks like it is still being written:
/// its modification time is within the last two minutes. ffmpeg keeps the
/// file's mtime fresh while encoding, and the `+faststart` pass at the end
/// re-opens the output *by path* to shift the moov atom — so a cleanup that
/// unlinks an in-flight `.partial` makes that re-open fail and throws the
/// whole transcode away ("Unable to re-open … output file for shifting
/// data"). Cleanups skip files this reports as in flight.
pub(crate) fn partial_file_is_in_flight(path: &std::path::Path) -> bool {
    const IN_FLIGHT_WINDOW: std::time::Duration = std::time::Duration::from_secs(120);
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|modified| std::time::SystemTime::now().duration_since(modified).ok())
        .map(|age| age < IN_FLIGHT_WINDOW)
        .unwrap_or(false)
}

/// Lower the calling thread's scheduling priority (nice 10 on Linux) so a
/// background worker — and the threads and child processes it spawns, which
/// inherit it — never competes with playback and the UI for CPU.
pub(crate) fn lower_current_thread_priority() {
    #[cfg(target_os = "linux")]
    // SAFETY: setpriority(PRIO_PROCESS, 0, …) only changes the calling
    // thread's own nice value; it touches no Rust state.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS as _, 0, 10);
    }
}

/// Make the calling thread a true background worker: nice 10 *and* the
/// `SCHED_IDLE` policy on Linux, under which the kernel only schedules the
/// thread when a CPU would otherwise be idle. Threads and child processes
/// it spawns (whisper/ONNX pools, ffmpeg extractors) inherit both, so an
/// indexing job that is already running yields its cores the moment
/// playback — or anything else — wants them, instead of merely running at a
/// lower nice level. Not for user-facing work such as thumbnails, which
/// should still finish promptly on a busy machine.
pub(crate) fn make_current_thread_background() {
    lower_current_thread_priority();
    #[cfg(target_os = "linux")]
    // SAFETY: sched_setscheduler(0, …) only changes the calling thread's
    // own policy; the param struct is a plain zeroed C struct.
    unsafe {
        let param = libc::sched_param { sched_priority: 0 };
        libc::sched_setscheduler(0, libc::SCHED_IDLE, &param);
    }
}

static PLAYBACK_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Published by the window's tick: true while the Program Monitor is playing.
/// Background extractors poll it between units of work.
pub(crate) fn set_playback_active(active: bool) {
    PLAYBACK_ACTIVE.store(active, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn playback_active() -> bool {
    PLAYBACK_ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Block the calling background worker while playback is active, polling
/// every 250 ms, for at most `max_wait`. Used between frame extractions so
/// an in-flight indexing job stops spawning new 8K decodes once the user
/// presses play, and resumes by itself when playback stops.
pub(crate) fn wait_while_playback_active(max_wait: std::time::Duration) {
    let started = std::time::Instant::now();
    while playback_active() && started.elapsed() < max_wait {
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

static ANALYSIS_PROXY_PATHS: std::sync::RwLock<Option<std::collections::HashMap<String, String>>> =
    std::sync::RwLock::new(None);

/// Published by the window's tick: source path → ready plain proxy file
/// (no LUT / stabilisation variants). Thumbnail and CLIP frame extraction
/// decode the proxy instead of the original whenever one exists — a 1080p
/// H.264 frame is the same picture at a fraction of the decode cost of an
/// 8K 10-bit HEVC one.
pub(crate) fn set_analysis_proxy_paths(paths: std::collections::HashMap<String, String>) {
    if let Ok(mut guard) = ANALYSIS_PROXY_PATHS.write() {
        *guard = Some(paths);
    }
}

/// The file a background analyser should decode for `source_path`: its
/// ready proxy when one is published and still on disk, else the source.
pub(crate) fn analysis_source_for(source_path: &str) -> String {
    let proxy = ANALYSIS_PROXY_PATHS
        .read()
        .ok()
        .and_then(|guard| guard.as_ref().and_then(|m| m.get(source_path).cloned()));
    match proxy {
        Some(p) if std::path::Path::new(&p).is_file() => p,
        _ => source_path.to_string(),
    }
}

pub(crate) fn file_has_content(path: &str) -> bool {
    std::fs::metadata(path)
        .map(|m| m.len() > 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn analysis_source_falls_back_to_the_original_unless_the_proxy_exists() {
        let proxy = NamedTempFile::new().unwrap();
        let proxy_path = proxy.path().to_string_lossy().into_owned();
        set_analysis_proxy_paths(std::collections::HashMap::from([
            ("/m/has-proxy.mp4".to_string(), proxy_path.clone()),
            (
                "/m/stale-proxy.mp4".to_string(),
                "/definitely/missing/proxy.mp4".to_string(),
            ),
        ]));
        assert_eq!(analysis_source_for("/m/has-proxy.mp4"), proxy_path);
        assert_eq!(
            analysis_source_for("/m/stale-proxy.mp4"),
            "/m/stale-proxy.mp4"
        );
        assert_eq!(analysis_source_for("/m/no-proxy.mp4"), "/m/no-proxy.mp4");
        set_analysis_proxy_paths(std::collections::HashMap::new());
    }

    #[test]
    fn wait_while_playback_active_returns_at_once_when_idle() {
        set_playback_active(false);
        let started = std::time::Instant::now();
        wait_while_playback_active(std::time::Duration::from_secs(5));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        set_playback_active(true);
        let started = std::time::Instant::now();
        wait_while_playback_active(std::time::Duration::from_millis(300));
        assert!(started.elapsed() >= std::time::Duration::from_millis(250));
        set_playback_active(false);
    }

    #[test]
    fn file_has_content_returns_false_for_missing_file() {
        assert!(!file_has_content(
            "/definitely/missing/ultimateslice-cache-support"
        ));
    }

    #[test]
    fn fresh_partial_is_in_flight_and_old_partial_is_not() {
        let temp = NamedTempFile::new().expect("temp file");
        std::fs::write(temp.path(), b"encoding...").expect("write");
        assert!(partial_file_is_in_flight(temp.path()));
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(10 * 60);
        std::fs::File::options()
            .append(true)
            .open(temp.path())
            .expect("open")
            .set_modified(old)
            .expect("set mtime");
        assert!(!partial_file_is_in_flight(temp.path()));
        assert!(!partial_file_is_in_flight(std::path::Path::new(
            "/definitely/missing/ultimateslice.partial"
        )));
    }

    #[test]
    fn file_has_content_detects_nonempty_file() {
        let temp = NamedTempFile::new().expect("temp file");
        std::fs::write(temp.path(), b"data").expect("write temp file");
        assert!(file_has_content(temp.path().to_string_lossy().as_ref()));
    }
}
