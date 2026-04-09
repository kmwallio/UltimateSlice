# UltimateSlice — Improvement Plan

A backlog of code-quality, deduplication, structural, and constants improvements
identified across the codebase. This is **not** a roadmap of new features
(see `ROADMAP.md` for that) and **not** authoritative on architectural
invariants (see `docs/ARCHITECTURE.md`).

## How to use this document

- Items are grouped by **priority tier** (P0 → P4), not by file or subsystem.
- Each item has concrete file:line citations so it can be picked up cold.
- When an item lands, append a `CHANGELOG.md` entry under **Unreleased** and
  remove (or strike through) the item here.
- Several items are marked **(touches compound clips)** — those must preserve
  the rules in `docs/ARCHITECTURE.md` "Compound Clips, Timelines & Coordinate
  Spaces" or risk regressing the windowing fixes from the 2026‑04 session.
- Several items are marked **(touches GTK callback safety)** — those must
  preserve the borrow-and-drop rules in `docs/ARCHITECTURE.md` "Critical
  Rules for GTK4 + RefCell". A double-borrow inside a GTK trampoline is a
  hard abort with no panic recovery.

---

## Priority tiers

| Tier | Theme | Risk if ignored |
|---|---|---|
| **P0** | Correctness / safety | Silent failures, stale state, hard-to-diagnose bugs |
| **P1** | High-impact dedup | Bug surface widens — same fix has to be applied in N places |
| **P2** | Constants & magic numbers | Inconsistency, hard to retune, time-conversion math errors |
| **P3** | Large-file splits | Files becoming unworkable; build/edit/review friction |
| **P4** | Polish | Logging gaps, dead code, weak test coverage |

---

## P0 — Correctness & safety

### ~~P0.1 — Borrow-safety helper for callback dispatch~~ ✅ DONE (widget.rs only)

**(touches GTK callback safety)**

> **Landed:** `TimelineState::notify_project_changed(&state)` helper added in
> `src/ui/timeline/widget.rs` with full documentation explaining the GTK
> callback safety rationale. 18 explicit-drop sites in `widget.rs` migrated
> (including 4 conditional-capture blocks that became dead code). The
> architecture doc now points to the helper as the preferred pattern.
>
> **Still pending:** the equivalent sites in `window.rs` (~261 references —
> most are call-paths from outside the timeline state where the helper is
> not needed; the explicit-drop subset is small but each needs review) and
> `inspector.rs` (~4 references). Sweep them in a follow-up PR alongside
> the P3.2 / P3.3 splits where the surrounding callback structure changes
> anyway.

### ~~P0.2 — Silent failures with `let _ =`~~ ✅ DONE (program_player GStreamer state-changes still pending)

> **Landed:** All 8 cited sites in `tracking.rs`, all 4 in `animated_svg.rs`,
> and the 2 appsink drain sites in `program_player.rs` now either log a
> `warn!` (for cache dir creation, work channel sends, partial-file cleanup)
> or carry an explanatory comment (for genuinely uninteresting discards like
> cache-file-already-gone, periodic progress channel sends, child kill/wait
> races on cancel, scope appsink drains).
>
> **Still pending:** the ~230 `let _ = self.pipeline.set_state(…)` and
> `let _ = decoder.seek(…)` sites in `program_player.rs` are intentionally
> deferred. Many of those are in shutdown / cleanup / fast-path code where
> partial failures are expected during state transitions, and a blanket
> sweep would generate noise without surfacing actionable errors. Pick them
> up alongside the P3.1 program_player split where each one can be reviewed
> in the context of its containing function.

### P0.3 — `Result<_, String>` mixed with `anyhow::Result`
Eighteen files return `Result<_, String>` while neighbours return
`anyhow::Result<_>`. This means error context is lost at module boundaries
and `?` propagation requires manual `.map_err(|e| e.to_string())` calls.

Define a project-wide error enum (`thiserror`-based) and migrate the
`Result<_, String>` callsites. Top offenders by call count:

