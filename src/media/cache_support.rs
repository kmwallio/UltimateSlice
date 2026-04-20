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
    fn file_has_content_detects_nonempty_file() {
        let temp = NamedTempFile::new().expect("temp file");
        std::fs::write(temp.path(), b"data").expect("write temp file");
        assert!(file_has_content(temp.path().to_string_lossy().as_ref()));
    }
}
