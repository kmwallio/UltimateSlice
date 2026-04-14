use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::SystemTime;

/// Shared builder for media cache keys.
///
/// Most media caches key by some combination of source identity plus a small
/// number of rendering/inference parameters. Keeping the hashing entry points
/// here avoids each cache reimplementing its own `DefaultHasher` plumbing and
/// source-file fingerprint logic.
pub(crate) struct CacheKeyHasher(DefaultHasher);

impl CacheKeyHasher {
    pub(crate) fn new() -> Self {
        Self(DefaultHasher::new())
    }

    pub(crate) fn add<T: Hash>(&mut self, value: T) -> &mut Self {
        value.hash(&mut self.0);
        self
    }

    pub(crate) fn add_source_path(&mut self, source_path: &str) -> &mut Self {
        self.add(source_path)
    }

    /// Add the source path plus its current mtime (seconds since epoch).
    ///
    /// This is intentionally lightweight: it catches ordinary in-place source
    /// replacement without requiring a full content hash.
    pub(crate) fn add_source_fingerprint(&mut self, source_path: &str) -> &mut Self {
        self.add_source_path(source_path)
            .add(source_mtime_secs(source_path))
    }

    pub(crate) fn finish(self) -> u64 {
        self.0.finish()
    }
}

pub(crate) fn hashed_key(prefix: &str, build: impl FnOnce(&mut CacheKeyHasher)) -> String {
    let mut hasher = CacheKeyHasher::new();
    build(&mut hasher);
    format!("{prefix}_{:016x}", hasher.finish())
}

/// Read the source file's modification time as Unix seconds since epoch.
///
/// Returns 0 on any error (missing file, permissions, unsupported mtime).
pub(crate) fn source_mtime_secs(source_path: &str) -> u64 {
    std::fs::metadata(source_path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn hashed_key_is_stable_for_same_inputs() {
        let a = hashed_key("demo", |key| {
            key.add("source.mov").add(42_u32);
        });
        let b = hashed_key("demo", |key| {
            key.add("source.mov").add(42_u32);
        });
        assert_eq!(a, b);
        assert!(a.starts_with("demo_"));
    }

    #[test]
    fn source_mtime_secs_returns_zero_for_missing_path() {
        assert_eq!(
            source_mtime_secs("/definitely/missing/ultimateslice-cache-key"),
            0
        );
    }

    #[test]
    fn source_mtime_secs_reads_existing_file() {
        let temp = NamedTempFile::new().expect("temp file");
        assert!(source_mtime_secs(temp.path().to_string_lossy().as_ref()) > 0);
    }
}
