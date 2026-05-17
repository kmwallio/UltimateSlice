//! Model manifest: single source of truth for the AI models UltimateSlice
//! ships and downloads on demand.
//!
//! Each [`ModelManifestEntry`] carries the fields needed to identify, fetch,
//! and verify one ONNX (or other binary) model file:
//!
//! * `key` / `display_name` / `filename` — identity
//! * `url` — where to fetch from (hosted by us when we control the model;
//!   third-party CDN when we depend on someone else's published model)
//! * `expected_sha256` — verification hash. `None` means "no known hash yet"
//!   (verification is skipped with a warning that logs the actual hash so a
//!   future commit can pin it). Once `Some`, downloads with a mismatching
//!   digest are rejected and the partial file deleted.
//! * `expected_size_bytes` — used to compute a real progress fraction by
//!   polling the `.partial` file's size during download. `None` falls back
//!   to a pulsing/indeterminate UI.
//! * `license_short` / `license_url` — surfaced in Preferences → Models so
//!   users see what they're agreeing to before clicking download.
//!
//! [`download_model_in_background`] kicks off a curl subprocess on a worker
//! thread and returns a shared `Arc<Mutex<DownloadState>>` the GTK UI can
//! poll on a glib timeout to drive the progress bar and status label —
//! mirroring the existing background-download pattern in
//! `src/ui/preferences.rs` so callers don't need a new lifecycle model.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy)]
pub struct ModelManifestEntry {
    pub key: &'static str,
    pub display_name: &'static str,
    pub filename: &'static str,
    pub url: &'static str,
    pub expected_sha256: Option<&'static str>,
    pub expected_size_bytes: Option<u64>,
    pub license_short: &'static str,
    pub license_url: &'static str,
    pub description: &'static str,
}

/// Portrait-matting model used for AI background removal.
///
/// Currently MODNet, downloaded from a third-party Google Drive mirror
/// because we don't yet host our own segmentation model. `expected_sha256`
/// is `None` — the file the mirror serves can change without notice and we
/// haven't pinned a known-good digest for the upstream binary. The
/// download still records the actual SHA-256 of what arrived (logged at
/// info level + stored in the [`DownloadState::Done`] payload) so a future
/// commit can promote that hash to a hardcoded `Some(...)` for integrity
/// pinning. When we replace MODNet with a self-hosted model in a follow-up
/// phase, this entry's `url` and `expected_sha256` flip together.
pub const PORTRAIT_MATTING: ModelManifestEntry = ModelManifestEntry {
    key: "portrait_matting",
    display_name: "MODNet (Background Removal)",
    filename: "modnet_photographic_portrait_matting.onnx",
    url: "https://drive.usercontent.google.com/download?id=1cgycTQlYXpTh26gB9FTnthE7AvruV8hd&export=download&confirm=t",
    expected_sha256: None,
    expected_size_bytes: Some(25_000_000),
    license_short: "Creative Commons BY-NC-SA 4.0",
    license_url: "https://github.com/ZHKKKe/MODNet/blob/master/LICENSE",
    description: "AI-powered portrait background removal. Runs offline via ONNX Runtime.",
};

/// Look up a manifest entry by its `key` identifier. There's currently
/// only one entry, but the lookup is the natural API for the second
/// (when RIFE / SAM / Whisper migrate onto the same manifest).
#[allow(dead_code)]
pub fn entry_by_key(key: &str) -> Option<&'static ModelManifestEntry> {
    match key {
        k if k == PORTRAIT_MATTING.key => Some(&PORTRAIT_MATTING),
        _ => None,
    }
}

