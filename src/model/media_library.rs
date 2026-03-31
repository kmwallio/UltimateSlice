use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A named folder for organizing media items in the library.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaBin {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
}

impl MediaBin {
    pub fn new(name: impl Into<String>, parent_id: Option<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            parent_id,
        }
    }

    /// Depth from root (root bins = 0, children = 1). Max enforced depth is 2.
    pub fn depth(&self, bins: &[MediaBin]) -> usize {
        match &self.parent_id {
            None => 0,
            Some(pid) => bins
                .iter()
                .find(|b| b.id == *pid)
                .map(|parent| 1 + parent.depth(bins))
                .unwrap_or(0),
        }
    }
}

/// The full media library: items + bin hierarchy.
#[derive(Debug, Clone, Default)]
pub struct MediaLibrary {
    pub items: Vec<MediaItem>,
    pub bins: Vec<MediaBin>,
}

impl MediaLibrary {
    pub fn new() -> Self {
        Self::default()
    }

    /// Items in a specific bin (or root when bin_id is None).
    pub fn items_in_bin(&self, bin_id: Option<&str>) -> Vec<&MediaItem> {
        self.items
            .iter()
            .filter(|i| i.bin_id.as_deref() == bin_id)
            .collect()
    }

    /// Direct child bins of a parent (or root-level bins when parent_id is None).
    pub fn child_bins(&self, parent_id: Option<&str>) -> Vec<&MediaBin> {
        self.bins
            .iter()
            .filter(|b| b.parent_id.as_deref() == parent_id)
            .collect()
    }

    /// All items regardless of bin.
    pub fn all_items(&self) -> &[MediaItem] {
        &self.items
    }

    /// Find a bin by id.
    pub fn find_bin(&self, id: &str) -> Option<&MediaBin> {
        self.bins.iter().find(|b| b.id == id)
    }

    /// Build the ancestor chain for breadcrumb display (root-first order).
    pub fn bin_ancestors(&self, bin_id: &str) -> Vec<&MediaBin> {
        let mut chain = Vec::new();
        let mut current = bin_id;
        while let Some(bin) = self.find_bin(current) {
            chain.push(bin);
            match &bin.parent_id {
                Some(pid) => current = pid,
                None => break,
            }
        }
        chain.reverse();
        chain
    }
}

/// A media item in the project library — not yet placed on the timeline.
#[derive(Debug, Clone)]
pub struct MediaItem {
    #[allow(dead_code)]
    pub id: String,
    pub source_path: String,
    pub duration_ns: u64,
    pub label: String,
    /// True when the file has no video streams (audio-only).
    /// Set asynchronously after background probe completes.
    pub is_audio_only: bool,
    /// True when the file has at least one audio stream.
    /// Set asynchronously after background probe completes.
    pub has_audio: bool,
    /// True when the file is a still image (PNG, JPEG, etc.).
    /// Set asynchronously after background probe completes.
    pub is_image: bool,
    /// Optional absolute source time reference for the start of the media.
    pub source_timecode_base_ns: Option<u64>,
    /// True when the source file path cannot be resolved on disk.
    pub is_missing: bool,
    /// Bin this item belongs to (None = root level).
    pub bin_id: Option<String>,
}

