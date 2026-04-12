// SPDX-License-Identifier: GPL-3.0-or-later
//! Transcript-to-script alignment engine.
//!
//! Two-phase approach:
//! 1. Coarse scene matching via TF-IDF weighted Jaccard similarity.
//! 2. Fine-grained Smith-Waterman local alignment for sub-clip boundaries.

use serde::{Deserialize, Serialize};
use crate::model::clip::SubtitleSegment;
use super::script::Script;

// ── Data types ──────────────────────────────────────────────────────────

/// A single clip-to-scene mapping produced by the alignment engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneMapping {
    /// Source path of the media clip.
    pub clip_source_path: String,
    /// Scene ID from the parsed script.
    pub scene_id: String,
    /// Alignment confidence (0.0 .. 1.0).
    pub confidence: f64,
    /// Sub-clip in-point within the source file (nanoseconds).
    pub source_in_ns: u64,
    /// Sub-clip out-point within the source file (nanoseconds).
    pub source_out_ns: u64,
    /// The portion of the transcript that matched.
    pub transcript_excerpt: String,
}

/// Full alignment result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlignmentResult {
    /// Successful clip-to-scene mappings, sorted by scene order.
    pub mappings: Vec<SceneMapping>,
    /// Source paths that had no confident match.
    pub unmatched_clips: Vec<String>,
}

/// Progress reported from the alignment background thread.
#[derive(Debug, Clone, Copy)]
pub enum AlignmentPhase {
    Transcribing,
    Aligning,
    Done,
}

#[derive(Debug, Clone, Copy)]
pub struct AlignmentProgress {
    pub phase: AlignmentPhase,
    pub completed: usize,
    pub total: usize,
}

// ── Public API ──────────────────────────────────────────────────────────