- `src/media/program_player.rs` — 41 unwrap + 35 expect
- `src/fcpxml/parser.rs` — 39 unwrap + 84 expect
- `src/fcpxml/writer.rs` — 172 expect
- `src/media/player.rs` — 52 unwrap

Note: most `unwrap()` / `expect()` calls in the codebase are in `#[cfg(test)]`
blocks; that's fine. The audit should focus on production paths.

### P0.4 — Note on `panic!()`
A grep for `panic!(` finds matches in `mcp/server.rs`, `media/program_player.rs`,
`media/export.rs`, `otio/schema.rs`, and `media/music_gen.rs`, but **all of them
are inside `#[cfg(test)]` modules**. There are currently **no `panic!()` calls
in production code paths**. Keep it that way during refactors — particularly
when extracting MCP handlers (P3.5), do not introduce new panics in
`handle_mcp_command()` arms.

---

## P1 — High-impact deduplication

### ~~P1.1 — Three flattening paths share the same windowing logic ⭐~~ ✅ DONE
**(touches compound clips)**

> **Landed:** `Clip::rebase_to_window(window_start, window_end) -> Option<Clip>` extracted to `src/model/clip.rs` with eleven edge-case unit tests. All three call sites (`clip_to_program_clips` in `window.rs`, `flatten_clips` in `export.rs`, `break_apart_compound` in `widget.rs`) now delegate to the helper. The architectural rule that parent-timeline rebasing must stay in the caller is documented in the helper's docstring with a pointer back to `docs/ARCHITECTURE.md`.

Three sites implement the same `compound source_in/source_out` windowing
algorithm (skip-clips → trim-edges → rebase-keyframes-and-subtitles):

| Path | File | Lines |
|---|---|---|
| Preview playback | `src/ui/window.rs` | 3759-3790 |
| MP4 export | `src/media/export.rs` | 5399-5428 |
| Break-apart compound | `src/ui/timeline/widget.rs` | 2681-2716 |

All three follow identical structure:

```rust
let left_trim = window_start.saturating_sub(windowed.timeline_start);
if left_trim > 0 {
    windowed.source_in = windowed.source_in.saturating_add(left_trim);
    windowed.timeline_start = window_start;
}
let mut right_trim = 0u64;
if windowed.timeline_end() > window_end {
    right_trim = windowed.timeline_end() - window_end;
    windowed.source_out = windowed.source_out.saturating_sub(right_trim);
}
if left_trim > 0 || right_trim > 0 {
    let range_end = orig_duration.saturating_sub(right_trim);
    windowed.retain_keyframes_in_local_range(left_trim, range_end);
    windowed.retain_subtitles_in_local_range(left_trim, range_end);
}
```

**Proposed fix:** Extract a method on `Clip` in `src/model/clip.rs`:

```rust
impl Clip {
    /// Trim a clone of this clip to the visible window
    /// `[window_start, window_end]`. Returns `None` if the clip is entirely
    /// outside the window. Keyframes and subtitles are rebased to stay
    /// aligned with the trimmed content.
    pub fn rebase_to_window(&self, window_start: u64, window_end: u64) -> Option<Clip> { ... }
}
```

All three sites collapse to a single call. The architectural invariant
becomes impossible to forget because the helper enforces it.

### ~~P1.2 — MCP argument extraction boilerplate~~ ✅ DONE