impl MediaItem {
    pub fn new(source_path: impl Into<String>, duration_ns: u64) -> Self {
        let source_path = source_path.into();
        let is_missing = !source_path_exists(&source_path);
        let label = std::path::Path::new(&source_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("media")
            .to_string();
        Self {
            id: Uuid::new_v4().to_string(),
            source_path,
            duration_ns,
            label,
            is_audio_only: false,
            has_audio: false,
            is_image: false,
            source_timecode_base_ns: None,
            is_missing,
            bin_id: None,
        }
    }
}

pub fn source_path_exists(source_path: &str) -> bool {
    std::fs::metadata(source_path).is_ok()
}

/// Serialize bin data from the library into the project's transient fields (for FCPXML save).
pub fn sync_bins_to_project(lib: &MediaLibrary, project: &mut crate::model::project::Project) {
    if lib.bins.is_empty() {
        project.parsed_bins_json = None;
        project.parsed_media_bins_json = None;
        return;
    }
    project.parsed_bins_json = serde_json::to_string(&lib.bins).ok();
    let media_bins: std::collections::HashMap<String, String> = lib
        .items
        .iter()
        .filter_map(|i| i.bin_id.as_ref().map(|bid| (i.source_path.clone(), bid.clone())))
        .collect();
    if media_bins.is_empty() {
        project.parsed_media_bins_json = None;
    } else {
        project.parsed_media_bins_json = serde_json::to_string(&media_bins).ok();
    }
}

/// Restore bin data from the project's transient fields into the library (after FCPXML load).
pub fn apply_bins_from_project(lib: &mut MediaLibrary, project: &mut crate::model::project::Project) {
    if let Some(ref bins_json) = project.parsed_bins_json {
        if let Ok(bins) = serde_json::from_str::<Vec<MediaBin>>(bins_json) {
            lib.bins = bins;
        }
    }
    if let Some(ref media_bins_json) = project.parsed_media_bins_json {
        if let Ok(map) = serde_json::from_str::<std::collections::HashMap<String, String>>(media_bins_json) {
            for item in lib.items.iter_mut() {
                if let Some(bin_id) = map.get(&item.source_path) {
                    // Only assign if the bin actually exists
                    if lib.bins.iter().any(|b| &b.id == bin_id) {
                        item.bin_id = Some(bin_id.clone());
                    }
                }
            }
        }
    }
    // Clear transient fields
    project.parsed_bins_json = None;
    project.parsed_media_bins_json = None;
}

/// In/out marks and current source for the source preview monitor.
#[derive(Debug, Clone)]
pub struct SourceMarks {
    /// Filesystem path of the currently loaded source clip
    pub path: String,
    /// Total duration of the loaded source
    pub duration_ns: u64,
    /// In-point (start of selection), in nanoseconds
    pub in_ns: u64,
    /// Out-point (end of selection), in nanoseconds
    pub out_ns: u64,
    /// Last position explicitly seeked to — used as display position while
    /// GStreamer is still pre-rolling (query_position returns 0 during that time).
    pub display_pos_ns: u64,
    /// Frame duration in nanoseconds (default 24 fps ≈ 41_666_667 ns).
    /// Used for frame-accurate jog/shuttle stepping.
    pub frame_ns: u64,
    /// True when the loaded source has no video streams (audio file).
    pub is_audio_only: bool,
    /// True when the loaded source has at least one audio stream.
    pub has_audio: bool,
    /// True when the loaded source is a still image.
    pub is_image: bool,
    /// Optional absolute source time reference for the start of the loaded media.
    pub source_timecode_base_ns: Option<u64>,
}

impl Default for SourceMarks {
    fn default() -> Self {
        Self {
            path: String::new(),
            duration_ns: 0,
            in_ns: 0,
            out_ns: 0,
            display_pos_ns: 0,
            frame_ns: 41_666_667, // 24 fps default
            is_audio_only: false,
            has_audio: false,
            is_image: false,
            source_timecode_base_ns: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_bin(name: &str, parent_id: Option<String>) -> MediaBin {
        MediaBin::new(name, parent_id)
    }

    fn make_item(path: &str, bin_id: Option<String>) -> MediaItem {
        MediaItem {
            id: Uuid::new_v4().to_string(),
            source_path: path.to_string(),
            duration_ns: 1_000_000_000,
            label: path.to_string(),
            is_audio_only: false,
            has_audio: true,
            is_image: false,
            source_timecode_base_ns: None,
            is_missing: true, // test paths don't exist
            bin_id,
        }
    }

    #[test]
    fn test_bin_depth_root() {
        let bin = make_bin("Root Bin", None);
        assert_eq!(bin.depth(&[bin.clone()]), 0);
    }

    #[test]
    fn test_bin_depth_nested() {
        let root = make_bin("Root", None);
        let child = make_bin("Child", Some(root.id.clone()));
        let bins = vec![root.clone(), child.clone()];
        assert_eq!(root.depth(&bins), 0);
        assert_eq!(child.depth(&bins), 1);
    }

    #[test]
    fn test_bin_depth_two_levels() {
        let root = make_bin("Root", None);
        let child = make_bin("Child", Some(root.id.clone()));
        let grandchild = make_bin("Grandchild", Some(child.id.clone()));
        let bins = vec![root.clone(), child.clone(), grandchild.clone()];
        assert_eq!(grandchild.depth(&bins), 2);
    }

    #[test]
    fn test_items_in_bin() {
        let bin = make_bin("Footage", None);
        let mut lib = MediaLibrary::new();
        lib.bins.push(bin.clone());
        lib.items.push(make_item("a.mp4", None));
        lib.items.push(make_item("b.mp4", Some(bin.id.clone())));
        lib.items.push(make_item("c.mp4", Some(bin.id.clone())));

        let root_items = lib.items_in_bin(None);
        assert_eq!(root_items.len(), 1);
        assert_eq!(root_items[0].source_path, "a.mp4");

        let bin_items = lib.items_in_bin(Some(&bin.id));
        assert_eq!(bin_items.len(), 2);
    }

    #[test]
    fn test_child_bins() {
        let root = make_bin("Root", None);
        let child1 = make_bin("Child1", Some(root.id.clone()));
        let child2 = make_bin("Child2", Some(root.id.clone()));
        let other = make_bin("Other", None);
        let mut lib = MediaLibrary::new();
        lib.bins = vec![root.clone(), child1, child2, other];

        let root_children = lib.child_bins(Some(&root.id));
        assert_eq!(root_children.len(), 2);

        let top_level = lib.child_bins(None);
        assert_eq!(top_level.len(), 2); // root + other
    }

    #[test]
    fn test_bin_ancestors() {
        let root = make_bin("Root", None);
        let child = make_bin("Child", Some(root.id.clone()));
        let grandchild = make_bin("Grandchild", Some(child.id.clone()));
        let mut lib = MediaLibrary::new();
        lib.bins = vec![root.clone(), child.clone(), grandchild.clone()];

        let ancestors = lib.bin_ancestors(&grandchild.id);
        assert_eq!(ancestors.len(), 3);
        assert_eq!(ancestors[0].id, root.id);
        assert_eq!(ancestors[1].id, child.id);
        assert_eq!(ancestors[2].id, grandchild.id);
    }

    #[test]
    fn test_find_bin() {
        let bin = make_bin("Test", None);
        let mut lib = MediaLibrary::new();
        lib.bins.push(bin.clone());

        assert!(lib.find_bin(&bin.id).is_some());
        assert!(lib.find_bin("nonexistent").is_none());
    }
}
