// SPDX-License-Identifier: GPL-3.0-or-later
//! Background contextual auto-tagging built on top of CLIP-style embeddings.

use crate::model::media_library::{MediaAutoTag, MediaAutoTagCategory, MediaVisualEmbedding};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;

const CACHE_VERSION: &str = "media-auto-tags-v1";

#[derive(Clone, Copy, Debug)]
struct TagPrototype {
    category: MediaAutoTagCategory,
    label: &'static str,
    prompt: &'static str,
    min_similarity: f32,
}

const SHOT_TYPE_TAGS: &[TagPrototype] = &[
    TagPrototype {
        category: MediaAutoTagCategory::ShotType,
        label: "wide",
        prompt: "a wide shot of a scene",
        min_similarity: 0.20,
    },
    TagPrototype {
        category: MediaAutoTagCategory::ShotType,
        label: "medium",
        prompt: "a medium shot of a person or subject",
        min_similarity: 0.20,
    },
    TagPrototype {
        category: MediaAutoTagCategory::ShotType,
        label: "close-up",
        prompt: "a close-up shot of a person or object",
        min_similarity: 0.20,
    },
];

const SETTING_TAGS: &[TagPrototype] = &[
    TagPrototype {
        category: MediaAutoTagCategory::Setting,
        label: "indoor",
        prompt: "an indoor scene inside a building",
        min_similarity: 0.20,
    },
    TagPrototype {
        category: MediaAutoTagCategory::Setting,
        label: "outdoor",
        prompt: "an outdoor scene outside",
        min_similarity: 0.20,
    },
];

const TIME_OF_DAY_TAGS: &[TagPrototype] = &[
    TagPrototype {
        category: MediaAutoTagCategory::TimeOfDay,
        label: "day",
        prompt: "a scene in daylight",
        min_similarity: 0.21,
    },
    TagPrototype {
        category: MediaAutoTagCategory::TimeOfDay,
        label: "night",
        prompt: "a scene at night in the dark",
        min_similarity: 0.21,
    },
];

const SUBJECT_TAGS: &[TagPrototype] = &[
    TagPrototype {
        category: MediaAutoTagCategory::Subject,
        label: "person",
        prompt: "a person or portrait",
        min_similarity: 0.22,
    },
    TagPrototype {
        category: MediaAutoTagCategory::Subject,
        label: "crowd",
        prompt: "a crowd of many people",
        min_similarity: 0.23,
    },
    TagPrototype {
        category: MediaAutoTagCategory::Subject,
        label: "car",
        prompt: "a car or vehicle",
        min_similarity: 0.22,
    },
    TagPrototype {
        category: MediaAutoTagCategory::Subject,
        label: "building",
        prompt: "a building or architecture",
        min_similarity: 0.22,
    },
    TagPrototype {
        category: MediaAutoTagCategory::Subject,
        label: "screen",
        prompt: "a computer screen or phone screen",
        min_similarity: 0.22,
    },
    TagPrototype {
        category: MediaAutoTagCategory::Subject,
        label: "text",
        prompt: "text or words on screen",
        min_similarity: 0.22,
    },
    TagPrototype {
        category: MediaAutoTagCategory::Subject,
        label: "nature",
        prompt: "trees landscape sky or nature",
        min_similarity: 0.22,
    },
    TagPrototype {
        category: MediaAutoTagCategory::Subject,
        label: "animal",
        prompt: "an animal",
        min_similarity: 0.23,
    },
];

#[derive(Clone, Debug)]
enum WorkerUpdate {
    Done(WorkerResult),
}

#[derive(Clone, Debug)]
struct WorkerResult {
    cache_key: String,
    auto_tags: Option<Vec<MediaAutoTag>>,
    success: bool,
}

#[derive(Clone, Debug)]
struct AutoTagJob {
    cache_key: String,
    source_path: String,
    embedding: MediaVisualEmbedding,
    output_path: String,
}

pub struct AutoTagProgress {
    pub total: usize,
    pub completed: usize,
    pub in_flight: bool,
}

pub struct AutoTagPollResult {
    pub source_path: String,
    pub auto_tags: Vec<MediaAutoTag>,
}

pub enum AutoTagRequest {
    Skipped,
    Queued,
    Ready(AutoTagPollResult),
}

pub struct AutoTagCache {
    tags_by_source: HashMap<String, Vec<MediaAutoTag>>,
    source_to_key: HashMap<String, String>,
    pending: HashSet<String>,
    failed: HashSet<String>,
    key_to_source: HashMap<String, String>,
    total_requested: usize,
    total_completed: usize,
    result_rx: mpsc::Receiver<WorkerUpdate>,
    work_tx: Option<mpsc::Sender<AutoTagJob>>,
    cache_root: PathBuf,
}