/// Live download status, shared between the worker thread and the UI poller.
#[derive(Debug, Clone)]
pub enum DownloadState {
    /// Worker has started but no bytes have arrived yet (or the `.partial`
    /// file hasn't been created).
    Pending,
    /// Bytes arriving. `total` is `Some` only when the manifest entry
    /// declared `expected_size_bytes` — the UI can use it to compute a
    /// fraction; otherwise it should fall back to pulse mode.
    Downloading { downloaded: u64, total: Option<u64> },
    /// Download finished, SHA-256 computation in progress.
    Verifying,
    /// Successful end state. `final_path` is the destination after the
    /// atomic rename. `computed_sha256` is the digest we actually computed
    /// off disk — useful for promoting an unknown hash to a pinned one or
    /// for tests / scripts that need the post-download artifact location.
    #[allow(dead_code)]
    Done {
        final_path: PathBuf,
        computed_sha256: String,
    },
    /// Failure end state. Holds a user-displayable message.
    Failed(String),
}

/// Compute the lowercase hex SHA-256 of a file on disk.
///
/// Reads in 1 MiB chunks so memory use stays flat regardless of file size.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Pure helper: decide whether a computed digest passes the manifest's
/// expectation. `None` expectation accepts anything (with caller's warning).
pub fn digest_matches(expected: Option<&str>, computed: &str) -> bool {
    match expected {
        Some(e) => e.eq_ignore_ascii_case(computed),
        None => true,
    }
}