> **Landed:** five `arg_str!` / `arg_bool!` / `arg_f64!` / `arg_u64!` /
> `arg_i64!` macros in `src/mcp/server.rs`, replacing **191 inline sites**
> (the plan's "75+" estimate was low). Each macro has two forms — with and
> without an explicit default — to cover the empty-default and the
> per-tool custom-default cases (`"smooth"`, `"medium"`, `"none"`, etc.).
>
> **Still pending:** `arg_string_array!` and `arg_f64_map!` for the array/
> object extraction patterns. The current implementations of those (e.g.
> the LADSPA params object at server.rs ~2086) are short enough that a
> macro doesn't pay for itself; promote later if a third site appears.

### P1.3 — Manual `project.tracks` iteration where recursive helpers exist
**(touches compound clips)**

`docs/ARCHITECTURE.md` mandates `Project::clip_ref` / `clip_mut` / `track_mut`
for all lookups so compound-internal clips are reachable. Past sessions
replaced ~130 sites, but new code occasionally adds inline iteration.

Sweep periodically with grep for patterns like
`for track in &mut .*proj.*tracks`, `proj.tracks.iter().flat_map`,
`tracks.iter().find_map(|t| t.clips.iter().find(`.

Also add a convenience helper to `Project` for the common
"given clip_id, give me the track id" pattern:

```rust
impl Project {
    pub fn find_track_id_for_clip(&self, clip_id: &str) -> Option<String> { ... }
}
```

Existing callsites that would benefit:
- `src/ui/inspector.rs:838-843`, `5080-5086`
- `src/ui/window.rs:1367-1374`
- `src/ui/timeline/widget.rs:2665-2675`

### P1.4 — Undo command boilerplate
**File:** `src/undo.rs`

30+ `EditCommand` impls all share the same skeleton:

```rust
fn execute(&self, project: &mut Project) {
    if let Some(track) = project.track_mut(&self.track_id) {
        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
            // mutate
        }
    }
    project.dirty = true;
}
```

(See `TrimClipCommand` 56-78, `TrimOutCommand` 88-108, `SplitClipCommand`
498-535 as representative examples.)

**Proposed fix:** Generic clip-mutation wrapper:

```rust
pub struct ClipMutateCommand<T: Clone> {
    pub clip_id: String,
    pub old_state: T,
    pub new_state: T,
    pub apply: fn(&mut Clip, T),
    pub label: &'static str,
}

impl<T: Clone> EditCommand for ClipMutateCommand<T> {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            (self.apply)(clip, self.new_state.clone());
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            (self.apply)(clip, self.old_state.clone());
        }
        project.dirty = true;
    }
    fn description(&self) -> &str { self.label }
}
```

Also: ripple-edit logic at lines 120-248 is duplicated between
`RippleTrimOutCommand` and `RippleTrimInCommand`. Extract
`apply_ripple_delta(track: &mut Track, threshold_ns: u64, delta_ns: i64)`.

### ~~P1.5 — FCPXML keyframe emission duplication~~ ✅ DONE (position merge still pending)

> **Landed:** `emit_keyframe_animation_param` helper extracted in
> `src/fcpxml/writer.rs`. Five sites migrated — the scale and rotation
> blocks inside `write_transform_keyframe_params`, plus the standalone
> `write_opacity_keyframe_params`, `write_volume_keyframe_params`, and
> `write_pan_keyframe_params` functions. Each former 30-40 line block is
> now a 10-line helper invocation with a per-property `format_value`
> closure. All 144 FCPXML round-trip tests still pass.
>
> **Still pending:** the position-merge `merge_keyframe_times` extraction —
> the position writer still has its own x/y merge loop because it doesn't
> fit the simple shape of the helper. Defer to a follow-up PR if a third
> caller appears.

### ~~P1.6 — RGBA-from-u32 unpacking~~ ✅ DONE

> **Landed:** `src/ui/colors.rs` now exports three helpers — `rgba_u32_to_u8`,
> `rgba_u32_to_f64`, `rgba_u32_to_f32` — covering the three byte-shift call
> patterns that the original plan only saw one of (the inspector's
> `gdk4::RGBA::new` call sites need `f32`, not `f64`). All 91 sites across
> `media/export.rs`, `media/program_player.rs`, `ui/window.rs`,
> `ui/inspector.rs`, `ui/timeline/widget.rs`, and `otio/writer.rs` were swept.

### P1.7 — Inspector slider connect-handler boilerplate
**File:** `src/ui/inspector.rs`

36+ sliders each spell out the same `clone_clip_id_ref → borrow → mutate
clip → fire on_changed` block. The existing `connect_color_slider()`
helper and `wire_color_slider!` macro reduce some duplication, but each
slider still passes ~5 clone arguments by hand.

Propose a more general factory keyed by a `mutate_fn: fn(&mut Clip, f64)`:

```rust
fn connect_clip_property_slider(
    slider: &Scale,
    clip_id_ref: &Rc<RefCell<Option<String>>>,
    project: &Rc<RefCell<Project>>,
    updating: &Rc<RefCell<bool>>,
    on_value_changed: &Rc<dyn Fn()>,
    mutate: fn(&mut Clip, f64),
)
```

Couple this work with P3.3 (inspector split) — they reinforce each other.

### ~~P1.8 — Track-kind helper methods~~ ✅ DONE

> **Landed:** `Track::is_video()` / `Track::is_audio()` added to `src/model/track.rs`. The 38 sites that previously inline-compared `track.kind == TrackKind::Video|Audio` (across `model::project`, `edl::writer`, `otio::writer`, `fcpxml::writer`, `media::export`, `ui::window`, and `ui::timeline::widget`) now use the helpers. Two newly-unused `TrackKind` imports were also removed from `model::project` and `media::export`.

---

## P2 — Constants & magic numbers

The codebase already declares ~166 `const` items across 40 files (mostly
local to the module). The goal here is to **deduplicate cross-cutting
constants** and centralize ones that are inlined as literals.

### P2.1 — Time conversions ⭐ CRITICAL — module landed; full callsite sweep deferred
Inlined `1_000_000_000` for ns/sec appears in many call sites despite
being defined locally as `NS_PER_SECOND` in `src/ui/timeline/widget.rs:32`
and re-defined in `src/ui/preview.rs:12`. Twelve+ literal `1_000_000_000`
inlines in `src/media/export.rs` alone (293, 321, 351, 461, 536, 556,
642, 701, 706, 1051). `1_000_000` (ns/ms) inlined in
`src/ui/inspector.rs:2220` and `src/ui/window.rs:3925-3930`.

**Status:** `src/units.rs` exists and exports `NS_PER_SECOND` (`u64`) and
`NS_PER_SECOND_F` (`f64`). The three duplicate declarations in
`widget.rs:32`, `preview.rs:12`, and `timecode.rs:3` now delegate via
`use … as` aliases / explicit casts so callsites in those files were not
disturbed. **Still TODO:** the 444 inline literals across the rest of the
codebase (382 × `1_000_000_000`, 62 × `1_000_000` excluding the ns/sec
matches). Migrate site-by-site in follow-up PRs — `src/media/export.rs`
has the highest concentration and is the natural starting point.

When picking up the deferred sweep, also re-add the constants that were
removed from this PR for being unused (`NS_PER_MS`, `NS_PER_MS_F`,
`NS_PER_US`, `US_PER_SECOND`, `MS_PER_SECOND`) — the spec for them is in
the closed PR's version of this file.

### ~~P2.2 — Snap & hit-test thresholds~~ ✅ DONE (curves_editor still pending)

> **Landed:** `SNAP_TOLERANCE_PX` (timeline/widget.rs), `KEYFRAME_SNAP_TOLERANCE_NS`
> (model/clip.rs, promoted from two private `const`s inside `impl Clip` methods
> to a single module-level public constant), `TRANSFORM_HANDLE_RADIUS_PX` /
> `TRANSFORM_HANDLE_HIT_RADIUS_PX` (transform_overlay.rs).
>
> **Still pending:** Curve point hit radius in `src/ui/curves_editor.rs:13,14` —
> defer with a follow-up since that file uses its own naming conventions and a
> rename should be reviewed alongside any other curves-editor cleanup.

### ~~P2.3 — Preview zoom levels (defined ×3)~~ ✅ DONE

> **Landed:** `PROGRAM_MONITOR_ZOOM_LEVELS` constant in
> `src/ui/program_monitor.rs`. The plan's "preview.rs" citation was stale —
> the duplicates were actually in `program_monitor.rs`.

### ~~P2.4 — Frame rates and resolution presets~~ ✅ DONE (export-dialog presets still pending)

> **Landed:** `FRAMERATE_OPTIONS` and `ASPECT_RATIO_PRESETS` const tables in
> `src/ui/toolbar.rs`. The Project Settings dialog now reads from those
> tables in all four places (label list, init lookup, change handler,
> response handler). The `(2997, 125)` simplified-form detection of
> 23.976 fps is preserved as an `or_else` fallback so old projects still
> match the right preset.
>
> **Still pending:** the export-dialog resolution dropdown at
> `toolbar.rs:1503` still has its own hard-coded 5-entry list (Same as
> project / 4K / 1080p / 720p / 480p). It uses different presets than
> the Project Settings dialog, so it deserves its own table; defer to a
> follow-up.

### ~~P2.5 — Color palette / theme~~ ✅ DONE (one-off colors still inline)

> **Landed:** the named theme palette lives in the existing `src/ui/colors.rs`
> module (no new `theme.rs` — `colors.rs` already exists from batch 1 and is
> the natural home). Constants added: `COLOR_SELECTION_FILL`,
> `COLOR_SELECTION_BORDER`, `COLOR_LEVEL_GOOD/WARN/CLIP`,
> `COLOR_AUDIO_DIALOGUE/EFFECTS/MUSIC/ROLE_NONE`, `COLOR_TIMELINE_BG`,
> `COLOR_TRACK_LABEL_BG`. Both timeline meters (widget.rs) and program-monitor
> meters (program_monitor.rs) share the same level constants.
>
> **Still pending:** the ~50 one-off colors in widget.rs that are unique to
> a single drawing site (compound badge `(0.18, 0.50, 0.48)`, error tints,
> through-edit indicator, etc.). The rule of three didn't fire for those —
> promote later only if they start to duplicate.

### ~~P2.6 — ITU-R BT.709 luma coefficients (duplicated)~~ ✅ DONE

> **Landed:** `LUMA_R` / `LUMA_G` / `LUMA_B` constants now live in the new
> `src/ui/colors.rs` module. Both occurrences at `program_monitor.rs:432`
> and `:478` consume them. The new module is intentionally minimal —
> designed as the future home for the deferred P1.6 RGBA-from-u32 helper
> and the P2.5 named theme palette.

### ~~P2.7 — Inspector slider ranges~~ ✅ DONE (single-use ranges still inline)

> **Landed:** four slider-range groups added at the top of `src/ui/inspector.rs`:
> `COLOR_SLIDER_*` (17 sites — brightness/exposure/tint/sharpness/grading),
> `UNIT_SLIDER_*` (12 sites — denoise/blur/vidstab/chroma/bg-removal/masks/
> opacity/title), `DOUBLE_SLIDER_*` (contrast, saturation), and
> `COLOR_TEMPERATURE_*_K` (the temperature slider). The transform sliders
> already used `model::transform_bounds` constants from an earlier release
> and were not touched.
>
> **Intentionally inline:** single-use ranges like vi pad `(0.0, 500.0, 5.0)`,
> vi fade `(0.0, 200.0, 1.0)`, vi silence threshold `(-60.0, -10.0, 1.0)`,
> eq gain `(-24.0, 12.0, 0.1)`, eq Q `(0.1, 10.0, 0.1)`, pitch shift
> `(-12.0, 12.0, 0.5)`, duck amount `(-24.0, 0.0, 0.5)`, mask feather
> `(0.0, 0.5, 0.01)`, mask expansion `(-0.5, 0.5, 0.01)`, subtitle position
> `(0.05, 0.95, 0.05)`, and subtitle word window `(2.0, 10.0, 1.0)`. The rule
> of three didn't fire — promoting them would add line noise without
> reducing duplication.

### ~~P2.8 — Font sizes~~ ✅ DONE

> **Landed:** `RULER_FONT_SIZE`, `MARKER_FONT_SIZE`,
> `TRACK_LABEL_FONT_SIZE_MIN/MAX` in `src/ui/timeline/widget.rs`. The other
> `set_font_size(10.0)` sites in widget.rs (badge labels for Solo / Duck /
> "T" / "ADJ") were intentionally left alone — they happen to share the
> ruler's pixel size but are semantically independent.

---

## P3 — Large-file splits

These are multi-week efforts. Each must preserve borrow safety and
compound-clip rules. Recommendation: do them in branches, test extensively
against the MCP test suite plus manual compound clip / drill-down /
import-export round trips, and ship in small reviewable PRs where possible.

Current sizes (lines):

| File | Lines | `.clone()` calls | Notes |
|---|---:|---:|---|
| `src/media/program_player.rs` | 19,084 | — | **Largest** |
| `src/ui/window.rs` | 20,450 | 1,699 | Most clones |
| `src/ui/timeline/widget.rs` | 10,673 | 507 | |
| `src/ui/inspector.rs` | 9,770 | 1,103 | One 6,592-line `build_inspector` |
| `src/media/export.rs` | 7,573 | — | |
| `src/fcpxml/writer.rs` | 7,409 | — | 172 `.expect()` calls |
| `src/fcpxml/parser.rs` | 5,814 | — | 84 `.expect()` calls |
| `src/model/clip.rs` | 4,898 | — | |
| `src/mcp/server.rs` | 4,223 | — | JSON-RPC plumbing only |

### P3.1 — `src/media/program_player.rs` (19,084 lines)
Largest file in the codebase. Suggested cut lines (each becomes a
submodule under `src/media/program_player/`):

- `pipeline_builder.rs` — GStreamer pipeline construction
- `renderer.rs` — Cairo frame rendering & color pipeline
- `effects.rs` — frei0r / LADSPA / mask / stabilization / color-match application
- `prerender.rs` — prerender cache & background jobs
- `state.rs` — play / pause / stop state machine

The 21 `#[allow(dead_code)]` annotations in this file should be audited
during the split — most are likely cruft from old workflows.

### P3.2 — `src/ui/window.rs` (20,450 lines)
1,699 `.clone()` calls is the highest in the codebase, indicating heavy
state duplication in callback wiring.

`handle_mcp_command()` is at line **14456** of this file (despite being
"the MCP handler" — `mcp/server.rs` is just JSON-RPC plumbing that sends
`McpCommand` variants over a channel). The handler is a giant switch with
hundreds of arms and is the natural first extraction.

Suggested submodules under `src/ui/window/`:

- `panel_layout.rs` — paned layout & workspace state setup
- `mcp_dispatch.rs` — `handle_mcp_command()` and per-tool handler arms
  (couple with **P3.5**)
- `timeline_integration.rs` — playhead sync, audio match wiring,
  on_project_changed plumbing
- `project_lifecycle.rs` — new / open / save / export workflows
- Keep `window.rs` as the orchestrator that imports the submodules

**(touches GTK callback safety)** — extracting callbacks across modules
must keep the borrow-and-drop rules intact.

### P3.3 — `src/ui/inspector.rs` (9,770 lines)
`build_inspector()` is a single function spanning lines **3093 → 9685**
(6,592 lines). Its parameter list takes 17 callbacks including a
**19-argument `on_color_changed` closure** at lines 3096-3116.

Refactor the signature first (do this *before* the file split — it's the
biggest readability win):

```rust
pub struct ColorGradeUpdate {
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub temperature: f32,
    pub tint: f32,
    // ... all 19 fields, named
}

on_color_changed: impl Fn(ColorGradeUpdate) + 'static
```

Then split `build_inspector()` per property section into submodules under
`src/ui/inspector/`:

- `clip_section.rs` — name, label, source path
- `color_section.rs` — color grading, curves, color wheels
- `audio_section.rs` — volume, EQ, voice isolation, audio match
- `transform_section.rs` — scale, position, rotation, anamorphic, crop
- `effects_section.rs` — frei0r, LADSPA, chroma key, bg removal, stabilization
- `title_section.rs` — text, font, subtitles
- `keyframe_section.rs` — speed/opacity keyframe editor

Couple with P1.7 (slider boilerplate dedup).

### P3.4 — `src/ui/timeline/widget.rs` (10,673 lines)
Suggested cut lines under `src/ui/timeline/`:

- `drawing.rs` — `draw_timeline`, `draw_clip`, waveform/keyframe markers
- `hit_test.rs` — `track_index_at_y`, `clip_at_point`, geometry helpers
- `gestures.rs` — click / drag / key controllers
- `state_mutations.rs` — move / trim / split / razor commit logic
- `widget.rs` — dispatch coordinator, struct definitions, state

**(touches compound clips)** — `docs/ARCHITECTURE.md` drill-down rules
(breadcrumb height offset, content height including breadcrumb,
`editing_playhead_ns` translation) must be preserved through the
hit-test extraction.

**(touches GTK callback safety)** — gesture handlers borrow `TimelineState`
heavily; ensure the borrow-and-drop pattern is preserved across module
boundaries.

### P3.5 — `src/mcp/server.rs` (4,223 lines) + `handle_mcp_command()` arms
The actual mcp/server.rs is JSON-RPC plumbing (`tools_list()`, `call_tool()`,
the stdio/socket loops). It's only 4K lines because the heavy lifting
happens in `handle_mcp_command()` over in `src/ui/window.rs:14456`.

Extract per-tool-category handler modules under `src/mcp/handlers/`:

- `handlers/project.rs` — create_project, get_project, list_tracks, list_clips
- `handlers/timeline.rs` — add/remove/move/trim/insert/overwrite/slip/slide
- `handlers/effects.rs` — frei0r, LADSPA, color grading, audio effects
- `handlers/export.rs` — export_mp4, save_fcpxml, save_otio, presets
- `handlers/media.rs` — list_library, import_media, relink_media, bins
- `handlers/playback.rs` — play, pause, stop, seek_playhead

Each handler takes the same shared-state arguments plus its specific
`McpCommand` variant. The `handle_mcp_command()` function in `window.rs`
becomes a thin dispatcher.

### P3.6 — `src/fcpxml/writer.rs` (7,409 lines) + `parser.rs` (5,814 lines)
Two big XML files with 172 + 84 `.expect()` calls between them.

For `writer.rs`: split into a schema-builder that constructs an
intermediate representation from `Project`, plus an XML emitter that
serializes the IR. Decouples "what to write" from "how to write it" and
enables easier testing of round-trip semantics.

For `parser.rs`: separate event-based XML stream parsing from the
AST → `Project` transformation, and replace `.expect()` with `?`
propagation through a typed error type.

### P3.7 — `src/model/clip.rs` (4,898 lines)
Less urgent than the others, but the file grew to hold subtitles,
keyframes, frei0r/LADSPA effects, multicam angles, and tracking
attachments alongside the core `Clip` struct.

Consider sub-modules under `src/model/clip/`:

- `kinds.rs` — `ClipKind`, helpers
- `keyframes.rs` — `NumericKeyframe`, interpolation, retain_*_in_range helpers
- `subtitles.rs` — `SubtitleSegment`, word timings, retain helpers
- `effects.rs` — frei0r and LADSPA effect structs
- `multicam.rs` — `MulticamAngle`, segment computation
- `clip.rs` — the `Clip` struct + core methods

This is the "model" layer so it must stay backward compatible with the
serialization format (P0.3 / project versioning).

---

## P4 — Polish

### P4.1 — Logging consistency
411 mixed `log::*` and `eprintln!()` calls across the codebase. Standardize
on the `log` crate and remove the `eprintln!` calls (keeping a single
`env_logger` init in `main.rs`).

### P4.2 — `#[allow(dead_code)]` audit (35+ instances)
- `src/media/program_player.rs` — 21 instances (worst offender)
- `src/media/player.rs` — 6 instances
- Others scattered

Each annotation should either be deleted (if the code is genuinely
unused) or get a comment explaining why it's intentionally kept (e.g.
"part of internal API used by future plugin loader").

