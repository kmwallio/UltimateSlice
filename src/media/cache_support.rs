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