impl AutoTagCache {
    pub fn new() -> Self {
        let (result_tx, result_rx) = mpsc::sync_channel::<WorkerUpdate>(32);
        let (work_tx, work_rx) = mpsc::channel::<AutoTagJob>();

        std::thread::spawn(move || {
            while let Ok(job) = work_rx.recv() {
                let auto_tags = run_auto_tag_job(&job);
                let success = auto_tags.is_some();
                let _ = result_tx.send(WorkerUpdate::Done(WorkerResult {
                    cache_key: job.cache_key,
                    auto_tags,
                    success,
                }));
            }
        });

        let cache_root = crate::media::cache_support::cache_root_dir("media_auto_tags");
        let _ = std::fs::create_dir_all(&cache_root);

        Self {
            tags_by_source: HashMap::new(),
            source_to_key: HashMap::new(),
            pending: HashSet::new(),
            failed: HashSet::new(),
            key_to_source: HashMap::new(),
            total_requested: 0,
            total_completed: 0,
            result_rx,
            work_tx: Some(work_tx),
            cache_root,
        }
    }

    pub fn is_available(&self) -> bool {
        crate::media::clip_embedding_cache::clip_search_models_available()
    }

    pub fn request(
        &mut self,
        source_path: &str,
        embedding: MediaVisualEmbedding,
    ) -> AutoTagRequest {
        if source_path.trim().is_empty()
            || embedding.frames.is_empty()
            || !crate::media::clip_embedding_cache::clip_search_models_available()
        {
            return AutoTagRequest::Skipped;
        }
        let key = cache_key(source_path, &embedding);
        if self.pending.contains(&key) || self.failed.contains(&key) {
            return AutoTagRequest::Skipped;
        }
        if self.source_to_key.get(source_path) == Some(&key)
            && self.tags_by_source.contains_key(source_path)
        {
            return AutoTagRequest::Skipped;
        }

        let output_path = self.output_path_for_key(&key);
        if tag_file_is_ready(&output_path) {
            if let Some(auto_tags) = load_tag_file(&output_path) {
                self.source_to_key
                    .insert(source_path.to_string(), key.clone());
                self.tags_by_source
                    .insert(source_path.to_string(), auto_tags.clone());
                return AutoTagRequest::Ready(AutoTagPollResult {
                    source_path: source_path.to_string(),
                    auto_tags,
                });
            }
            let _ = std::fs::remove_file(&output_path);
        }

        self.total_requested += 1;
        self.pending.insert(key.clone());
        self.key_to_source
            .insert(key.clone(), source_path.to_string());
        self.source_to_key
            .insert(source_path.to_string(), key.clone());
        if let Some(ref tx) = self.work_tx {
            if tx
                .send(AutoTagJob {
                    cache_key: key,
                    source_path: source_path.to_string(),
                    embedding,
                    output_path,
                })
                .is_ok()
            {
                return AutoTagRequest::Queued;
            }
        }
        AutoTagRequest::Skipped
    }

    pub fn poll(&mut self) -> Vec<AutoTagPollResult> {
        let mut resolved = Vec::new();
        while let Ok(update) = self.result_rx.try_recv() {
            match update {
                WorkerUpdate::Done(result) => {
                    self.pending.remove(&result.cache_key);
                    self.total_completed += 1;
                    if result.success {
                        if let Some(source_path) = self.key_to_source.remove(&result.cache_key) {
                            if let Some(auto_tags) = result.auto_tags {
                                if self.source_to_key.get(&source_path) == Some(&result.cache_key) {
                                    self.tags_by_source
                                        .insert(source_path.clone(), auto_tags.clone());
                                    resolved.push(AutoTagPollResult {
                                        source_path,
                                        auto_tags,
                                    });
                                }
                            }
                        }
                    } else {
                        log::warn!("AutoTagCache: failed key={}", result.cache_key);
                        self.failed.insert(result.cache_key);
                    }
                }
            }
        }
        resolved
    }

    pub fn progress(&self) -> AutoTagProgress {
        AutoTagProgress {
            total: self.total_requested,
            completed: self.total_completed,
            in_flight: !self.pending.is_empty(),
        }
    }

    fn output_path_for_key(&self, key: &str) -> String {
        self.cache_root
            .join(format!("{key}.json"))
            .to_string_lossy()
            .to_string()
    }
}

impl Drop for AutoTagCache {
    fn drop(&mut self) {
        self.work_tx.take();
    }
}

