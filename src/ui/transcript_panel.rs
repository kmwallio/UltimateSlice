//! Transcript-Based Editing panel.
//!
//! Bottom-of-window panel that flattens every clip's STT subtitle words into
//! one read-only `TextView` in timeline order. The user can:
//!
//! - Click any word to seek the playhead to its timeline-absolute position.
//! - Shift-click (or click then shift-click) to select a contiguous word
//!   range *within a single clip*.
//! - Press `Delete` / `Backspace` (or click the **Delete Range** button) to
//!   split the underlying clip at the selection edges and ripple-delete the
//!   middle slice in one undo entry.
//!
//! Cross-clip selections are rejected with a status-bar message — v1 only
//! deletes ranges that live in one clip. The active word is highlighted as
//! playback advances, driven by the existing 33 ms playhead poll in
//! `window.rs`.
//!
//! All editing flows through `TimelineState::delete_transcript_word_range`,
//! the same helper invoked by the MCP tool — there is no panel-only logic
//! that bypasses the model.

use crate::model::clip::Clip;
use crate::model::project::Project;
use crate::model::track::Track;
use crate::ui::timeline::TimelineState;
use gtk4::prelude::*;
use gtk4::{
    self as gtk, Box as GBox, Button, EventControllerKey, GestureClick, Label, Orientation,
    TextBuffer, TextTag, TextView, WrapMode,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// One entry per visible word. Maps a `TextBuffer` character offset range to
/// the originating clip + clip-local word time range, plus a timeline-absolute
/// span used for click-to-seek and active-word highlighting.
///
/// `track_id`, `segment_idx`, `word_idx`, and `text` are not consumed by the
/// panel itself — they exist so the MCP `delete_transcript_range` tool and
/// future automation can resolve a word back to its segment without
/// re-walking the project.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct FlatWordRef {
    /// Owning clip's id.
    pub clip_id: String,
    /// Owning track's id.
    pub track_id: String,
    /// Index of the segment inside the clip.
    pub segment_idx: usize,
    /// Index of the word inside the segment.
    pub word_idx: usize,
    /// Word text as displayed in the buffer.
    pub text: String,
    /// Clip-local 1× start time (0 = clip's `source_in`).
    pub clip_local_start_ns: u64,
    /// Clip-local 1× end time (0 = clip's `source_in`).
    pub clip_local_end_ns: u64,
    /// Timeline-absolute start ns: `clip.timeline_start + (start / speed)`.
    pub timeline_start_ns: u64,
    /// Timeline-absolute end ns: `clip.timeline_start + (end / speed)`.
    pub timeline_end_ns: u64,
    /// Character offset in the `TextBuffer` where the word's text begins.
    pub buffer_start_offset: i32,
    /// Character offset in the `TextBuffer` where the word's text ends
    /// (exclusive, matching `TextIter` semantics).
    pub buffer_end_offset: i32,
}

/// Build a flat list of `FlatWordRef`s for every clip on every track that has
/// subtitle words, in timeline order. The function is pure — it takes only an
/// `&[Track]` slice (typically obtained via `resolve_editing_tracks`) — so it
/// is unit-testable.
///
/// Buffer offsets are computed assuming each word is rendered with one space
/// after it and that each clip's words are preceded by a one-line header
/// `[clip-name]\n` followed by a trailing newline. The caller must use the
/// returned offsets when actually filling the `TextBuffer` so the click-hit
/// math stays in sync with what the user sees.
pub fn build_flat_word_cache(tracks: &[Track]) -> (String, Vec<FlatWordRef>) {
    let mut text = String::new();
    let mut words = Vec::new();

    // Walk clips in timeline order. Many tracks → flatten then sort by
    // (timeline_start_ns, track_index) so the spoken order matches what the
    // user hears during playback.
    let mut entries: Vec<(usize, &Track, &Clip)> = Vec::new();
    for (track_idx, track) in tracks.iter().enumerate() {
        for clip in &track.clips {
            if !clip.subtitle_segments.is_empty() {
                entries.push((track_idx, track, clip));
            }
        }
    }
    entries.sort_by_key(|(track_idx, _, c)| (c.timeline_start, *track_idx));

    for (_, track, clip) in entries {
        let speed = if clip.speed > 0.0 {
            clip.speed as f64
        } else {
            1.0
        };

        // Header line: `[clip-name]\n`
        text.push('[');
        text.push_str(&clip.label);
        text.push(']');
        text.push('\n');

        for (seg_idx, segment) in clip.subtitle_segments.iter().enumerate() {
            for (word_idx, word) in segment.words.iter().enumerate() {
                let buffer_start = char_count(&text);
                text.push_str(&word.text);
                let buffer_end = char_count(&text);
                text.push(' ');

                let timeline_start_ns = clip.timeline_start + (word.start_ns as f64 / speed) as u64;
                let timeline_end_ns = clip.timeline_start + (word.end_ns as f64 / speed) as u64;

                words.push(FlatWordRef {
                    clip_id: clip.id.clone(),
                    track_id: track.id.clone(),
                    segment_idx: seg_idx,
                    word_idx,
                    text: word.text.clone(),
                    clip_local_start_ns: word.start_ns,
                    clip_local_end_ns: word.end_ns,
                    timeline_start_ns,
                    timeline_end_ns,
                    buffer_start_offset: buffer_start,
                    buffer_end_offset: buffer_end,
                });
            }
            // Newline between segments to break long paragraphs.
            text.push('\n');
        }
        // Blank line between clips for readability.
        text.push('\n');
    }
    (text, words)
}