### P4.3 — `#[allow(deprecated)]` audit
- `src/ui/timeline/widget.rs:3409, 3481, 3624, 8860`
- `src/ui/inspector.rs:2568, 2580, 4436, 4439`

Document which GTK4 API is deprecated, what the replacement is, and
whether/when to migrate.

### P4.4 — Test coverage gaps
40 files have `#[cfg(test)]`, so the codebase has decent test discipline,
but most coverage is in the model/undo/FCPXML layers. Gaps:

- **MCP handlers** — only basic dispatch tests; no per-tool error-path coverage
- **Inspector property builders** — none (would benefit from the P3.3 split)
- **Timeline hit-test/geometry** — none (would benefit from the P3.4 split)
- **`Clip::rebase_to_window` helper** (after P1.1 extraction) — should
  ship with full tests for the windowing edge cases that the 2026‑04
  compound-clip session uncovered
- **GStreamer pipeline / effects** — none

### P4.5 — Function signatures with too many parameters
- `build_inspector()` (17 callbacks, see P3.3)
- `handle_mcp_command()` in `window.rs:14456` takes ~20 shared-state
  references — bundle related state into a `MainThreadState` struct passed
  by reference

### P4.6 — Repeated `.clone()` chains in callback setup
Top cloners:
- `src/ui/window.rs` — 1,699
- `src/ui/inspector.rs` — 1,103
- `src/ui/timeline/widget.rs` — 507

