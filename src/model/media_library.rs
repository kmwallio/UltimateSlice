use crate::model::clip::ClipKind;
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
    pub collections: Vec<MediaCollection>,
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

    /// All smart collections.
    pub fn collections(&self) -> &[MediaCollection] {
        &self.collections
    }

    /// Find a bin by id.
    pub fn find_bin(&self, id: &str) -> Option<&MediaBin> {
        self.bins.iter().find(|b| b.id == id)
    }

    /// Find a smart collection by id.
    pub fn find_collection(&self, id: &str) -> Option<&MediaCollection> {
        self.collections.iter().find(|c| c.id == id)
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

    /// Items matching a smart collection.
    pub fn items_in_collection(&self, collection_id: &str) -> Vec<&MediaItem> {
        let Some(collection) = self.find_collection(collection_id) else {
            return Vec::new();
        };
        self.items
            .iter()
            .filter(|item| media_matches_filters(item, &collection.criteria))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaRating {
    #[default]
    None,
    Favorite,
    Reject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaRatingFilter {
    #[default]
    All,
    Favorite,
    Reject,
    Unrated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaKeywordRange {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub start_ns: u64,
    #[serde(default)]
    pub end_ns: u64,
}

impl MediaKeywordRange {
    pub fn new(label: impl Into<String>, start_ns: u64, end_ns: u64) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            label: label.into(),
            start_ns,
            end_ns,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKindFilter {
    #[default]
    All,
    Video,
    Audio,
    Image,
    Offline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionFilter {
    #[default]
    All,
    SdOrSmaller,
    Hd,
    FullHd,
    UltraHd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameRateFilter {
    #[default]
    All,
    Fps24OrLess,
    Fps25To30,
    Fps31To59,
    Fps60Plus,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MediaFilterCriteria {
    #[serde(default)]
    pub search_text: String,
    #[serde(default)]
    pub kind: MediaKindFilter,
    #[serde(default)]
    pub resolution: ResolutionFilter,
    #[serde(default)]
    pub frame_rate: FrameRateFilter,
    #[serde(default)]
    pub rating: MediaRatingFilter,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaCollection {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub criteria: MediaFilterCriteria,
}

impl MediaCollection {
    pub fn new(name: impl Into<String>, criteria: MediaFilterCriteria) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            criteria,
        }
    }
}

/// A media item in the project library.
///
/// Most entries are imported source files, but the browser can also surface
/// non-file-backed timeline clips (titles, adjustment layers, compounds,
/// multicam clips) using their clip id as the stable library key.
#[derive(Debug, Clone)]
pub struct MediaItem {
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
    /// True when the source is an animated SVG that should use the animated-image path.
    pub is_animated_svg: bool,
    /// Optional absolute source time reference for the start of the media.
    pub source_timecode_base_ns: Option<u64>,
    /// Video resolution when the source contains a video stream.
    pub video_width: Option<u32>,
    pub video_height: Option<u32>,
    /// Frame rate as a rational value when the source contains a video stream.
    pub frame_rate_num: Option<u32>,
    pub frame_rate_den: Option<u32>,
    /// Human-friendly codec summary derived from probe metadata.
    pub codec_summary: Option<String>,
    /// File size resolved from filesystem metadata.
    pub file_size_bytes: Option<u64>,
    /// Timeline-native clip kind when this item has no backing source file.
    pub clip_kind: Option<ClipKind>,
    /// Current title text for title clips shown in the media browser.
    pub title_text: Option<String>,
    /// True when the source file path cannot be resolved on disk.
    pub is_missing: bool,
    /// Bin this item belongs to (None = root level).
    pub bin_id: Option<String>,
    /// Editorial triage rating shown in the media browser.
    pub rating: MediaRating,
    /// Named sub-ranges within the source media for browser triage.
    pub keyword_ranges: Vec<MediaKeywordRange>,
}

impl MediaItem {
    pub fn new(source_path: impl Into<String>, duration_ns: u64) -> Self {
        let source_path = source_path.into();
        let is_missing = !source_path.is_empty() && !source_path_exists(&source_path);
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
            is_animated_svg: false,
            source_timecode_base_ns: None,
            video_width: None,
            video_height: None,
            frame_rate_num: None,
            frame_rate_den: None,
            codec_summary: None,
            file_size_bytes: None,
            clip_kind: None,
            title_text: None,
            is_missing,
            bin_id: None,
            rating: MediaRating::None,
            keyword_ranges: Vec::new(),
        }
    }

    pub fn has_backing_file(&self) -> bool {
        !self.source_path.is_empty()
    }

    pub fn library_key(&self) -> String {
        if self.has_backing_file() {
            self.source_path.clone()
        } else {
            format!("clip:{}", self.id)
        }
    }

    pub fn matches_library_key(&self, key: &str) -> bool {
        key.strip_prefix("clip:")
            .map(|clip_id| !self.has_backing_file() && self.id == clip_id)
            .unwrap_or_else(|| self.source_path == key)
    }
}

pub fn source_path_exists(source_path: &str) -> bool {
    std::fs::metadata(source_path).is_ok()
}

pub fn normalized_media_text(text: Option<&str>) -> Option<String> {
    let normalized = text?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" / ");
    (!normalized.is_empty()).then_some(normalized)
}

pub fn non_file_clip_kind_text(kind: &ClipKind) -> &'static str {
    match kind {
        ClipKind::Title => "Title clip",
        ClipKind::Adjustment => "Adjustment layer",
        ClipKind::Compound => "Compound clip",
        ClipKind::Multicam => "Multicam clip",
        ClipKind::Video => "Generated video clip",
        ClipKind::Audio => "Generated audio clip",
        ClipKind::Image => "Generated image clip",
    }
}

pub fn media_display_name(item: &MediaItem) -> String {
    if matches!(item.clip_kind, Some(ClipKind::Title)) {
        if let Some(text) = normalized_media_text(item.title_text.as_deref()) {
            return text;
        }
    }
    normalized_media_text(Some(item.label.as_str())).unwrap_or_else(|| {
        item.clip_kind
            .as_ref()
            .map(non_file_clip_kind_text)
            .unwrap_or("media")
            .to_string()
    })
}

pub fn media_max_dimension(item: &MediaItem) -> Option<u32> {
    item.video_width
        .zip(item.video_height)
        .map(|(w, h)| w.max(h))
}

pub fn media_frame_rate_value(item: &MediaItem) -> Option<f64> {
    let (num, den) = item.frame_rate_num.zip(item.frame_rate_den)?;
    if num == 0 || den == 0 {
        return None;
    }
    Some(num as f64 / den as f64)
}

pub fn media_rating_text(rating: MediaRating) -> Option<&'static str> {
    match rating {
        MediaRating::None => None,
        MediaRating::Favorite => Some("Favorite"),
        MediaRating::Reject => Some("Reject"),
    }
}

pub fn media_keyword_summary(item: &MediaItem, max_labels: usize) -> Option<String> {
    if item.keyword_ranges.is_empty() || max_labels == 0 {
        return None;
    }
    let mut labels: Vec<String> = item
        .keyword_ranges
        .iter()
        .map(|range| range.label.trim())
        .filter(|label| !label.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    labels.sort();
    labels.dedup();
    if labels.is_empty() {
        return None;
    }
    let extra = labels.len().saturating_sub(max_labels);
    labels.truncate(max_labels);
    if extra > 0 {
        labels.push(format!("+{extra}"));
    }
    Some(labels.join(" • "))
}

pub fn media_matches_filters(item: &MediaItem, filters: &MediaFilterCriteria) -> bool {
    if !matches_media_kind_filter(item, filters.kind) {
        return false;
    }
    if !matches_resolution_filter(item, filters.resolution) {
        return false;
    }
    if !matches_frame_rate_filter(item, filters.frame_rate) {
        return false;
    }
    if !matches_media_rating_filter(item, filters.rating) {
        return false;
    }
    if filters.search_text.trim().is_empty() {
        return true;
    }

    let needle = filters.search_text.trim().to_ascii_lowercase();
    media_display_name(item)
        .to_ascii_lowercase()
        .contains(&needle)
        || item.label.to_ascii_lowercase().contains(&needle)
        || item
            .title_text
            .as_ref()
            .is_some_and(|text| text.to_ascii_lowercase().contains(&needle))
        || item.source_path.to_ascii_lowercase().contains(&needle)
        || item
            .codec_summary
            .as_ref()
            .is_some_and(|codec| codec.to_ascii_lowercase().contains(&needle))
        || item
            .keyword_ranges
            .iter()
            .any(|range| range.label.trim().to_ascii_lowercase().contains(&needle))
}

pub fn matches_media_kind_filter(item: &MediaItem, filter: MediaKindFilter) -> bool {
    match filter {
        MediaKindFilter::All => true,
        MediaKindFilter::Video => !item.is_missing && !item.is_audio_only && !item.is_image,
        MediaKindFilter::Audio => !item.is_missing && item.is_audio_only,
        MediaKindFilter::Image => !item.is_missing && item.is_image,
        MediaKindFilter::Offline => item.is_missing,
    }
}

pub fn matches_resolution_filter(item: &MediaItem, filter: ResolutionFilter) -> bool {
    match filter {
        ResolutionFilter::All => true,
        ResolutionFilter::SdOrSmaller => media_max_dimension(item).is_some_and(|dim| dim <= 720),
        ResolutionFilter::Hd => {
            media_max_dimension(item).is_some_and(|dim| (721..=1280).contains(&dim))
        }
        ResolutionFilter::FullHd => {
            media_max_dimension(item).is_some_and(|dim| (1281..=1920).contains(&dim))
        }
        ResolutionFilter::UltraHd => media_max_dimension(item).is_some_and(|dim| dim >= 1921),
    }
}

pub fn matches_frame_rate_filter(item: &MediaItem, filter: FrameRateFilter) -> bool {
    match filter {
        FrameRateFilter::All => true,
        FrameRateFilter::Fps24OrLess => media_frame_rate_value(item).is_some_and(|fps| fps <= 24.0),
        FrameRateFilter::Fps25To30 => media_frame_rate_value(item)
            .is_some_and(|fps| (24.0..=30.0).contains(&fps) && fps > 24.0),
        FrameRateFilter::Fps31To59 => media_frame_rate_value(item)
            .is_some_and(|fps| (30.0..60.0).contains(&fps) && fps > 30.0),
        FrameRateFilter::Fps60Plus => media_frame_rate_value(item).is_some_and(|fps| fps >= 60.0),
    }
}

pub fn matches_media_rating_filter(item: &MediaItem, filter: MediaRatingFilter) -> bool {
    match filter {
        MediaRatingFilter::All => true,
        MediaRatingFilter::Favorite => item.rating == MediaRating::Favorite,
        MediaRatingFilter::Reject => item.rating == MediaRating::Reject,
        MediaRatingFilter::Unrated => item.rating == MediaRating::None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct SavedMediaAnnotations {
    #[serde(default)]
    rating: MediaRating,
    #[serde(default)]
    keyword_ranges: Vec<MediaKeywordRange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SavedLibraryItem {
    pub source_path: String,
    #[serde(default)]
    pub duration_ns: u64,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub is_audio_only: bool,
    #[serde(default)]
    pub has_audio: bool,
    #[serde(default)]
    pub is_image: bool,
    #[serde(default)]
    pub is_animated_svg: bool,
    #[serde(default)]
    pub source_timecode_base_ns: Option<u64>,
    #[serde(default)]
    pub video_width: Option<u32>,
    #[serde(default)]
    pub video_height: Option<u32>,
    #[serde(default)]
    pub frame_rate_num: Option<u32>,
    #[serde(default)]
    pub frame_rate_den: Option<u32>,
    #[serde(default)]
    pub codec_summary: Option<String>,
    #[serde(default)]
    pub file_size_bytes: Option<u64>,
}

impl SavedLibraryItem {
    fn from_media_item(item: &MediaItem) -> Option<Self> {
        item.has_backing_file().then(|| Self {
            source_path: item.source_path.clone(),
            duration_ns: item.duration_ns,
            label: item.label.clone(),
            is_audio_only: item.is_audio_only,
            has_audio: item.has_audio,
            is_image: item.is_image,
            is_animated_svg: item.is_animated_svg,
            source_timecode_base_ns: item.source_timecode_base_ns,
            video_width: item.video_width,
            video_height: item.video_height,
            frame_rate_num: item.frame_rate_num,
            frame_rate_den: item.frame_rate_den,
            codec_summary: item.codec_summary.clone(),
            file_size_bytes: item.file_size_bytes,
        })
    }

    fn apply_to_item(&self, item: &mut MediaItem) {
        item.duration_ns = self.duration_ns;
        item.label = if self.label.trim().is_empty() {
            item.label.clone()
        } else {
            self.label.clone()
        };
        item.is_audio_only = self.is_audio_only;
        item.has_audio = self.has_audio;
        item.is_image = self.is_image;
        item.is_animated_svg = self.is_animated_svg;
        item.source_timecode_base_ns = self.source_timecode_base_ns;
        item.video_width = self.video_width;
        item.video_height = self.video_height;
        item.frame_rate_num = self.frame_rate_num;
        item.frame_rate_den = self.frame_rate_den;
        item.codec_summary = self.codec_summary.clone();
        item.file_size_bytes = self.file_size_bytes;
        item.is_missing = source_path_exists(&item.source_path);
        item.is_missing = !item.is_missing;
    }

    fn into_media_item(self) -> MediaItem {
        let mut item = MediaItem::new(self.source_path, self.duration_ns);
        if !self.label.trim().is_empty() {
            item.label = self.label;
        }
        item.is_audio_only = self.is_audio_only;
        item.has_audio = self.has_audio;
        item.is_image = self.is_image;
        item.is_animated_svg = self.is_animated_svg;
        item.source_timecode_base_ns = self.source_timecode_base_ns;
        item.video_width = self.video_width;
        item.video_height = self.video_height;
        item.frame_rate_num = self.frame_rate_num;
        item.frame_rate_den = self.frame_rate_den;
        item.codec_summary = self.codec_summary;
        item.file_size_bytes = self.file_size_bytes;
        item
    }
}

/// Serialize media-browser state from the library into the project's transient fields
/// (for FCPXML save).
pub fn sync_bins_to_project(lib: &MediaLibrary, project: &mut crate::model::project::Project) {
    if lib.bins.is_empty() {
        project.parsed_bins_json = None;
    } else {
        project.parsed_bins_json = serde_json::to_string(&lib.bins).ok();
    }
    let media_bins: std::collections::HashMap<String, String> = lib
        .items
        .iter()
        .filter_map(|i| i.bin_id.as_ref().map(|bid| (i.library_key(), bid.clone())))
        .collect();
    if media_bins.is_empty() {
        project.parsed_media_bins_json = None;
    } else {
        project.parsed_media_bins_json = serde_json::to_string(&media_bins).ok();
    }
    if lib.collections.is_empty() {
        project.parsed_collections_json = None;
    } else {
        project.parsed_collections_json = serde_json::to_string(&lib.collections).ok();
    }
    let library_items: Vec<SavedLibraryItem> = lib
        .items
        .iter()
        .filter_map(SavedLibraryItem::from_media_item)
        .collect();
    if library_items.is_empty() {
        project.parsed_library_items_json = None;
    } else {
        project.parsed_library_items_json = serde_json::to_string(&library_items).ok();
    }
    let annotations: std::collections::HashMap<String, SavedMediaAnnotations> = lib
        .items
        .iter()
        .filter_map(|item| {
            ((item.rating != MediaRating::None) || !item.keyword_ranges.is_empty()).then(|| {
                (
                    item.library_key(),
                    SavedMediaAnnotations {
                        rating: item.rating,
                        keyword_ranges: item.keyword_ranges.clone(),
                    },
                )
            })
        })
        .collect();
    if annotations.is_empty() {
        project.parsed_media_annotations_json = None;
    } else {
        project.parsed_media_annotations_json = serde_json::to_string(&annotations).ok();
    }
}

/// Restore media-browser state from the project's transient fields into the library
/// (after FCPXML load).
pub fn apply_bins_from_project(
    lib: &mut MediaLibrary,
    project: &mut crate::model::project::Project,
) {
    if let Some(ref library_items_json) = project.parsed_library_items_json {
        if let Ok(saved_items) = serde_json::from_str::<Vec<SavedLibraryItem>>(library_items_json) {
            for saved in saved_items {
                if let Some(item) = lib
                    .items
                    .iter_mut()
                    .find(|item| item.matches_library_key(&saved.source_path))
                {
                    saved.apply_to_item(item);
                } else {
                    lib.items.push(saved.into_media_item());
                }
            }
        }
    }
    if let Some(ref bins_json) = project.parsed_bins_json {
        if let Ok(bins) = serde_json::from_str::<Vec<MediaBin>>(bins_json) {
            lib.bins = bins;
        }
    }
    if let Some(ref media_bins_json) = project.parsed_media_bins_json {
        if let Ok(map) =
            serde_json::from_str::<std::collections::HashMap<String, String>>(media_bins_json)
        {
            for item in lib.items.iter_mut() {
                let bin_id = map.get(&item.library_key()).or_else(|| {
                    map.iter()
                        .find_map(|(key, value)| item.matches_library_key(key).then_some(value))
                });
                if let Some(bin_id) = bin_id {
                    // Only assign if the bin actually exists
                    if lib.bins.iter().any(|b| &b.id == bin_id) {
                        item.bin_id = Some(bin_id.clone());
                    }
                }
            }
        }
    }
    if let Some(ref collections_json) = project.parsed_collections_json {
        if let Ok(collections) = serde_json::from_str::<Vec<MediaCollection>>(collections_json) {
            lib.collections = collections;
        }
    }
    if let Some(ref annotations_json) = project.parsed_media_annotations_json {
        if let Ok(map) = serde_json::from_str::<
            std::collections::HashMap<String, SavedMediaAnnotations>,
        >(annotations_json)
        {
            for item in lib.items.iter_mut() {
                let annotations = map.get(&item.library_key()).or_else(|| {
                    map.iter()
                        .find_map(|(key, value)| item.matches_library_key(key).then_some(value))
                });
                if let Some(annotations) = annotations {
                    item.rating = annotations.rating;
                    item.keyword_ranges = annotations.keyword_ranges.clone();
                }
            }
        }
    }
    // Clear transient fields
    project.parsed_bins_json = None;
    project.parsed_media_bins_json = None;
    project.parsed_collections_json = None;
    project.parsed_library_items_json = None;
    project.parsed_media_annotations_json = None;
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
    /// True when the loaded source is an animated SVG.
    pub is_animated_svg: bool,
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
            is_animated_svg: false,
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
            is_animated_svg: false,
            source_timecode_base_ns: None,
            video_width: None,
            video_height: None,
            frame_rate_num: None,
            frame_rate_den: None,
            codec_summary: None,
            file_size_bytes: None,
            clip_kind: None,
            title_text: None,
            is_missing: true, // test paths don't exist
            bin_id,
            rating: MediaRating::None,
            keyword_ranges: Vec::new(),
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

    #[test]
    fn test_library_key_uses_clip_id_for_non_file_items() {
        let mut item = MediaItem::new("", 2_000_000_000);
        item.id = "title-123".to_string();
        item.clip_kind = Some(ClipKind::Title);

        assert_eq!(item.library_key(), "clip:title-123");
        assert!(item.matches_library_key("clip:title-123"));
    }

    #[test]
    fn test_sync_bins_round_trips_non_file_items() {
        let bin = make_bin("Generated", None);
        let mut lib = MediaLibrary::new();
        lib.bins.push(bin.clone());

        let mut file_item = make_item("/tmp/a.mp4", Some(bin.id.clone()));
        file_item.is_missing = false;
        lib.items.push(file_item);

        let mut title_item = MediaItem::new("", 4_000_000_000);
        title_item.id = "title-clip".to_string();
        title_item.label = "Lower Third".to_string();
        title_item.clip_kind = Some(ClipKind::Title);
        title_item.title_text = Some("Jane Doe".to_string());
        title_item.bin_id = Some(bin.id.clone());
        lib.items.push(title_item.clone());

        let mut project = crate::model::project::Project::new("Test");
        sync_bins_to_project(&lib, &mut project);

        let bins_json = project
            .parsed_media_bins_json
            .as_ref()
            .expect("media bins should be serialized");
        assert!(bins_json.contains("clip:title-clip"));

        let mut restored = MediaLibrary::new();
        restored.bins.push(bin.clone());

        let mut restored_file = make_item("/tmp/a.mp4", None);
        restored_file.is_missing = false;
        restored.items.push(restored_file);

        title_item.bin_id = None;
        restored.items.push(title_item);

        apply_bins_from_project(&mut restored, &mut project);

        assert_eq!(restored.items[0].bin_id.as_deref(), Some(bin.id.as_str()));
        assert_eq!(restored.items[1].bin_id.as_deref(), Some(bin.id.as_str()));
    }

    #[test]
    fn test_media_matches_filters_by_title_text_and_frame_rate() {
        let mut title = MediaItem::new("", 4_000_000_000);
        title.label = "Lower Third".to_string();
        title.clip_kind = Some(ClipKind::Title);
        title.title_text = Some("Jane Doe".to_string());
        title.is_missing = false;
        assert!(media_matches_filters(
            &title,
            &MediaFilterCriteria {
                search_text: "jane".to_string(),
                ..Default::default()
            }
        ));

        let mut clip = MediaItem::new("/tmp/clip.mov", 1_000_000_000);
        clip.is_missing = false;
        clip.frame_rate_num = Some(60000);
        clip.frame_rate_den = Some(1000);
        assert!(media_matches_filters(
            &clip,
            &MediaFilterCriteria {
                frame_rate: FrameRateFilter::Fps60Plus,
                ..Default::default()
            }
        ));
        assert!(!media_matches_filters(
            &clip,
            &MediaFilterCriteria {
                frame_rate: FrameRateFilter::Fps24OrLess,
                ..Default::default()
            }
        ));
    }

    #[test]
    fn test_media_matches_filters_by_rating_and_keyword() {
        let mut item = MediaItem::new("/tmp/clip.mov", 1_000_000_000);
        item.is_missing = false;
        item.rating = MediaRating::Favorite;
        item.keyword_ranges
            .push(MediaKeywordRange::new("Close Up", 100, 200));

        assert!(media_matches_filters(
            &item,
            &MediaFilterCriteria {
                rating: MediaRatingFilter::Favorite,
                ..Default::default()
            }
        ));
        assert!(!media_matches_filters(
            &item,
            &MediaFilterCriteria {
                rating: MediaRatingFilter::Reject,
                ..Default::default()
            }
        ));
        assert!(media_matches_filters(
            &item,
            &MediaFilterCriteria {
                search_text: "close".to_string(),
                ..Default::default()
            }
        ));
    }

    #[test]
    fn test_sync_bins_round_trips_collections() {
        let mut lib = MediaLibrary::new();
        lib.collections.push(MediaCollection::new(
            "4K clips",
            MediaFilterCriteria {
                resolution: ResolutionFilter::UltraHd,
                frame_rate: FrameRateFilter::Fps31To59,
                ..Default::default()
            },
        ));

        let mut project = crate::model::project::Project::new("Test");
        sync_bins_to_project(&lib, &mut project);
        assert!(project
            .parsed_collections_json
            .as_ref()
            .is_some_and(|json| json.contains("4K clips")));

        let mut restored = MediaLibrary::new();
        apply_bins_from_project(&mut restored, &mut project);
        assert_eq!(restored.collections.len(), 1);
        assert_eq!(restored.collections[0].name, "4K clips");
        assert_eq!(
            restored.collections[0].criteria.frame_rate,
            FrameRateFilter::Fps31To59
        );
    }

    #[test]
    fn test_sync_bins_round_trips_library_items_and_annotations() {
        let mut lib = MediaLibrary::new();
        let mut item = MediaItem::new("/tmp/rated.mov", 2_000_000_000);
        item.label = "Rated".to_string();
        item.rating = MediaRating::Favorite;
        item.keyword_ranges
            .push(MediaKeywordRange::new("B-roll", 250_000_000, 900_000_000));
        lib.items.push(item);

        let mut project = crate::model::project::Project::new("Test");
        sync_bins_to_project(&lib, &mut project);

        assert!(project
            .parsed_library_items_json
            .as_ref()
            .is_some_and(|json| json.contains("/tmp/rated.mov")));
        assert!(project
            .parsed_media_annotations_json
            .as_ref()
            .is_some_and(|json| json.contains("B-roll")));

        let mut restored = MediaLibrary::new();
        apply_bins_from_project(&mut restored, &mut project);

        assert_eq!(restored.items.len(), 1);
        assert_eq!(restored.items[0].source_path, "/tmp/rated.mov");
        assert_eq!(restored.items[0].rating, MediaRating::Favorite);
        assert_eq!(restored.items[0].keyword_ranges.len(), 1);
        assert_eq!(restored.items[0].keyword_ranges[0].label, "B-roll");
    }
}