pub fn cache_root_dir() -> PathBuf {
    crate::media::cache_support::cache_root_dir("media_auto_tags")
}

fn cache_key(source_path: &str, embedding: &MediaVisualEmbedding) -> String {
    crate::media::cache_key::hashed_key("media_auto_tags", |key| {
        key.add(CACHE_VERSION)
            .add_source_fingerprint(source_path)
            .add(embedding.model_id.as_str())
            .add(embedding.frames.len() as u64);
        for frame in &embedding.frames {
            key.add(frame.time_ns).add(frame.embedding.len() as u64);
        }
    })
}

fn tag_file_is_ready(path: &str) -> bool {
    crate::media::cache_support::file_has_content(path)
}

fn load_tag_file(path: &str) -> Option<Vec<MediaAutoTag>> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Vec<MediaAutoTag>>(&text).ok()
}

fn save_tag_file(path: &str, auto_tags: &[MediaAutoTag]) -> bool {
    let temp_path = format!("{path}.partial");
    let data = match serde_json::to_vec(auto_tags) {
        Ok(data) => data,
        Err(err) => {
            log::error!("AutoTagCache: failed to serialize tags: {err}");
            return false;
        }
    };
    if std::fs::write(&temp_path, data).is_err() {
        return false;
    }
    std::fs::rename(&temp_path, path).is_ok()
}

fn run_auto_tag_job(job: &AutoTagJob) -> Option<Vec<MediaAutoTag>> {
    let auto_tags = classify_embedding(&job.embedding)?;
    if !save_tag_file(&job.output_path, &auto_tags) {
        log::warn!(
            "AutoTagCache: failed to persist auto-tag cache for {}",
            job.source_path
        );
    }
    Some(auto_tags)
}

fn classify_embedding(embedding: &MediaVisualEmbedding) -> Option<Vec<MediaAutoTag>> {
    let mut scored_any = false;
    let mut auto_tags = Vec::new();
    if let Some(tag) = best_category_tag(embedding, SHOT_TYPE_TAGS, &mut scored_any) {
        auto_tags.push(tag);
    }
    if let Some(tag) = best_category_tag(embedding, SETTING_TAGS, &mut scored_any) {
        auto_tags.push(tag);
    }
    if let Some(tag) = best_category_tag(embedding, TIME_OF_DAY_TAGS, &mut scored_any) {
        auto_tags.push(tag);
    }
    auto_tags.extend(best_subject_tags(embedding, SUBJECT_TAGS, &mut scored_any));
    scored_any.then_some(auto_tags)
}

fn best_category_tag(
    embedding: &MediaVisualEmbedding,
    prototypes: &[TagPrototype],
    scored_any: &mut bool,
) -> Option<MediaAutoTag> {
    let mut best: Option<(f32, MediaAutoTag)> = None;
    for prototype in prototypes {
        let Some(candidate) = classify_prototype(embedding, *prototype) else {
            continue;
        };
        *scored_any = true;
        let replace = best
            .as_ref()
            .map(|(current, _)| candidate.0 > *current)
            .unwrap_or(true);
        if replace {
            best = Some(candidate);
        }
    }
    best.map(|(_, tag)| tag)
}

fn best_subject_tags(
    embedding: &MediaVisualEmbedding,
    prototypes: &[TagPrototype],
    scored_any: &mut bool,
) -> Vec<MediaAutoTag> {
    let mut matches = Vec::new();
    for prototype in prototypes {
        let Some(candidate) = classify_prototype(embedding, *prototype) else {
            continue;
        };
        *scored_any = true;
        matches.push(candidate);
    }
    matches.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    matches.into_iter().take(2).map(|(_, tag)| tag).collect()
}

fn classify_prototype(
    embedding: &MediaVisualEmbedding,
    prototype: TagPrototype,
) -> Option<(f32, MediaAutoTag)> {
    let query_embedding =
        crate::media::clip_embedding_cache::text_query_embedding(prototype.prompt)?;
    let visual_match =
        crate::media::clip_embedding_cache::best_visual_frame_match(&query_embedding, embedding)?;
    if visual_match.similarity < prototype.min_similarity {
        return None;
    }
    let confidence = similarity_to_confidence(visual_match.similarity, prototype.min_similarity);
    let tag = MediaAutoTag::new(
        prototype.category,
        prototype.label,
        confidence,
        visual_match.best_frame_time_ns,
    )?;
    Some((visual_match.similarity, tag))
}

fn similarity_to_confidence(similarity: f32, min_similarity: f32) -> f32 {
    ((similarity - min_similarity) / 0.18).clamp(0.0, 1.0)
}