Many are necessary for GTK callback safety, but the visual noise hides
intent. A `clone_for_callback!(state, project, history => { closure })`
macro could eliminate the boilerplate without changing semantics.

### P4.7 — `clip.duration()` vs inline `source_out - source_in`
`Clip::duration()` exists, but several call sites still compute
`source_out - source_in` inline. Sweep and replace.

---

## Suggested execution order

A reasonable rollout that minimizes risk and gets early wins:

1. ~~**P2.1** — `src/units.rs` time constants (mechanical, low risk)~~ ✅ module landed; full sweep deferred
2. ~~**P1.1** — Extract `Clip::rebase_to_window` (high impact, removes a class of compound-clip bugs forever)~~ ✅ DONE
3. ~~**P1.6** — RGBA helper~~ ✅ DONE, ~~**P1.8** — track-kind helpers~~ ✅ DONE
4. ~~**P2.2 / P2.3 / P2.6 / P2.8**~~ ✅ DONE (curves_editor hit radius still pending under P2.2)
5. **P0.2** — Audit `let _ =` patterns in background-thread code
6. **P1.2** — MCP arg extraction macros
7. **P1.4** — Generic `ClipMutateCommand<T>` wrapper in undo.rs
8. **P2.5 / P2.7** — Theme colors and slider-range constants
9. **P0.1** — `notify_project_changed` helper
10. **P2.4** — Framerate/resolution preset tables
11. **P1.5** — FCPXML keyframe emit dedup
12. **P3.5** — MCP handler extraction (small surface area)
13. **P3.3** — Inspector signature refactor + section split (couples with P1.7)
14. **P3.4** — Timeline widget split
15. **P3.1 / P3.2** — `program_player.rs` and `window.rs` splits (largest)
16. **P3.6 / P3.7** — FCPXML and Clip model splits

After each item: update `CHANGELOG.md` and remove the item from this file.

---

## See also

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — authoritative reference for
  compound clips, GTK borrow safety, GStreamer versioning
- [`ROADMAP.md`](ROADMAP.md) — feature roadmap (this file is debt, not features)
- [`CHANGELOG.md`](CHANGELOG.md) — running history of landed changes
