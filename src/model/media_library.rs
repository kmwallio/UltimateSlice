use crate::model::clip::{AudioChannelMode, ClipKind, SubtitleSegment};
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

/// Speech-to-text transcript captured for a specific source window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaTranscriptWindow {
    #[serde(default)]
    pub source_in_ns: u64,
    #[serde(default)]
    pub source_out_ns: u64,
    #[serde(default)]
    pub segments: Vec<SubtitleSegment>,
}

impl MediaTranscriptWindow {
    pub fn new(source_in_ns: u64, source_out_ns: u64, segments: Vec<SubtitleSegment>) -> Self {
        Self {
            source_in_ns,
            source_out_ns,
            segments,
        }
    }
}

/// CLIP-style visual-search embedding captured for a representative frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaVisualEmbeddingFrame {
    #[serde(default)]
    pub time_ns: u64,
    #[serde(default)]
    pub embedding: Vec<f32>,
}

/// Runtime visual-search embedding state for a media item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaVisualEmbedding {
    #[serde(default)]
    pub model_id: String,
    #[serde(default)]
    pub frames: Vec<MediaVisualEmbeddingFrame>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaAutoTagCategory {
    ShotType,
    Setting,
    TimeOfDay,
    Subject,
}

impl MediaAutoTagCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::ShotType => "Shot type",
            Self::Setting => "Setting",
            Self::TimeOfDay => "Time of day",
            Self::Subject => "Subject",
        }
    }

    pub fn search_text(self) -> &'static str {
        match self {
            Self::ShotType => "shot type shot framing",
            Self::Setting => "setting location scene",
            Self::TimeOfDay => "time of day daytime nighttime",
            Self::Subject => "subject object people",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaAutoTag {
    pub category: MediaAutoTagCategory,
    pub label: String,
    #[serde(default)]
    pub confidence: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_frame_time_ns: Option<u64>,
}

impl MediaAutoTag {
    pub fn new(
        category: MediaAutoTagCategory,
        label: impl Into<String>,
        confidence: f32,
        best_frame_time_ns: Option<u64>,
    ) -> Option<Self> {
        let label = label.into().trim().to_string();
        (!label.is_empty()).then_some(Self {
            category,
            label,
            confidence: confidence.clamp(0.0, 1.0),
            best_frame_time_ns,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaSearchField {
    DisplayName,
    Label,
    TitleText,
    SourcePath,
    Codec,
    Keyword,
    AutoTag,
    Transcript,
    Visual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaSearchMatch {
    pub field: MediaSearchField,
    pub score: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_in_ns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_out_ns: Option<u64>,
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
    /// Detected HDR colorimetry label (e.g. "bt2100-pq", "bt2100-hlg") when
    /// the source video uses a high dynamic range transfer function.
    pub hdr_colorimetry: Option<String>,
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
    /// Persistent semantic tags inferred from the visual search embedding.
    pub auto_tags: Vec<MediaAutoTag>,
    /// True once contextual auto-tagging has run for this item, even if no
    /// tags cleared the confidence threshold.
    pub auto_tags_indexed: bool,
    /// Library-keyed transcript windows generated from speech-to-text runs.
    pub transcript_windows: Vec<MediaTranscriptWindow>,
    /// Runtime-only CLIP-style frame embeddings for semantic visual search.
    pub visual_embedding: Option<MediaVisualEmbedding>,
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
            hdr_colorimetry: None,
            file_size_bytes: None,
            clip_kind: None,
            title_text: None,
            is_missing,
            bin_id: None,
            rating: MediaRating::None,
            keyword_ranges: Vec::new(),
            auto_tags: Vec::new(),
            auto_tags_indexed: false,
            transcript_windows: Vec::new(),
            visual_embedding: None,
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

    pub fn upsert_transcript_window(
        &mut self,
        source_in_ns: u64,
        source_out_ns: u64,
        mut segments: Vec<SubtitleSegment>,
    ) -> bool {
        if segments.is_empty() {
            return false;
        }
        segments.sort_by_key(|segment| (segment.start_ns, segment.end_ns));
        let normalized_out =
            normalize_transcript_window_end(source_in_ns, source_out_ns, &segments);
        if let Some(existing) = self.transcript_windows.iter_mut().find(|window| {
            window.source_in_ns == source_in_ns && window.source_out_ns == normalized_out
        }) {
            if existing.segments == segments {
                return false;
            }
            existing.segments = segments;
            return true;
        }
        self.transcript_windows.push(MediaTranscriptWindow::new(
            source_in_ns,
            normalized_out,
            segments,
        ));
        self.transcript_windows
            .sort_by_key(|window| (window.source_in_ns, window.source_out_ns));
        true
    }

    pub fn upsert_visual_embedding(&mut self, embedding: MediaVisualEmbedding) -> bool {
        if embedding.frames.is_empty() {
            return false;
        }
        if self.visual_embedding.as_ref() == Some(&embedding) {
            return false;
        }
        self.visual_embedding = Some(embedding);
        true
    }

    pub fn upsert_auto_tags(&mut self, auto_tags: Vec<MediaAutoTag>) -> bool {
        let mut normalized = auto_tags
            .into_iter()
            .filter_map(|tag| {
                MediaAutoTag::new(
                    tag.category,
                    tag.label,
                    tag.confidence,
                    tag.best_frame_time_ns,
                )
            })
            .collect::<Vec<_>>();
        normalized.sort_by(|a, b| {
            (a.category as u8)
                .cmp(&(b.category as u8))
                .then_with(|| {
                    b.confidence
                        .partial_cmp(&a.confidence)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| {
                    a.label
                        .to_ascii_lowercase()
                        .cmp(&b.label.to_ascii_lowercase())
                })
        });
        normalized
            .dedup_by(|a, b| a.category == b.category && a.label.eq_ignore_ascii_case(&b.label));
        if self.auto_tags == normalized && self.auto_tags_indexed {
            return false;
        }
        self.auto_tags = normalized;
        self.auto_tags_indexed = true;
        true
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
        ClipKind::Audition => "Audition clip",
        ClipKind::Video => "Generated video clip",
        ClipKind::Audio => "Generated audio clip",
        ClipKind::Image => "Generated image clip",
        ClipKind::Drawing => "Drawing overlay",
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

pub fn media_auto_tag_summary(item: &MediaItem, max_tags: usize) -> Option<String> {
    if item.auto_tags.is_empty() || max_tags == 0 {
        return None;
    }
    let mut labels: Vec<String> = item
        .auto_tags
        .iter()
        .map(|tag| tag.label.trim())
        .filter(|label| !label.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    labels.sort();
    labels.dedup();
    if labels.is_empty() {
        return None;
    }
    let extra = labels.len().saturating_sub(max_tags);
    labels.truncate(max_tags);
    if extra > 0 {
        labels.push(format!("+{extra}"));
    }
    Some(labels.join(" • "))
}

pub fn media_auto_tag_state_key(item: &MediaItem) -> String {
    std::iter::once(format!("indexed:{}", item.auto_tags_indexed))
        .chain(item.auto_tags.iter().map(|tag| {
            format!(
                "{:?}:{}:{:.3}:{}",
                tag.category,
                tag.label,
                tag.confidence,
                tag.best_frame_time_ns.unwrap_or_default()
            )
        }))
        .collect::<Vec<_>>()
        .join("|")
}

fn media_auto_tag_search_excerpt(item: &MediaItem, query: &SearchQuery) -> Option<String> {
    let mut best: Option<(i32, &MediaAutoTag)> = None;
    for tag in &item.auto_tags {
        let tag_text = format!("{} {}", tag.category.search_text(), tag.label);
        let Some(score) = score_search_text(tag_text.as_str(), query, 860, 720) else {
            continue;
        };
        let replace = best
            .as_ref()
            .map(|(current, _)| score > *current)
            .unwrap_or(true);
        if replace {
            best = Some((score, tag));
        }
    }
    best.map(|(_, tag)| {
        format!(
            "{}: {} ({:.0}%)",
            tag.category.label(),
            highlight_search_excerpt(tag.label.as_str(), query),
            tag.confidence * 100.0
        )
    })
}

pub fn media_transcript_state_key(item: &MediaItem) -> String {
    item.transcript_windows
        .iter()
        .map(|window| {
            let segments = window
                .segments
                .iter()
                .map(|segment| {
                    format!(
                        "{}:{}:{}:{}",
                        segment.id, segment.start_ns, segment.end_ns, segment.text
                    )
                })
                .collect::<Vec<_>>()
                .join("~");
            format!(
                "{}:{}:{}",
                window.source_in_ns, window.source_out_ns, segments
            )
        })
        .collect::<Vec<_>>()
        .join("|")
}

/// Return the request window for the current transcript-first background AI
/// indexing phase, or `None` when this media item should be skipped.
pub fn media_background_ai_index_request(item: &MediaItem) -> Option<(u64, u64)> {
    if !item.has_backing_file()
        || item.is_missing
        || item.is_image
        || !item.has_audio
        || item.duration_ns == 0
        || !item.transcript_windows.is_empty()
    {
        return None;
    }
    Some((0, item.duration_ns))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaVisualIndexRequest {
    pub duration_ns: u64,
    pub is_image: bool,
}

pub fn media_background_visual_index_request(item: &MediaItem) -> Option<MediaVisualIndexRequest> {
    if !item.has_backing_file()
        || item.is_missing
        || item.is_audio_only
        || item.is_animated_svg
        || item.visual_embedding.is_some()
    {
        return None;
    }
    Some(MediaVisualIndexRequest {
        duration_ns: item.duration_ns,
        is_image: item.is_image,
    })
}

pub fn media_background_auto_tag_request(item: &MediaItem) -> Option<MediaVisualEmbedding> {
    if !item.has_backing_file() || item.is_missing || item.auto_tags_indexed {
        return None;
    }
    item.visual_embedding.clone()
}

pub fn media_search_match(item: &MediaItem, query: &str) -> Option<MediaSearchMatch> {
    let raw_query = query.trim();
    let query = SearchQuery::new(raw_query)?;
    let mut best = None;

    consider_text_search(
        &mut best,
        MediaSearchField::DisplayName,
        &media_display_name(item),
        &query,
        920,
        760,
        None,
        None,
        None,
    );
    consider_text_search(
        &mut best,
        MediaSearchField::Label,
        item.label.as_str(),
        &query,
        900,
        740,
        None,
        None,
        None,
    );
    if let Some(title_text) = item.title_text.as_deref() {
        consider_text_search(
            &mut best,
            MediaSearchField::TitleText,
            title_text,
            &query,
            910,
            750,
            None,
            None,
            None,
        );
    }
    if !item.source_path.is_empty() {
        consider_text_search(
            &mut best,
            MediaSearchField::SourcePath,
            item.source_path.as_str(),
            &query,
            760,
            620,
            None,
            None,
            None,
        );
    }
    if let Some(codec) = item.codec_summary.as_deref() {
        consider_text_search(
            &mut best,
            MediaSearchField::Codec,
            codec,
            &query,
            680,
            560,
            None,
            None,
            None,
        );
    }
    for range in &item.keyword_ranges {
        let label = range.label.trim();
        if label.is_empty() {
            continue;
        }
        consider_text_search(
            &mut best,
            MediaSearchField::Keyword,
            label,
            &query,
            860,
            700,
            Some(label.to_string()),
            None,
            None,
        );
    }
    if !item.auto_tags.is_empty() {
        let auto_tag_text = item
            .auto_tags
            .iter()
            .map(|tag| format!("{} {}", tag.category.search_text(), tag.label))
            .collect::<Vec<_>>()
            .join(" ");
        consider_text_search(
            &mut best,
            MediaSearchField::AutoTag,
            auto_tag_text.as_str(),
            &query,
            850,
            720,
            media_auto_tag_search_excerpt(item, &query),
            item.auto_tags.iter().find_map(|tag| tag.best_frame_time_ns),
            item.auto_tags.iter().find_map(|tag| tag.best_frame_time_ns),
        );
    }
    for window in &item.transcript_windows {
        let joined_text = window
            .segments
            .iter()
            .map(|segment| segment.text.trim())
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if !joined_text.is_empty() {
            consider_text_search(
                &mut best,
                MediaSearchField::Transcript,
                joined_text.as_str(),
                &query,
                780,
                650,
                Some(highlight_search_excerpt(joined_text.as_str(), &query)),
                Some(window.source_in_ns),
                Some(window.source_out_ns),
            );
        }
        for segment in &window.segments {
            let text = segment.text.trim();
            if text.is_empty() {
                continue;
            }
            consider_text_search(
                &mut best,
                MediaSearchField::Transcript,
                text,
                &query,
                820,
                690,
                Some(highlight_search_excerpt(text, &query)),
                Some(window.source_in_ns.saturating_add(segment.start_ns)),
                Some(window.source_in_ns.saturating_add(segment.end_ns)),
            );
        }
    }
    if let Some(embedding) = item.visual_embedding.as_ref() {
        if let Some(visual_match) =
            crate::media::clip_embedding_cache::visual_search_match(raw_query, embedding)
        {
            let candidate = MediaSearchMatch {
                field: MediaSearchField::Visual,
                score: visual_match.score,
                excerpt: visual_match
                    .best_frame_time_ns
                    .map(format_visual_search_excerpt)
                    .or_else(|| Some("Semantic visual match".to_string())),
                source_in_ns: visual_match.best_frame_time_ns,
                source_out_ns: visual_match.best_frame_time_ns,
            };
            let replace = best
                .as_ref()
                .map(|current| candidate.score > current.score)
                .unwrap_or(true);
            if replace {
                best = Some(candidate);
            }
        }
    }

    best
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
    media_search_match(item, filters.search_text.as_str()).is_some()
}

pub fn upsert_media_transcript(
    lib: &mut MediaLibrary,
    source_path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
    segments: Vec<SubtitleSegment>,
) -> bool {
    if source_path.trim().is_empty() || segments.is_empty() {
        return false;
    }
    if let Some(item) = lib
        .items
        .iter_mut()
        .find(|item| item.source_path == source_path)
    {
        return item.upsert_transcript_window(source_in_ns, source_out_ns, segments);
    }
    let mut item = MediaItem::new(source_path.to_string(), 0);
    let changed = item.upsert_transcript_window(source_in_ns, source_out_ns, segments);
    lib.items.push(item);
    changed
}

pub fn upsert_media_visual_embedding(
    lib: &mut MediaLibrary,
    source_path: &str,
    embedding: MediaVisualEmbedding,
) -> bool {
    if source_path.trim().is_empty() || embedding.frames.is_empty() {
        return false;
    }
    if let Some(item) = lib
        .items
        .iter_mut()
        .find(|item| item.source_path == source_path)
    {
        return item.upsert_visual_embedding(embedding);
    }
    false
}

pub fn upsert_media_auto_tags(
    lib: &mut MediaLibrary,
    source_path: &str,
    auto_tags: Vec<MediaAutoTag>,
) -> bool {
    if source_path.trim().is_empty() {
        return false;
    }
    if let Some(item) = lib
        .items
        .iter_mut()
        .find(|item| item.source_path == source_path)
    {
        return item.upsert_auto_tags(auto_tags);
    }
    false
}

#[derive(Debug, Clone)]
struct SearchQuery {
    raw_lower: String,
    folded: String,
    tokens: Vec<String>,
}

impl SearchQuery {
    fn new(query: &str) -> Option<Self> {
        let raw_lower = query.trim().to_ascii_lowercase();
        if raw_lower.is_empty() {
            return None;
        }
        let folded = fold_search_text(query);
        let mut tokens: Vec<String> = if folded.is_empty() {
            raw_lower
                .split_whitespace()
                .map(ToOwned::to_owned)
                .collect()
        } else {
            folded.split_whitespace().map(ToOwned::to_owned).collect()
        };
        tokens.retain(|token| !token.is_empty());
        if tokens.is_empty() {
            tokens.push(raw_lower.clone());
        }
        Some(Self {
            raw_lower,
            folded,
            tokens,
        })
    }
}

fn normalize_transcript_window_end(
    source_in_ns: u64,
    source_out_ns: u64,
    segments: &[SubtitleSegment],
) -> u64 {
    if source_out_ns != u64::MAX {
        return source_out_ns.max(source_in_ns);
    }
    segments
        .iter()
        .map(|segment| source_in_ns.saturating_add(segment.end_ns))
        .max()
        .unwrap_or(source_in_ns)
        .max(source_in_ns)
}

fn fold_search_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
            pending_space = false;
        } else {
            pending_space = true;
        }
    }
    out
}

fn score_search_text(
    text: &str,
    query: &SearchQuery,
    exact_base: i32,
    token_base: i32,
) -> Option<i32> {
    let lower = text.to_ascii_lowercase();
    if lower.contains(&query.raw_lower) {
        return Some(exact_base - lower.split_whitespace().count().min(48) as i32);
    }
    let folded = fold_search_text(text);
    if !query.folded.is_empty() && folded.contains(&query.folded) {
        return Some(exact_base - folded.split_whitespace().count().min(48) as i32);
    }
    let token_hits = query
        .tokens
        .iter()
        .filter(|token| folded.contains(token.as_str()))
        .count();
    if token_hits == 0 {
        return None;
    }
    let all_tokens = token_hits == query.tokens.len();
    Some(
        token_base + token_hits as i32 * 18 + if all_tokens { 48 } else { 0 }
            - folded.split_whitespace().count().min(48) as i32,
    )
}

fn highlight_search_excerpt(text: &str, query: &SearchQuery) -> String {
    let lower = text.to_ascii_lowercase();
    if let Some(start) = lower.find(&query.raw_lower) {
        let end = start + query.raw_lower.len();
        return format!("{}[{}]{}", &text[..start], &text[start..end], &text[end..]);
    }
    for token in &query.tokens {
        if let Some(start) = lower.find(token) {
            let end = start + token.len();
            return format!("{}[{}]{}", &text[..start], &text[start..end], &text[end..]);
        }
    }
    text.to_string()
}

fn format_visual_search_excerpt(time_ns: u64) -> String {
    let total_millis = time_ns / 1_000_000;
    let hours = total_millis / 3_600_000;
    let minutes = (total_millis / 60_000) % 60;
    let seconds = (total_millis / 1_000) % 60;
    let tenths = (total_millis % 1_000) / 100;
    if hours > 0 {
        format!("Closest visual frame around {hours:02}:{minutes:02}:{seconds:02}.{tenths}")
    } else {
        format!("Closest visual frame around {minutes:02}:{seconds:02}.{tenths}")
    }
}

fn consider_text_search(
    best: &mut Option<MediaSearchMatch>,
    field: MediaSearchField,
    text: &str,
    query: &SearchQuery,
    exact_base: i32,
    token_base: i32,
    excerpt: Option<String>,
    source_in_ns: Option<u64>,
    source_out_ns: Option<u64>,
) {
    let Some(score) = score_search_text(text, query, exact_base, token_base) else {
        return;
    };
    let candidate = MediaSearchMatch {
        field,
        score,
        excerpt,
        source_in_ns,
        source_out_ns,
    };
    let replace = best
        .as_ref()
        .map(|current| candidate.score > current.score)
        .unwrap_or(true);
    if replace {
        *best = Some(candidate);
    }
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

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
struct SavedMediaAnnotations {
    #[serde(default)]
    rating: MediaRating,
    #[serde(default)]
    keyword_ranges: Vec<MediaKeywordRange>,
    #[serde(default)]
    auto_tags: Vec<MediaAutoTag>,
    #[serde(default)]
    auto_tags_indexed: bool,
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
    pub hdr_colorimetry: Option<String>,
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
            hdr_colorimetry: item.hdr_colorimetry.clone(),
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
        item.hdr_colorimetry = self.hdr_colorimetry.clone();
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
        item.hdr_colorimetry = self.hdr_colorimetry;
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
            ((item.rating != MediaRating::None)
                || !item.keyword_ranges.is_empty()
                || !item.auto_tags.is_empty()
                || item.auto_tags_indexed)
                .then(|| {
                    (
                        item.library_key(),
                        SavedMediaAnnotations {
                            rating: item.rating,
                            keyword_ranges: item.keyword_ranges.clone(),
                            auto_tags: item.auto_tags.clone(),
                            auto_tags_indexed: item.auto_tags_indexed,
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
    let transcript_cache: std::collections::HashMap<String, Vec<MediaTranscriptWindow>> = lib
        .items
        .iter()
        .filter_map(|item| {
            (!item.transcript_windows.is_empty())
                .then(|| (item.library_key(), item.transcript_windows.clone()))
        })
        .collect();
    if transcript_cache.is_empty() {
        project.parsed_transcript_cache_json = None;
    } else {
        project.parsed_transcript_cache_json = serde_json::to_string(&transcript_cache).ok();
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
                    item.auto_tags = annotations.auto_tags.clone();
                    item.auto_tags_indexed = annotations.auto_tags_indexed;
                }
            }
        }
    }
    if let Some(ref transcript_cache_json) = project.parsed_transcript_cache_json {
        if let Ok(map) = serde_json::from_str::<
            std::collections::HashMap<String, Vec<MediaTranscriptWindow>>,
        >(transcript_cache_json)
        {
            for item in lib.items.iter_mut() {
                let windows = map.get(&item.library_key()).or_else(|| {
                    map.iter()
                        .find_map(|(key, value)| item.matches_library_key(key).then_some(value))
                });
                if let Some(windows) = windows {
                    item.transcript_windows = windows.clone();
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
    project.parsed_transcript_cache_json = None;
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
    /// Preferred program-audio routing when this source is placed on the timeline.
    pub audio_channel_mode: AudioChannelMode,
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
            audio_channel_mode: AudioChannelMode::Stereo,
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
            hdr_colorimetry: None,
            file_size_bytes: None,
            clip_kind: None,
            title_text: None,
            is_missing: true, // test paths don't exist
            bin_id,
            rating: MediaRating::None,
            keyword_ranges: Vec::new(),
            auto_tags: Vec::new(),
            auto_tags_indexed: false,
            transcript_windows: Vec::new(),
            visual_embedding: None,
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
    fn test_media_search_match_finds_auto_tags() {
        let mut item = MediaItem::new("/tmp/clip.mov", 3_000_000_000);
        item.is_missing = false;
        assert!(item.upsert_auto_tags(vec![
            MediaAutoTag::new(
                MediaAutoTagCategory::ShotType,
                "wide",
                0.82,
                Some(1_000_000_000),
            )
            .expect("wide tag"),
            MediaAutoTag::new(
                MediaAutoTagCategory::Setting,
                "outdoor",
                0.76,
                Some(2_000_000_000),
            )
            .expect("outdoor tag"),
        ]));

        let search_match =
            media_search_match(&item, "outdoor wide").expect("expected auto-tag match");
        assert_eq!(search_match.field, MediaSearchField::AutoTag);
        assert!(search_match
            .excerpt
            .as_deref()
            .is_some_and(|excerpt| excerpt.contains("wide") || excerpt.contains("outdoor")));
        assert!(media_matches_filters(
            &item,
            &MediaFilterCriteria {
                search_text: "outdoor wide".to_string(),
                ..Default::default()
            }
        ));
    }

    #[test]
    fn test_upsert_media_transcript_creates_library_item() {
        let mut lib = MediaLibrary::new();
        let segments = vec![SubtitleSegment {
            id: "seg-1".to_string(),
            start_ns: 0,
            end_ns: 1_000_000_000,
            text: "Hello transcript".to_string(),
            words: Vec::new(),
        }];
        assert!(upsert_media_transcript(
            &mut lib,
            "/tmp/transcript.mov",
            0,
            u64::MAX,
            segments.clone()
        ));
        assert_eq!(lib.items.len(), 1);
        assert_eq!(lib.items[0].transcript_windows.len(), 1);
        assert_eq!(
            lib.items[0].transcript_windows[0].source_out_ns,
            1_000_000_000
        );
        assert!(!upsert_media_transcript(
            &mut lib,
            "/tmp/transcript.mov",
            0,
            u64::MAX,
            segments
        ));
    }

    #[test]
    fn test_media_search_match_finds_spoken_content() {
        let mut item = MediaItem::new("/tmp/dialog.mov", 10_000_000_000);
        item.is_missing = false;
        item.has_audio = true;
        item.upsert_transcript_window(
            0,
            10_000_000_000,
            vec![SubtitleSegment {
                id: "seg-1".to_string(),
                start_ns: 1_000_000_000,
                end_ns: 2_500_000_000,
                text: "Find the sticker on the table".to_string(),
                words: Vec::new(),
            }],
        );

        let search_match = media_search_match(&item, "sticker").expect("expected transcript match");
        assert_eq!(search_match.field, MediaSearchField::Transcript);
        assert!(search_match
            .excerpt
            .as_deref()
            .is_some_and(|excerpt| excerpt.contains("[sticker]")));
        assert!(media_matches_filters(
            &item,
            &MediaFilterCriteria {
                search_text: "sticker".to_string(),
                ..Default::default()
            }
        ));
    }

    #[test]
    fn test_media_background_ai_index_request_requires_audio_and_transcript_gap() {
        let mut item = MediaItem::new("/tmp/dialog.mov", 10_000_000_000);
        item.is_missing = false;
        assert_eq!(media_background_ai_index_request(&item), None);

        item.has_audio = true;
        assert_eq!(
            media_background_ai_index_request(&item),
            Some((0, 10_000_000_000))
        );

        item.upsert_transcript_window(
            0,
            10_000_000_000,
            vec![SubtitleSegment {
                id: "seg-1".to_string(),
                start_ns: 0,
                end_ns: 1_000_000_000,
                text: "Already indexed".to_string(),
                words: Vec::new(),
            }],
        );
        assert_eq!(media_background_ai_index_request(&item), None);
    }

    #[test]
    fn test_media_background_visual_index_request_requires_visual_gap() {
        let mut item = MediaItem::new("/tmp/visual.mov", 12_000_000_000);
        item.is_missing = false;
        assert_eq!(
            media_background_visual_index_request(&item),
            Some(MediaVisualIndexRequest {
                duration_ns: 12_000_000_000,
                is_image: false,
            })
        );

        item.is_audio_only = true;
        assert_eq!(media_background_visual_index_request(&item), None);

        item.is_audio_only = false;
        item.is_animated_svg = true;
        assert_eq!(media_background_visual_index_request(&item), None);

        item.is_animated_svg = false;
        item.visual_embedding = Some(MediaVisualEmbedding {
            model_id: "test".to_string(),
            frames: vec![MediaVisualEmbeddingFrame {
                time_ns: 0,
                embedding: vec![1.0, 0.0],
            }],
        });
        assert_eq!(media_background_visual_index_request(&item), None);
    }

    #[test]
    fn test_media_background_auto_tag_request_requires_visual_gap() {
        let mut item = MediaItem::new("/tmp/visual.mov", 12_000_000_000);
        item.is_missing = false;
        assert_eq!(media_background_auto_tag_request(&item), None);

        let embedding = MediaVisualEmbedding {
            model_id: "test".to_string(),
            frames: vec![MediaVisualEmbeddingFrame {
                time_ns: 0,
                embedding: vec![1.0, 0.0],
            }],
        };
        item.visual_embedding = Some(embedding.clone());
        assert_eq!(
            media_background_auto_tag_request(&item),
            Some(embedding.clone())
        );

        item.upsert_auto_tags(vec![MediaAutoTag::new(
            MediaAutoTagCategory::Subject,
            "person",
            0.8,
            Some(0),
        )
        .expect("person tag")]);
        assert_eq!(media_background_auto_tag_request(&item), None);
    }

    #[test]
    fn test_upsert_media_visual_embedding_updates_existing_item() {
        let mut lib = MediaLibrary::new();
        let mut item = MediaItem::new("/tmp/visual.mov", 5_000_000_000);
        item.is_missing = false;
        lib.items.push(item);

        let embedding = MediaVisualEmbedding {
            model_id: "test-model".to_string(),
            frames: vec![MediaVisualEmbeddingFrame {
                time_ns: 2_000_000_000,
                embedding: vec![0.5, 0.5],
            }],
        };
        assert!(upsert_media_visual_embedding(
            &mut lib,
            "/tmp/visual.mov",
            embedding.clone()
        ));
        assert_eq!(lib.items[0].visual_embedding.as_ref(), Some(&embedding));
        assert!(!upsert_media_visual_embedding(
            &mut lib,
            "/tmp/visual.mov",
            embedding
        ));
    }

    #[test]
    fn test_upsert_media_auto_tags_updates_existing_item() {
        let mut lib = MediaLibrary::new();
        let mut item = MediaItem::new("/tmp/visual.mov", 5_000_000_000);
        item.is_missing = false;
        lib.items.push(item);

        let auto_tags = vec![
            MediaAutoTag::new(MediaAutoTagCategory::Setting, "indoor", 0.65, Some(0))
                .expect("indoor tag"),
            MediaAutoTag::new(MediaAutoTagCategory::Subject, "person", 0.71, Some(0))
                .expect("person tag"),
        ];
        assert!(upsert_media_auto_tags(
            &mut lib,
            "/tmp/visual.mov",
            auto_tags.clone()
        ));
        assert_eq!(lib.items[0].auto_tags, auto_tags);
        assert!(!upsert_media_auto_tags(
            &mut lib,
            "/tmp/visual.mov",
            auto_tags
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
        item.auto_tags.push(
            MediaAutoTag::new(
                MediaAutoTagCategory::Setting,
                "outdoor",
                0.77,
                Some(500_000_000),
            )
            .expect("outdoor tag"),
        );
        item.upsert_transcript_window(
            0,
            2_000_000_000,
            vec![SubtitleSegment {
                id: "seg-1".to_string(),
                start_ns: 100_000_000,
                end_ns: 900_000_000,
                text: "Interview opening line".to_string(),
                words: Vec::new(),
            }],
        );
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
            .is_some_and(|json| json.contains("B-roll") && json.contains("outdoor")));
        assert!(project
            .parsed_transcript_cache_json
            .as_ref()
            .is_some_and(|json| json.contains("Interview opening line")));

        let mut restored = MediaLibrary::new();
        apply_bins_from_project(&mut restored, &mut project);

        assert_eq!(restored.items.len(), 1);
        assert_eq!(restored.items[0].source_path, "/tmp/rated.mov");
        assert_eq!(restored.items[0].rating, MediaRating::Favorite);
        assert_eq!(restored.items[0].keyword_ranges.len(), 1);
        assert_eq!(restored.items[0].keyword_ranges[0].label, "B-roll");
        assert_eq!(restored.items[0].auto_tags.len(), 1);
        assert_eq!(restored.items[0].auto_tags[0].label, "outdoor");
        assert_eq!(restored.items[0].transcript_windows.len(), 1);
        assert_eq!(
            restored.items[0].transcript_windows[0].segments[0].text,
            "Interview opening line"
        );
        assert!(project.parsed_transcript_cache_json.is_none());
    }

    #[test]
    fn library_item_roundtrip_through_fcpxml() {
        let mut project = crate::model::project::Project::new("Test");
        let mut lib = MediaLibrary::new();

        // Add an off-timeline item to the library.
        let mut item = MediaItem::new("/tmp/video.mp4", 5_000_000_000);
        item.label = "My Clip".to_string();
        item.has_audio = true;
        lib.items.push(item);

        // Sync library → project transient fields.
        sync_bins_to_project(&lib, &mut project);
        assert!(
            project.parsed_library_items_json.is_some(),
            "parsed_library_items_json should be populated"
        );

        // Write FCPXML.
        let xml = crate::fcpxml::writer::write_fcpxml(&project).expect("write_fcpxml");
        assert!(
            xml.contains("us:library-items"),
            "FCPXML should contain us:library-items vendor attr"
        );

        // Parse FCPXML back.
        let mut loaded = crate::fcpxml::parser::parse_fcpxml(&xml).expect("parse_fcpxml");
        assert!(
            loaded.parsed_library_items_json.is_some(),
            "loaded project should carry parsed_library_items_json"
        );

        // Apply to a fresh library (simulates clear-on-reload).
        let mut new_lib = MediaLibrary::new();
        apply_bins_from_project(&mut new_lib, &mut loaded);
        assert_eq!(new_lib.items.len(), 1);
        assert_eq!(new_lib.items[0].source_path, "/tmp/video.mp4");
        assert_eq!(new_lib.items[0].duration_ns, 5_000_000_000);
        assert_eq!(new_lib.items[0].label, "My Clip");
        assert!(new_lib.items[0].has_audio);
    }
}