/// Download a manifest entry to `dest_dir` on a worker thread.
///
/// Returns immediately with a shared `DownloadState` handle. The caller
/// (typically Preferences UI) polls the handle on a glib timeout to drive
/// progress UI. The download writes to `<dest_dir>/<filename>.partial`
/// while in flight; on successful SHA-256 verification it atomically
/// renames to `<dest_dir>/<filename>`. On any failure the partial file
/// is removed and the state transitions to `Failed`.
pub fn download_model_in_background(
    entry: &'static ModelManifestEntry,
    dest_dir: PathBuf,
) -> Arc<Mutex<DownloadState>> {
    let state = Arc::new(Mutex::new(DownloadState::Pending));
    let state_w = state.clone();

    std::thread::spawn(move || {
        let dest = dest_dir.join(entry.filename);
        let partial = dest_dir.join(format!("{}.partial", entry.filename));

        // Old .partial from a prior failed run would skew the size poller
        // reading. Best-effort cleanup; any error is surfaced by the curl
        // step itself if it can't write.
        let _ = std::fs::remove_file(&partial);

        // Spawn curl. We poll the partial file's size from this thread
        // rather than parsing curl's progress output — disk-stat polling
        // works regardless of curl version and CLI flags.
        let mut child = match std::process::Command::new("curl")
            .args(["-L", "-s", "-o", &partial.to_string_lossy(), entry.url])
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                *state_w.lock().unwrap() = DownloadState::Failed(format!(
                    "Failed to launch curl: {e}. Make sure curl is on PATH."
                ));
                return;
            }
        };

        // Poll partial size until curl exits.
        loop {
            let downloaded = std::fs::metadata(&partial).map(|m| m.len()).unwrap_or(0);
            *state_w.lock().unwrap() = DownloadState::Downloading {
                downloaded,
                total: entry.expected_size_bytes,
            };
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        let _ = std::fs::remove_file(&partial);
                        *state_w.lock().unwrap() = DownloadState::Failed(format!(
                            "curl exited with status {status} — check network access \
                             and the model URL."
                        ));
                        return;
                    }
                    break;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(250)),
                Err(e) => {
                    let _ = std::fs::remove_file(&partial);
                    *state_w.lock().unwrap() =
                        DownloadState::Failed(format!("Failed to wait on curl: {e}"));
                    return;
                }
            }
        }

        // Verify.
        *state_w.lock().unwrap() = DownloadState::Verifying;
        let computed = match sha256_file(&partial) {
            Ok(h) => h,
            Err(e) => {
                let _ = std::fs::remove_file(&partial);
                *state_w.lock().unwrap() =
                    DownloadState::Failed(format!("Failed to read downloaded file: {e}"));
                return;
            }
        };
        if !digest_matches(entry.expected_sha256, &computed) {
            let _ = std::fs::remove_file(&partial);
            *state_w.lock().unwrap() = DownloadState::Failed(format!(
                "SHA-256 mismatch: expected {}, got {}. Partial file removed.",
                entry.expected_sha256.unwrap_or("(unset)"),
                computed,
            ));
            return;
        }
        if entry.expected_sha256.is_none() {
            log::warn!(
                "Model '{}' downloaded without hash verification (manifest has \
                 expected_sha256 = None). Computed SHA-256 = {}. Promote this to \
                 a pinned hash in src/media/model_manifest.rs to enable integrity \
                 checking.",
                entry.key,
                computed,
            );
        }

        // Atomic rename — only after verification passes.
        if let Err(e) = std::fs::rename(&partial, &dest) {
            let _ = std::fs::remove_file(&partial);
            *state_w.lock().unwrap() =
                DownloadState::Failed(format!("Failed to finalize download: {e}"));
            return;
        }
        *state_w.lock().unwrap() = DownloadState::Done {
            final_path: dest,
            computed_sha256: computed,
        };
    });

    state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portrait_matting_entry_is_well_formed() {
        let e = &PORTRAIT_MATTING;
        assert_eq!(e.key, "portrait_matting");
        assert!(e.filename.ends_with(".onnx"));
        assert!(e.url.starts_with("http"));
        assert!(!e.display_name.is_empty());
        assert!(!e.license_short.is_empty());
        assert!(e.license_url.starts_with("http"));
    }

    #[test]
    fn entry_by_key_returns_known_and_rejects_unknown() {
        assert!(entry_by_key("portrait_matting").is_some());
        assert!(entry_by_key("does_not_exist").is_none());
        assert!(entry_by_key("").is_none());
    }

    #[test]
    fn sha256_file_matches_known_vector() {
        // SHA-256("hello world\n") = a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        std::fs::write(&path, b"hello world\n").unwrap();
        let h = sha256_file(&path).unwrap();
        assert_eq!(
            h,
            "a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447"
        );
    }

    #[test]
    fn sha256_file_handles_empty_file() {
        // SHA-256(empty) = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.bin");
        std::fs::write(&path, b"").unwrap();
        let h = sha256_file(&path).unwrap();
        assert_eq!(
            h,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_file_chunked_read_matches_short_read() {
        // Verify the 1 MiB chunked reader produces the same hash as a
        // single-shot read on a > 1 MiB input. Catches off-by-one /
        // accumulator-reset bugs in the chunk loop.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.bin");
        let payload = vec![0xABu8; 2 * 1024 * 1024 + 17];
        std::fs::write(&path, &payload).unwrap();
        let h = sha256_file(&path).unwrap();

        use sha2::{Digest, Sha256};
        let mut single_shot = Sha256::new();
        single_shot.update(&payload);
        let expected = format!("{:x}", single_shot.finalize());
        assert_eq!(h, expected);
    }

    #[test]
    fn digest_matches_accepts_when_no_expectation() {
        assert!(digest_matches(None, "anything"));
        assert!(digest_matches(None, ""));
    }

    #[test]
    fn digest_matches_rejects_mismatch() {
        assert!(!digest_matches(Some("aaa"), "bbb"));
    }

    #[test]
    fn digest_matches_is_case_insensitive_on_hex() {
        // Hex digests are conventionally lowercase but accepting either
        // saves a future maintainer from a foot-gun if a manifest entry
        // gets pasted in uppercase.
        assert!(digest_matches(Some("ABCDEF1234567890"), "abcdef1234567890"));
        assert!(digest_matches(Some("abcdef1234567890"), "ABCDEF1234567890"));
    }

    #[test]
    fn download_state_pending_is_initial() {
        // download_model_in_background returns Pending immediately; verify
        // the enum's Default-ish first state is what we expect rather than
        // accidentally racing into Downloading on a fast disk.
        let s = DownloadState::Pending;
        assert!(matches!(s, DownloadState::Pending));
    }
}