/// Align clip transcripts against a parsed screenplay.
///
/// `transcripts` is a list of (source_path, subtitle_segments) pairs from STT.
/// `confidence_threshold` controls the cutoff below which clips are "unmatched".
pub fn align_transcripts_to_script(
    script: &Script,
    transcripts: &[(String, Vec<SubtitleSegment>)],
    confidence_threshold: f64,
) -> AlignmentResult {
    if script.scenes.is_empty() || transcripts.is_empty() {
        return AlignmentResult {
            mappings: Vec::new(),
            unmatched_clips: transcripts.iter().map(|(p, _)| p.clone()).collect(),
        };
    }

    // Pre-tokenize all scenes.
    let scene_token_sets: Vec<Vec<String>> = script
        .scenes
        .iter()
        .map(|s| tokenize(&s.full_text))
        .collect();

    // Compute IDF across all scenes.
    let idf = compute_idf(&scene_token_sets);

    // Extract character names from the script for boosting.
    let character_names: Vec<String> = script
        .scenes
        .iter()
        .flat_map(|s| {
            s.elements
                .iter()
                .filter_map(|e| e.character.as_ref().map(|c| c.to_lowercase()))
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    let mut mappings = Vec::new();
    let mut unmatched_clips = Vec::new();

    for (source_path, segments) in transcripts {
        // Flatten transcript to words.
        let transcript_text = segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        let transcript_tokens = tokenize(&transcript_text);

        if transcript_tokens.is_empty() {
            unmatched_clips.push(source_path.clone());
            continue;
        }

        // Phase 1: Coarse matching.
        let coarse_scores =
            coarse_scene_match(&scene_token_sets, &transcript_tokens, &idf, &character_names);

        // Phase 2: Fine alignment against ALL scenes above a coarse threshold.
        // Collect candidate regions so a single clip can match multiple scenes.
        let coarse_cutoff = confidence_threshold * 0.3; // liberal coarse gate
        let mut candidates: Vec<CandidateRegion> = Vec::new();

        for &(scene_idx, coarse_score) in &coarse_scores {
            if coarse_score < coarse_cutoff {
                continue;
            }
            let scene = &script.scenes[scene_idx];
            let scene_words = tokenize(&scene.full_text);

            let (sw_score, t_start, t_end) =
                smith_waterman_align(&transcript_tokens, &scene_words);

            if sw_score == 0 {
                continue;
            }

            // Normalize SW score to 0..1 range.
            let max_possible = scene_words.len().min(transcript_tokens.len()) as f64 * 2.0;
            let normalized = if max_possible > 0.0 {
                (sw_score as f64 / max_possible).min(1.0)
            } else {
                0.0
            };

            // Blend coarse + fine scores.
            let confidence = (coarse_score * 0.4 + normalized * 0.6).min(1.0);

            if confidence > confidence_threshold {
                let end_clamped = t_end.min(transcript_tokens.len().saturating_sub(1));
                candidates.push(CandidateRegion {
                    scene_idx,
                    confidence,
                    t_start,
                    t_end: end_clamped,
                });
            }
        }

        // Resolve overlapping regions: greedy interval scheduling by confidence.
        // Sort by confidence descending so we keep the best matches.
        candidates.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let accepted = resolve_overlapping_regions(&candidates);

        if accepted.is_empty() {
            unmatched_clips.push(source_path.clone());
        } else {
            for region in &accepted {
                let scene = &script.scenes[region.scene_idx];
                let (in_ns, out_ns) = word_indices_to_timestamps(
                    segments,
                    &transcript_tokens,
                    region.t_start,
                    region.t_end,
                );
                let excerpt_words = &transcript_tokens[region.t_start..=region.t_end];
                let excerpt = excerpt_words.join(" ");

                mappings.push(SceneMapping {
                    clip_source_path: source_path.clone(),
                    scene_id: scene.id.clone(),
                    confidence: region.confidence,
                    source_in_ns: in_ns,
                    source_out_ns: out_ns,
                    transcript_excerpt: excerpt,
                });
            }
        }
    }

    // Sort mappings by script scene order.
    let scene_order: std::collections::HashMap<&str, usize> = script
        .scenes
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();
    mappings.sort_by_key(|m| scene_order.get(m.scene_id.as_str()).copied().unwrap_or(usize::MAX));

    AlignmentResult {
        mappings,
        unmatched_clips,
    }
}

// ── Candidate region types and overlap resolution ───────────────────────

/// A candidate alignment of a transcript region to a scene.
#[derive(Debug, Clone)]
struct CandidateRegion {
    scene_idx: usize,
    confidence: f64,
    /// Start word index in the transcript token array (inclusive).
    t_start: usize,
    /// End word index in the transcript token array (inclusive).
    t_end: usize,
}

/// Greedy interval scheduling: accept non-overlapping regions by confidence.
///
/// Input must be sorted by confidence descending. Returns accepted regions
/// sorted by `t_start` (i.e. in transcript order).
fn resolve_overlapping_regions(candidates: &[CandidateRegion]) -> Vec<CandidateRegion> {
    let mut accepted: Vec<CandidateRegion> = Vec::new();

    for candidate in candidates {
        // Check if this candidate overlaps any already-accepted region.
        let overlaps = accepted.iter().any(|a| {
            candidate.t_start <= a.t_end && candidate.t_end >= a.t_start
        });
        if !overlaps {
            accepted.push(candidate.clone());
        }
    }

    // Sort by transcript position so mappings are in source-time order.
    accepted.sort_by_key(|r| r.t_start);
    accepted
}

// ── Tokenization ────────────────────────────────────────────────────────

fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

// ── IDF computation ─────────────────────────────────────────────────────

fn compute_idf(scene_tokens: &[Vec<String>]) -> std::collections::HashMap<String, f64> {
    use std::collections::{HashMap, HashSet};

    let n = scene_tokens.len() as f64;
    let mut doc_freq: HashMap<String, usize> = HashMap::new();

    for tokens in scene_tokens {
        let unique: HashSet<&String> = tokens.iter().collect();
        for tok in unique {
            *doc_freq.entry(tok.clone()).or_insert(0) += 1;
        }
    }

    doc_freq
        .into_iter()
        .map(|(tok, df)| (tok, (n / df as f64).ln().max(0.0)))
        .collect()
}

// ── Phase 1: Coarse scene matching ──────────────────────────────────────

fn coarse_scene_match(
    scene_token_sets: &[Vec<String>],
    transcript_tokens: &[String],
    idf: &std::collections::HashMap<String, f64>,
    character_names: &[String],
) -> Vec<(usize, f64)> {
    use std::collections::HashSet;

    let transcript_set: HashSet<&String> = transcript_tokens.iter().collect();
    let char_set: HashSet<&str> = character_names.iter().map(|s| s.as_str()).collect();

    let mut scores: Vec<(usize, f64)> = scene_token_sets
        .iter()
        .enumerate()
        .map(|(idx, scene_tokens)| {
            let scene_set: HashSet<&String> = scene_tokens.iter().collect();

            // Weighted Jaccard with IDF and character-name boosting.
            let mut intersection_weight = 0.0;
            let mut union_weight = 0.0;

            for tok in scene_set.union(&transcript_set) {
                let w = idf.get(tok.as_str()).copied().unwrap_or(1.0);
                // Boost character names 2×.
                let boost = if char_set.contains(tok.as_str()) {
                    2.0
                } else {
                    1.0
                };
                let weight = w * boost;

                if scene_set.contains(tok) && transcript_set.contains(tok) {
                    intersection_weight += weight;
                }
                union_weight += weight;
            }

            let score = if union_weight > 0.0 {
                intersection_weight / union_weight
            } else {
                0.0
            };

            (idx, score)
        })
        .collect();

    // Sort by score descending.
    scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scores
}

// ── Phase 2: Smith-Waterman local alignment ─────────────────────────────

/// Smith-Waterman local alignment between transcript words and scene words.
///
/// Returns (raw_score, transcript_start_word_idx, transcript_end_word_idx).
fn smith_waterman_align(
    transcript_words: &[String],
    scene_words: &[String],
) -> (i32, usize, usize) {
    let m = transcript_words.len();
    let n = scene_words.len();

    if m == 0 || n == 0 {
        return (0, 0, 0);
    }

    const MATCH_SCORE: i16 = 2;
    const MISMATCH_PENALTY: i16 = -1;
    const GAP_PENALTY: i16 = -1;

    // Allocate scoring matrix using i16 to save memory.
    // Use flat Vec for cache friendliness.
    let cols = n + 1;
    let mut matrix = vec![0i16; (m + 1) * cols];
    let idx = |i: usize, j: usize| -> usize { i * cols + j };

    let mut max_score: i16 = 0;
    let mut max_i = 0;
    let mut max_j = 0;

    for i in 1..=m {
        for j in 1..=n {
            let match_val = if transcript_words[i - 1] == scene_words[j - 1] {
                MATCH_SCORE
            } else {
                MISMATCH_PENALTY
            };

            let diag = matrix[idx(i - 1, j - 1)] + match_val;
            let up = matrix[idx(i - 1, j)] + GAP_PENALTY;
            let left = matrix[idx(i, j - 1)] + GAP_PENALTY;

            let val = diag.max(up).max(left).max(0);
            matrix[idx(i, j)] = val;

            if val > max_score {
                max_score = val;
                max_i = i;
                max_j = j;
            }
        }
    }

    // Traceback to find alignment start in transcript.
    let mut i = max_i;
    let mut j = max_j;
    let end_idx = if max_i > 0 { max_i - 1 } else { 0 };

    while i > 0 && j > 0 && matrix[idx(i, j)] > 0 {
        let diag = matrix[idx(i - 1, j - 1)];
        let up = matrix[idx(i - 1, j)];
        let left = matrix[idx(i, j - 1)];

        if diag >= up && diag >= left {
            i -= 1;
            j -= 1;
        } else if up >= left {
            i -= 1;
        } else {
            j -= 1;
        }
    }

    let start_idx = i; // i is now pointing at the row before the first match

    (max_score as i32, start_idx, end_idx)
}

// ── Timestamp mapping ───────────────────────────────────────────────────

/// Map word indices from the flattened transcript back to nanosecond timestamps.
///
/// The `all_words` slice is the same tokenized sequence used for alignment.
/// We walk through `segments` word-by-word to find the corresponding SubtitleWord
/// timestamps for the start and end indices.
fn word_indices_to_timestamps(
    segments: &[SubtitleSegment],
    _all_words: &[String],
    start_word_idx: usize,
    end_word_idx: usize,
) -> (u64, u64) {
    // Flatten all words from segments with their timestamps.
    let mut flat_words: Vec<(u64, u64)> = Vec::new();
    for seg in segments {
        if seg.words.is_empty() {
            // If no word-level timing, use segment-level as one "word".
            flat_words.push((seg.start_ns, seg.end_ns));
        } else {
            for w in &seg.words {
                flat_words.push((w.start_ns, w.end_ns));
            }
        }
    }

    if flat_words.is_empty() {
        return (0, 0);
    }

    // Tokenization may produce a different number of tokens than subtitle words
    // (e.g. hyphenated words, contractions). Use a proportional mapping.
    let n_flat = flat_words.len();
    let start = if start_word_idx < n_flat {
        flat_words[start_word_idx].0
    } else {
        flat_words.last().map_or(0, |w| w.0)
    };
    let end = if end_word_idx < n_flat {
        flat_words[end_word_idx].1
    } else {
        flat_words.last().map_or(0, |w| w.1)
    };

    (start, end)
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clip::{SubtitleSegment, SubtitleWord};
    use crate::media::script::{Script, Scene, ScriptElement, ScriptElementKind};

    fn make_segment(text: &str, start_ns: u64, end_ns: u64) -> SubtitleSegment {
        let words: Vec<SubtitleWord> = text
            .split_whitespace()
            .enumerate()
            .map(|(i, w)| {
                let n = text.split_whitespace().count() as u64;
                let dur = (end_ns - start_ns) / n.max(1);
                SubtitleWord {
                    start_ns: start_ns + i as u64 * dur,
                    end_ns: start_ns + (i as u64 + 1) * dur,
                    text: w.to_string(),
                }
            })
            .collect();
        SubtitleSegment {
            id: uuid::Uuid::new_v4().to_string(),
            start_ns,
            end_ns,
            text: text.to_string(),
            words,
        }
    }

    fn make_scene(id: &str, heading: &str, dialogue: &[&str]) -> Scene {
        let mut elements = Vec::new();
        let mut full_parts = vec![heading.to_lowercase()];
        for d in dialogue {
            elements.push(ScriptElement {
                kind: ScriptElementKind::Dialogue,
                text: d.to_string(),
                character: None,
            });
            full_parts.push(d.to_lowercase());
        }
        Scene {
            id: id.to_string(),
            scene_number: None,
            heading: heading.to_string(),
            elements,
            full_text: full_parts.join(" "),
        }
    }

    #[test]
    fn test_smith_waterman_exact_match() {
        let a: Vec<String> = vec!["hello", "world", "foo"]
            .into_iter()
            .map(String::from)
            .collect();
        let b: Vec<String> = vec!["hello", "world"]
            .into_iter()
            .map(String::from)
            .collect();
        let (score, start, end) = smith_waterman_align(&a, &b);
        assert!(score > 0);
        assert_eq!(start, 0);
        assert_eq!(end, 1);
    }

    #[test]
    fn test_smith_waterman_no_match() {
        let a: Vec<String> = vec!["aaa", "bbb"].into_iter().map(String::from).collect();
        let b: Vec<String> = vec!["ccc", "ddd"].into_iter().map(String::from).collect();
        let (score, _, _) = smith_waterman_align(&a, &b);
        assert_eq!(score, 0);
    }

    #[test]
    fn test_align_basic() {
        let script = Script {
            path: "test.fountain".to_string(),
            title: None,
            scenes: vec![
                make_scene("s1", "INT. OFFICE - DAY", &["hello there john"]),
                make_scene("s2", "EXT. PARK - NIGHT", &["the birds are singing"]),
            ],
        };

        let transcripts = vec![
            (
                "clip1.mp4".to_string(),
                vec![make_segment("hello there john", 0, 3_000_000_000)],
            ),
            (
                "clip2.mp4".to_string(),
                vec![make_segment("the birds are singing", 0, 4_000_000_000)],
            ),
        ];

        let result = align_transcripts_to_script(&script, &transcripts, 0.1);
        assert_eq!(result.mappings.len(), 2);
        assert!(result.unmatched_clips.is_empty());
        assert_eq!(result.mappings[0].scene_id, "s1");
        assert_eq!(result.mappings[1].scene_id, "s2");
    }

    #[test]
    fn test_align_unmatched() {
        let script = Script {
            path: "test.fountain".to_string(),
            title: None,
            scenes: vec![make_scene("s1", "INT. OFFICE", &["specific dialogue here"])],
        };

        let transcripts = vec![(
            "random.mp4".to_string(),
            vec![make_segment("completely different content about nothing", 0, 5_000_000_000)],
        )];

        let result = align_transcripts_to_script(&script, &transcripts, 0.8);
        assert!(result.mappings.is_empty());
        assert_eq!(result.unmatched_clips.len(), 1);
    }

    #[test]
    fn test_tokenize() {
        let tokens = tokenize("Hello, World! It's a test.");
        assert_eq!(tokens, vec!["hello", "world", "it's", "a", "test"]);
    }

    #[test]
    fn test_multi_scene_clip_splitting() {
        // A clip whose transcript spans dialogue from two distinct scenes.
        let script = Script {
            path: "test.fountain".to_string(),
            title: None,
            scenes: vec![
                make_scene(
                    "s1",
                    "INT. KITCHEN - MORNING",
                    &["good morning honey would you like some coffee"],
                ),
                make_scene(
                    "s2",
                    "INT. KITCHEN - MORNING LATER",
                    &["the eggs are burning oh no call the fire department"],
                ),
            ],
        };

        // Single clip contains dialogue from both scenes back-to-back.
        let transcripts = vec![(
            "kitchen_take.mp4".to_string(),
            vec![
                make_segment(
                    "good morning honey would you like some coffee",
                    0,
                    4_000_000_000,
                ),
                make_segment(
                    "the eggs are burning oh no call the fire department",
                    4_000_000_000,
                    9_000_000_000,
                ),
            ],
        )];

        let result = align_transcripts_to_script(&script, &transcripts, 0.1);

        // Should produce two mappings for the same clip — one per scene.
        assert_eq!(
            result.mappings.len(),
            2,
            "Expected 2 mappings (one per scene), got {}",
            result.mappings.len()
        );
        assert!(
            result.unmatched_clips.is_empty(),
            "Clip should not be unmatched"
        );

        // Both mappings refer to the same source file.
        assert_eq!(result.mappings[0].clip_source_path, "kitchen_take.mp4");
        assert_eq!(result.mappings[1].clip_source_path, "kitchen_take.mp4");

        // They should map to different scenes (order by scene order in script).
        assert_eq!(result.mappings[0].scene_id, "s1");
        assert_eq!(result.mappings[1].scene_id, "s2");

        // The source_in/out ranges should not overlap.
        assert!(
            result.mappings[0].source_out_ns <= result.mappings[1].source_in_ns
                || result.mappings[0].source_in_ns >= result.mappings[1].source_out_ns,
            "Sub-clip ranges should not overlap: [{}, {}) vs [{}, {})",
            result.mappings[0].source_in_ns,
            result.mappings[0].source_out_ns,
            result.mappings[1].source_in_ns,
            result.mappings[1].source_out_ns,
        );
    }

    #[test]
    fn test_resolve_overlapping_regions() {
        // Two candidates that overlap — higher confidence wins.
        let candidates = vec![
            CandidateRegion {
                scene_idx: 0,
                confidence: 0.9,
                t_start: 0,
                t_end: 5,
            },
            CandidateRegion {
                scene_idx: 1,
                confidence: 0.7,
                t_start: 3,
                t_end: 8,
            },
        ];
        let accepted = resolve_overlapping_regions(&candidates);
        assert_eq!(accepted.len(), 1, "Overlapping regions: only highest confidence kept");
        assert_eq!(accepted[0].scene_idx, 0);
    }

    #[test]
    fn test_resolve_non_overlapping_regions() {
        // Two non-overlapping candidates — both kept.
        let candidates = vec![
            CandidateRegion {
                scene_idx: 0,
                confidence: 0.9,
                t_start: 0,
                t_end: 4,
            },
            CandidateRegion {
                scene_idx: 1,
                confidence: 0.8,
                t_start: 5,
                t_end: 9,
            },
        ];
        let accepted = resolve_overlapping_regions(&candidates);
        assert_eq!(accepted.len(), 2, "Non-overlapping regions should both be kept");
        // Should be sorted by t_start.
        assert_eq!(accepted[0].scene_idx, 0);
        assert_eq!(accepted[1].scene_idx, 1);
    }

    #[test]
    fn test_single_scene_clip_still_works() {
        // Regression: a clip that matches only one scene should still produce one mapping.
        let script = Script {
            path: "test.fountain".to_string(),
            title: None,
            scenes: vec![
                make_scene("s1", "INT. OFFICE", &["hello there john how are you"]),
                make_scene("s2", "EXT. PARK", &["the weather is beautiful today"]),
            ],
        };

        let transcripts = vec![(
            "office.mp4".to_string(),
            vec![make_segment("hello there john how are you", 0, 3_000_000_000)],
        )];

        let result = align_transcripts_to_script(&script, &transcripts, 0.1);
        assert_eq!(result.mappings.len(), 1);
        assert_eq!(result.mappings[0].scene_id, "s1");
    }
}