/// `TextBuffer` offsets are character offsets, not byte offsets.
fn char_count(s: &str) -> i32 {
    s.chars().count() as i32
}

/// Binary-search the flat word cache for the word containing the given
/// timeline-absolute nanosecond. Returns `None` when `ns` falls in a gap
/// between words. Boundary inclusion: `[start, end)`.
pub fn word_at_timeline_ns(words: &[FlatWordRef], ns: u64) -> Option<usize> {
    if words.is_empty() {
        return None;
    }
    // Binary search by start_ns: find the rightmost word whose start <= ns,
    // then verify ns < that word's end.
    let mut lo = 0usize;
    let mut hi = words.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        if words[mid].timeline_start_ns <= ns {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if lo == 0 {
        return None;
    }
    let candidate = lo - 1;
    if ns < words[candidate].timeline_end_ns {
        Some(candidate)
    } else {
        None
    }
}

/// Look up the word index whose buffer offset range contains the given
/// `TextBuffer` character offset.
fn word_at_buffer_offset(words: &[FlatWordRef], offset: i32) -> Option<usize> {
    let mut lo = 0usize;
    let mut hi = words.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        if words[mid].buffer_start_offset <= offset {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if lo == 0 {
        return None;
    }
    let candidate = lo - 1;
    if offset < words[candidate].buffer_end_offset {
        Some(candidate)
    } else {
        None
    }
}

/// External handle owned by `window.rs` so the playhead poll and
/// `on_project_changed` callback can drive the panel without going through
/// any GTK widget search.
pub struct TranscriptPanelView {
    text_view: TextView,
    buffer: TextBuffer,
    words: RefCell<Vec<FlatWordRef>>,
    /// `(start_idx, end_idx)` inclusive, both indexing `words`. Always within
    /// a single clip — extending across clips is rejected.
    selection: RefCell<Option<(usize, usize)>>,
    /// Index of the word currently highlighted by the playhead poll.
    last_highlighted_idx: Cell<Option<usize>>,
    /// Tag applied to selection range (light-blue background).
    selection_tag: TextTag,
    /// Tag applied to the active spoken word (yellow background).
    active_word_tag: TextTag,
    delete_button: Button,
    status_label: Label,
    timeline_state: Rc<RefCell<TimelineState>>,
    on_project_changed: Rc<dyn Fn()>,
    on_seek: Rc<dyn Fn(u64)>,
}

impl TranscriptPanelView {
    /// Rebuild the flat word cache and refill the `TextBuffer` from the
    /// current project. Called whenever the project changes (clip add/move/
    /// delete, subtitle generation, etc.).
    pub fn rebuild_from_project(&self, project: &Project) {
        let tracks = self.resolve_tracks(project);
        let (text, words) = build_flat_word_cache(tracks);
        self.buffer.set_text(&text);
        *self.words.borrow_mut() = words;
        // Selection and highlight refer to indices that no longer exist after
        // a rebuild — clear them.
        *self.selection.borrow_mut() = None;
        self.last_highlighted_idx.set(None);
        self.delete_button.set_sensitive(false);
        self.status_label.set_text("");
    }

    /// Highlight the word containing the current playhead, removing the
    /// previous highlight if it changed. Cheap enough to call every 33 ms.
    pub fn update_playhead(&self, _project: &Project, playhead_ns: u64) {
        let words = self.words.borrow();
        let new_idx = word_at_timeline_ns(&words, playhead_ns);
        let prev_idx = self.last_highlighted_idx.get();
        if new_idx == prev_idx {
            return;
        }
        if let Some(prev) = prev_idx {
            if let Some(prev_word) = words.get(prev) {
                let start = self.buffer.iter_at_offset(prev_word.buffer_start_offset);
                let end = self.buffer.iter_at_offset(prev_word.buffer_end_offset);
                self.buffer.remove_tag(&self.active_word_tag, &start, &end);
            }
        }
        if let Some(idx) = new_idx {
            if let Some(word) = words.get(idx) {
                let start = self.buffer.iter_at_offset(word.buffer_start_offset);
                let end = self.buffer.iter_at_offset(word.buffer_end_offset);
                self.buffer.apply_tag(&self.active_word_tag, &start, &end);
            }
        }
        self.last_highlighted_idx.set(new_idx);
    }

    /// Returns the resolved editing tracks for the current compound nav
    /// state, mirroring the timeline widget's behavior.
    fn resolve_tracks<'a>(&self, project: &'a Project) -> &'a [Track] {
        // We can't borrow timeline_state here because the caller may already
        // hold its borrow. Inline the same walk that `resolve_editing_tracks`
        // uses, but read the nav stack via a try_borrow. If contended,
        // fall back to root tracks (correct for the common no-drill-down case).
        let nav = match self.timeline_state.try_borrow() {
            Ok(st) => st.compound_nav_stack.clone(),
            Err(_) => Vec::new(),
        };
        let mut tracks: &[Track] = &project.tracks;
        for compound_id in &nav {
            let found = tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == *compound_id && c.is_compound());
            if let Some(compound) = found {
                if let Some(ref inner) = compound.compound_tracks {
                    tracks = inner;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        tracks
    }

    /// Apply the visual selection tag to the buffer range covered by the
    /// current `selection` field, removing any prior selection highlight.
    fn refresh_selection_tag(&self) {
        // Remove existing tag everywhere first.
        let start = self.buffer.start_iter();
        let end = self.buffer.end_iter();
        self.buffer.remove_tag(&self.selection_tag, &start, &end);

        let sel = *self.selection.borrow();
        let words = self.words.borrow();
        let Some((a, b)) = sel else {
            self.delete_button.set_sensitive(false);
            return;
        };
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let (Some(first), Some(last)) = (words.get(lo), words.get(hi)) else {
            self.delete_button.set_sensitive(false);
            return;
        };
        let s = self.buffer.iter_at_offset(first.buffer_start_offset);
        let e = self.buffer.iter_at_offset(last.buffer_end_offset);
        self.buffer.apply_tag(&self.selection_tag, &s, &e);
        self.delete_button.set_sensitive(true);
    }

    /// Resolve a click into a word index, then either start a new selection
    /// (single click) or extend the existing one (shift click).
    fn handle_click(&self, x: f64, y: f64, shift: bool) {
        let Some((mut buffer_x, buffer_y)) = self.window_to_buffer_coords(x, y) else {
            return;
        };
        // y can be slightly past the last line on a click in trailing whitespace
        // — clamp x so the iter lookup still hits a word when possible.
        if buffer_x < 0 {
            buffer_x = 0;
        }
        let Some(iter) = self.text_view.iter_at_location(buffer_x, buffer_y) else {
            return;
        };
        let offset = iter.offset();
        let words = self.words.borrow();
        let Some(idx) = word_at_buffer_offset(&words, offset) else {
            // Clicking in whitespace clears selection.
            drop(words);
            *self.selection.borrow_mut() = None;
            self.status_label.set_text("");
            self.refresh_selection_tag();
            return;
        };
        let new_word = words[idx].clone();
        drop(words);

        let mut sel = self.selection.borrow_mut();
        if shift {
            if let Some((anchor, _)) = *sel {
                // Reject cross-clip extension.
                let words = self.words.borrow();
                if let Some(anchor_word) = words.get(anchor) {
                    if anchor_word.clip_id != new_word.clip_id {
                        self.status_label
                            .set_text("Selection must stay within one clip");
                        return;
                    }
                }
                *sel = Some((anchor, idx));
                drop(sel);
                self.status_label.set_text("");
                drop(words);
                self.refresh_selection_tag();
                return;
            }
        }
        *sel = Some((idx, idx));
        drop(sel);
        self.status_label.set_text("");
        self.refresh_selection_tag();
        // Single click also seeks the playhead to the start of this word.
        (self.on_seek)(new_word.timeline_start_ns);
    }

    /// Convert a window-relative click position to buffer coordinates.
    fn window_to_buffer_coords(&self, x: f64, y: f64) -> Option<(i32, i32)> {
        let (bx, by) =
            self.text_view
                .window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        Some((bx, by))
    }

    /// Run the actual delete operation against the model. Called by the
    /// Delete button and the Delete/BackSpace key handler.
    fn delete_selection(&self) {
        let sel = *self.selection.borrow();
        let Some((a, b)) = sel else {
            return;
        };
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let (clip_id, word_start_ns, word_end_ns) = {
            let words = self.words.borrow();
            let (Some(first), Some(last)) = (words.get(lo), words.get(hi)) else {
                return;
            };
            if first.clip_id != last.clip_id {
                self.status_label
                    .set_text("Selection must stay within one clip");
                return;
            }
            (
                first.clip_id.clone(),
                first.clip_local_start_ns,
                last.clip_local_end_ns,
            )
        };

        let changed = self
            .timeline_state
            .borrow_mut()
            .delete_transcript_word_range(&clip_id, word_start_ns, word_end_ns);
        if !changed {
            self.status_label.set_text("Nothing to delete");
            return;
        }
        // Clear local state — rebuild_from_project will fire via
        // on_project_changed and refill the panel from the new model.
        *self.selection.borrow_mut() = None;
        self.last_highlighted_idx.set(None);
        self.status_label.set_text("");
        // Notify project change after dropping any borrows. timeline_state
        // borrow is already released by the .borrow_mut() temporary above.
        TimelineState::notify_project_changed(&self.timeline_state);
        // The chained on_project_changed in window.rs will call our
        // rebuild_from_project, but call it through the project ref directly
        // as a safety net so the panel never lags behind a successful edit.
        (self.on_project_changed)();
    }
}

/// Build the transcript panel widget tree. Mirrors `build_keyframe_editor`'s
/// signature so `window.rs` can drop it next to the dopesheet.
pub fn build_transcript_panel(
    project: Rc<RefCell<Project>>,
    timeline_state: Rc<RefCell<TimelineState>>,
    on_project_changed: Rc<dyn Fn()>,
    on_seek: Rc<dyn Fn(u64)>,
) -> (GBox, Rc<TranscriptPanelView>) {
    let root = GBox::new(Orientation::Vertical, 6);
    root.set_margin_top(4);
    root.set_margin_bottom(4);
    root.set_margin_start(4);
    root.set_margin_end(4);

    let title = Label::new(Some("Transcript"));
    title.add_css_class("browser-header");
    title.set_halign(gtk::Align::Start);
    root.append(&title);

    let controls = GBox::new(Orientation::Horizontal, 6);
    let delete_button = Button::with_label("Delete Range");
    delete_button.add_css_class("small-btn");
    delete_button.set_tooltip_text(Some(
        "Split the underlying clip at the selection edges and ripple-delete the middle slice (Delete / Backspace)",
    ));
    delete_button.set_sensitive(false);
    controls.append(&delete_button);

    let status_label = Label::new(None);
    status_label.set_halign(gtk::Align::Start);
    status_label.add_css_class("dim-label");
    controls.append(&status_label);
    root.append(&controls);

    let buffer = TextBuffer::new(None);
    let text_view = TextView::with_buffer(&buffer);
    text_view.set_editable(false);
    text_view.set_cursor_visible(false);
    text_view.set_wrap_mode(WrapMode::Word);
    text_view.set_monospace(false);
    text_view.set_left_margin(8);
    text_view.set_right_margin(8);
    text_view.set_top_margin(4);
    text_view.set_bottom_margin(4);
    text_view.set_can_focus(true);

    // Tags applied to selection ranges and active words. Colors chosen to
    // contrast with both the dark default theme and any future light theme.
    let selection_tag = buffer
        .create_tag(Some("ts-selection"), &[("background", &"#3a4f7a")])
        .expect("create selection tag");
    let active_word_tag = buffer
        .create_tag(
            Some("ts-active-word"),
            &[
                ("background", &"#bf9000"),
                ("foreground", &"#000000"),
                ("weight", &600i32),
            ],
        )
        .expect("create active word tag");

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_child(Some(&text_view));
    scroller.set_vexpand(true);
    root.append(&scroller);

    let view = Rc::new(TranscriptPanelView {
        text_view: text_view.clone(),
        buffer: buffer.clone(),
        words: RefCell::new(Vec::new()),
        selection: RefCell::new(None),
        last_highlighted_idx: Cell::new(None),
        selection_tag,
        active_word_tag,
        delete_button: delete_button.clone(),
        status_label: status_label.clone(),
        timeline_state: timeline_state.clone(),
        on_project_changed: on_project_changed.clone(),
        on_seek: on_seek.clone(),
    });

    // Initial fill so the panel isn't empty before the first
    // on_project_changed tick.
    {
        let proj = project.borrow();
        view.rebuild_from_project(&proj);
    }

    // Click → seek; shift-click → extend selection.
    {
        let view = view.clone();
        let click = GestureClick::new();
        click.set_button(gtk4::gdk::BUTTON_PRIMARY);
        click.connect_pressed(move |gesture, _, x, y| {
            let event = gesture.current_event();
            let shift = event
                .map(|e| {
                    e.modifier_state()
                        .contains(gtk4::gdk::ModifierType::SHIFT_MASK)
                })
                .unwrap_or(false);
            view.handle_click(x, y, shift);
        });
        text_view.add_controller(click);
    }

    // Delete / BackSpace → delete current selection.
    {
        let view = view.clone();
        let key = EventControllerKey::new();
        key.connect_key_pressed(move |_, keyval, _keycode, _state| {
            use gtk4::gdk::Key;
            match keyval {
                Key::Delete | Key::BackSpace => {
                    view.delete_selection();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        text_view.add_controller(key);
    }

    {
        let view = view.clone();
        delete_button.connect_clicked(move |_| {
            view.delete_selection();
        });
    }

    (root, view)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clip::{Clip, ClipKind, SubtitleSegment, SubtitleWord};
    use crate::model::track::Track;

    fn make_word(start_ns: u64, end_ns: u64, text: &str) -> SubtitleWord {
        SubtitleWord {
            start_ns,
            end_ns,
            text: text.to_string(),
        }
    }

    fn make_segment(words: Vec<SubtitleWord>, text: &str) -> SubtitleSegment {
        let start = words.first().map(|w| w.start_ns).unwrap_or(0);
        let end = words.last().map(|w| w.end_ns).unwrap_or(0);
        SubtitleSegment {
            id: "seg-test".to_string(),
            start_ns: start,
            end_ns: end,
            text: text.to_string(),
            words,
        }
    }

    fn make_clip(timeline_start: u64, source_in: u64, source_out: u64) -> Clip {
        let mut c = Clip::new("/tmp/test.mp4", source_out, timeline_start, ClipKind::Video);
        c.source_in = source_in;
        c.source_out = source_out;
        c.label = "TestClip".to_string();
        c
    }

    #[test]
    fn flat_word_cache_walks_timeline_order() {
        let mut clip_a = make_clip(0, 0, 10_000_000_000);
        clip_a.label = "A".to_string();
        clip_a.subtitle_segments = vec![make_segment(
            vec![
                make_word(1_000_000_000, 1_500_000_000, "hello"),
                make_word(1_500_000_000, 2_000_000_000, "world"),
            ],
            "hello world",
        )];
        let mut clip_b = make_clip(20_000_000_000, 0, 5_000_000_000);
        clip_b.label = "B".to_string();
        clip_b.subtitle_segments = vec![make_segment(
            vec![make_word(0, 500_000_000, "again")],
            "again",
        )];

        let mut track = Track::new_video("V1");
        track.clips = vec![clip_a, clip_b];
        let tracks = vec![track];
        let (text, words) = build_flat_word_cache(&tracks);

        assert_eq!(words.len(), 3);
        assert_eq!(words[0].text, "hello");
        assert_eq!(words[1].text, "world");
        assert_eq!(words[2].text, "again");
        // Buffer offsets must be strictly ordered and consistent with the
        // text we built.
        assert!(words[0].buffer_start_offset < words[1].buffer_start_offset);
        assert!(words[1].buffer_end_offset < words[2].buffer_start_offset);
        // Sanity-check the substring extraction.
        let chars: Vec<char> = text.chars().collect();
        let extract = |w: &FlatWordRef| -> String {
            chars[w.buffer_start_offset as usize..w.buffer_end_offset as usize]
                .iter()
                .collect()
        };
        assert_eq!(extract(&words[0]), "hello");
        assert_eq!(extract(&words[1]), "world");
        assert_eq!(extract(&words[2]), "again");
    }

    #[test]
    fn flat_word_cache_respects_clip_speed() {
        let mut clip = make_clip(10_000_000_000, 0, 10_000_000_000);
        clip.speed = 2.0;
        clip.subtitle_segments = vec![make_segment(
            vec![make_word(2_000_000_000, 4_000_000_000, "fast")],
            "fast",
        )];
        let mut track = Track::new_video("V1");
        track.clips = vec![clip];
        let (_text, words) = build_flat_word_cache(&[track]);
        assert_eq!(words.len(), 1);
        // word.start_ns / speed = 2_000_000_000 / 2 = 1_000_000_000
        assert_eq!(words[0].timeline_start_ns, 11_000_000_000);
        // word.end_ns / speed = 4_000_000_000 / 2 = 2_000_000_000
        assert_eq!(words[0].timeline_end_ns, 12_000_000_000);
    }

    #[test]
    fn word_at_timeline_ns_boundary_cases() {
        let words = vec![
            FlatWordRef {
                clip_id: "c".into(),
                track_id: "t".into(),
                segment_idx: 0,
                word_idx: 0,
                text: "a".into(),
                clip_local_start_ns: 0,
                clip_local_end_ns: 0,
                timeline_start_ns: 100,
                timeline_end_ns: 200,
                buffer_start_offset: 0,
                buffer_end_offset: 1,
            },
            FlatWordRef {
                clip_id: "c".into(),
                track_id: "t".into(),
                segment_idx: 0,
                word_idx: 1,
                text: "b".into(),
                clip_local_start_ns: 0,
                clip_local_end_ns: 0,
                timeline_start_ns: 300,
                timeline_end_ns: 400,
                buffer_start_offset: 2,
                buffer_end_offset: 3,
            },
        ];
        // Before any word: None.
        assert_eq!(word_at_timeline_ns(&words, 50), None);
        // Exact start: hits.
        assert_eq!(word_at_timeline_ns(&words, 100), Some(0));
        // Mid-word: hits.
        assert_eq!(word_at_timeline_ns(&words, 150), Some(0));
        // Exact end (exclusive): no hit.
        assert_eq!(word_at_timeline_ns(&words, 200), None);
        // In the gap between words: no hit.
        assert_eq!(word_at_timeline_ns(&words, 250), None);
        // Second word.
        assert_eq!(word_at_timeline_ns(&words, 350), Some(1));
        // Past the end.
        assert_eq!(word_at_timeline_ns(&words, 500), None);
    }

    #[test]
    fn word_at_buffer_offset_finds_words() {
        let words = vec![
            FlatWordRef {
                clip_id: "c".into(),
                track_id: "t".into(),
                segment_idx: 0,
                word_idx: 0,
                text: "hello".into(),
                clip_local_start_ns: 0,
                clip_local_end_ns: 0,
                timeline_start_ns: 0,
                timeline_end_ns: 0,
                buffer_start_offset: 5,
                buffer_end_offset: 10,
            },
            FlatWordRef {
                clip_id: "c".into(),
                track_id: "t".into(),
                segment_idx: 0,
                word_idx: 1,
                text: "world".into(),
                clip_local_start_ns: 0,
                clip_local_end_ns: 0,
                timeline_start_ns: 0,
                timeline_end_ns: 0,
                buffer_start_offset: 11,
                buffer_end_offset: 16,
            },
        ];
        assert_eq!(word_at_buffer_offset(&words, 4), None);
        assert_eq!(word_at_buffer_offset(&words, 5), Some(0));
        assert_eq!(word_at_buffer_offset(&words, 9), Some(0));
        // Trailing space — outside the word's range.
        assert_eq!(word_at_buffer_offset(&words, 10), None);
        assert_eq!(word_at_buffer_offset(&words, 11), Some(1));
        assert_eq!(word_at_buffer_offset(&words, 15), Some(1));
        assert_eq!(word_at_buffer_offset(&words, 16), None);
    }
}
