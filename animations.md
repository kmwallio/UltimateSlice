# Animations and Vector Overlays in UltimateSlice

## Status (2026-04-12)

**Build: green. 1088/1088 tests passing.** Drawing overlays and title
animations (Typewriter / Fade / Pop) are wired end-to-end for both preview
and export. What remains is UI polish.

## Completed

### Data model (`src/model/clip.rs`)
- `TitleAnimation` enum: `None | Typewriter | Fade | Pop`.
- `DrawingKind` enum + `DrawingItem` struct (kind / normalized points /
  color / width / optional fill_color).
- `ClipKind::Drawing` variant.
- `Clip` persists `title_animation`, `title_animation_duration_ns` (default
  1 s), and `drawing_items`; all serialize with `#[serde(default)]`.

### Undo (`src/undo.rs`)
- `AddClipCommand`, `SetDrawingItemsCommand`,
  `SetTitleAnimationCommand`, `SetTitleAnimationDurationCommand`.

### Interactive canvas (`src/ui/transform_overlay.rs`)
- Draw tool (`D`) activates freehand stroke capture on the program monitor.
- Strokes commit on mouse-up via `SetDrawingItemsCommand`, creating a
  `ClipKind::Drawing` clip at the playhead if one doesn't exist.
- `DragState` carries the stroke's color + width so multiple colors per
  clip are preserved.

### Inspector (`src/ui/inspector.rs`)
- Title Animation dropdown + Duration slider hydrate from the selected clip
  and write back via direct mutation + dirty flag.

### Drawing rasterization (`src/media/drawing_render.rs`)
- `rasterize_drawing_surface` renders Stroke / Rectangle / Ellipse / Arrow
  onto an ARGB32 Cairo surface (arrows get a procedural triangular head).
- `rasterize_drawing_to_png` writes the surface as a straight-alpha RGBA
  PNG via the `png` crate (pure Rust; avoids cairo-rs `png` feature dance).
- `ensure_drawing_png` caches the PNG by content hash in the OS temp dir.
- `animation_progress`, `typewriter_visible_chars` — shared animation math.
- 4/4 unit tests passing.

### Preview: drawing clips (`src/ui/window.rs::clip_to_program_clips`)
- Drawing clips are intercepted, rasterized to PNG at 1920×1080, and the
  downstream `ProgramClip` is rewritten with `kind = Image` + the PNG path
  so the existing imagefreeze/pngdec pipeline handles them.

### Preview: procedural title animations (`src/media/program_player.rs`)
- New `ProgramPlayer::apply_title_animations(timeline_pos_ns)` is called
  from the 33 ms (~30 FPS) program-monitor tick in `window.rs`.
- **Typewriter**: live-sets `textoverlay.text` to the visible prefix.
- **Fade**: scales the compositor pad's alpha, multiplied by the clip's
  keyframed `opacity_at_timeline_ns` so procedural fade and manual
  opacity keyframes compose multiplicatively.
- **Pop**: scales the compositor pad's width/height from 0 → native about
  the pad centre, multiplied by the clip's keyframed `scale_at_timeline_ns`.
- All paths skip when the playhead is outside the clip's range.

### Export: drawings (`src/media/export.rs::flatten_clips`)
- Drawing clips are rasterized (via the shared `drawing_render` helper)
  and rewritten as Image clips so the existing image-overlay export path
  handles them identically to preview.

### Export: title animations (`src/media/export.rs::build_title_filter`
  and its `prerender_build_title_filter` mirror in `program_player.rs`)
- **Fade**: single `drawtext` with `alpha='min(1,max(0,t/dur))*base_alpha'`.
- **Typewriter**: cascade of `drawtext` filters, one per character, each
  active in an exclusive time window via `enable='between(t,t0,t1)'` (the
  final threshold uses `gte(t,tk)`) rendering a progressively longer
  prefix. Works through both FFmpeg export and the prerender pipeline.
- **Pop**: falls back to static rendering (FFmpeg `drawtext.fontsize`
  evaluates once at init, so per-frame font scaling isn't supported —
  preview still animates it via compositor-pad scaling).
- 6/6 title-filter unit tests passing, including two new regression
  tests for the fade alpha expression and typewriter cascade.

### Draw-tool shape selection + ghost preview (`src/ui/transform_overlay.rs`)
- `TransformOverlay` now stores `drawing_color` / `drawing_width` /
  `drawing_kind` / `drawing_fill` as `Rc<Cell>` state with public
  setters (`set_drawing_color`, `set_drawing_width`, `set_drawing_kind`,
  `set_drawing_fill`) so the toolbar / inspector can drive them.
- **Keyboard shape picker**: with the Draw tool active, keys
  `1 / 2 / 3 / 4` select Stroke / Rectangle / Ellipse / Arrow.
- **Delete last item**: `Delete` / `Backspace` while the Draw tool is
  active pops the most recently committed drawing item from the
  clip under the playhead (via `SetDrawingItemsCommand`, so Undo
  restores it). Wired through a new `on_drawing_delete_last`
  callback on `TransformOverlay::new`.
- **Ghost preview**: dragging now renders the actual shape being drawn
  (rectangle outline + optional fill, ellipse, arrow with procedural
  head) rather than the raw polyline for shape kinds. Strokes retain
  the freehand polyline preview.
- On mouse-up, shapes commit only their start + end points; freehand
  strokes keep the full path. Fill color is applied only to Rectangle
  and Ellipse.
- The Draw toolbar button's tooltip documents every shortcut for
  discoverability.

### Draw-tool brush popover (`src/ui/window.rs`)
- A `MenuButton` in the header (graphics icon) opens a popover with
  shape-kind DropDown, stroke color (`ColorDialogButton` with alpha),
  width SpinButton, and fill toggle + fill color. Every control
  calls the matching `TransformOverlay` setter; the HUD re-renders
  on the next redraw so feedback is immediate.

### QuickTime RLE container (`src/media/drawing_render.rs`)
- Animated drawings are now baked to QuickTime RLE in `.mov`
  instead of VP9/alpha in `.webm`. The VP9 alpha path worked in
  the GStreamer preview but FFmpeg's matroska decoder reported
  frames as `yuv420p` (alpha stripped) even though the stream
  metadata said `alpha_mode: 1`, so exported overlays landed on a
  fully opaque black matte. `qtrle` in MOV uses an `argb` pixel
  format both decoders honour end-to-end.
- Cache filename extension changed from `.webm` to `.mov`, so the
  content-hash keys are effectively fresh — old `.webm` artefacts
  can be deleted from the OS temp dir without functional loss.

### Background encode + frame-rate match (`src/media/drawing_render.rs`)
- Animation encodes no longer freeze the UI. The preview path calls
  `ensure_drawing_animation_webm_nonblocking` instead of the blocking
  variant: on a cache hit the WebM is returned immediately, on a
  miss the encode runs in a `std::thread::spawn`'d worker while the
  current pass falls back to the static PNG.
- A thread-local `PENDING_DRAWING_ENCODES` set dedupes concurrent
  encode requests; `DRAWING_ENCODE_COMPLETE` (installed by the app
  at startup) fires `on_project_changed` from `glib::idle_add_once`
  when the worker finishes, so the animated version takes over on
  the next preview pass.
- Both preview (`clip_to_program_clips`) and export (`flatten_clips`)
  now honour the project's `frame_rate` instead of hardcoded 30 fps —
  a 60 fps project bakes a 60 fps animation; a 24 fps project bakes
  at 24 fps. `clip_to_program_clips` grew two new params
  (`project_fps_num`, `project_fps_den`); call sites thread them
  through from `proj.frame_rate`.

### Hit-test selection + encode feedback
- `TransformOverlay` now holds a `drawing_items_snapshot` plus a
  `selected_drawing_item: Option<usize>`. The 33 ms tick pushes the
  items under the playhead via `set_current_drawing_items`, so the
  overlay always has a current hit-test source.
- Clicks on the monitor without measurable motion (> 3 px) run
  `drawing_item_hit` against the snapshot (reverse iteration →
  top-most wins). Strokes + arrows use
  `point_to_segment_distance`, rects use edge-or-fill hit tests,
  ellipses use normalised-radius distance. Tolerance scales with
  per-item stroke width (floor 4 px × 1.8 = ~8 px).
- Selected item renders a cyan dashed bounding rectangle (offset
  by 4 px) in the overlay's draw pass.
- `Delete` / `Backspace` now routes through
  `on_drawing_delete_at(Option<usize>)`: `Some(idx)` removes the
  selected item via `SetDrawingItemsCommand`; `None` falls back to
  LIFO. Selection clears on each delete.
- `drawing_encode_is_pending()` exposes the thread-local pending
  set so the overlay shows a "Baking drawing animation…" pill
  (cyan text on dark pill) whenever a WebM bake is in flight,
  visible in *any* tool. When the Draw tool is active, the
  existing brush HUD grows an inline `• baking animation…` suffix
  in the same spot.

### In-video drawing reveal (`src/media/drawing_render.rs`)
- New `Clip.drawing_animation_reveal_ns: u64` (serde default 0).
  0 keeps the existing static PNG path; non-zero triggers a baked
  animation.
- `item_reveal_progress(idx, elapsed_ns, reveal_ns)` shares the
  stagger model with `drawing_svg` (70% overlap between consecutive
  items). `rasterize_drawing_surface_at_time` extends the Cairo
  rasteriser with time-based partial rendering: freehand strokes
  and arrow lines are truncated along their measured polyline
  length; shapes + arrowheads fade via alpha — matching the SVG
  `drawing_svg` output exactly.
- `ensure_drawing_animation_webm` bakes the clip's reveal to a
  VP9 / alpha WebM via FFmpeg stdin pipe, keyed by a content hash
  so content-identical clips reuse the cache. Re-runs only when
  the drawing or its timing change.
- `clip_to_program_clips` (preview) and `flatten_clips` (export)
  both route animated drawing clips through the WebM as a normal
  `ClipKind::Video` source; static drawings keep the PNG /
  `ClipKind::Image` path. Graceful fallback to static on encoder
  failure.
- Brush popover got an **Enable reveal animation** checkbox +
  per-item duration slider (0.1–3.0 s), writing to
  `drawing_animation_reveal_ns` for the drawing clip under the
  playhead and invalidating the preview through `on_project_changed`.
- 3 new unit tests on the progress math + partial-reveal
  rasterisation.

### SVG round-trip import (`src/media/drawing_svg.rs` + `build_source_clip`)
- Exports from `drawing_to_svg` are now stamped with
  `xmlns:us="urn:ultimateslice"` + `us:source="ultimate-slice-drawing-v1"`
  on the root `<svg>`, plus an `us:animated` flag.
- `try_parse_ultimate_slice_svg(content)` walks the stamped document
  and reconstructs `Vec<DrawingItem>` + `reveal_ns` from the
  SMIL `dur` attribute:
  * `<path d="M x y L …">` → Stroke
  * `<rect>` → Rectangle (with `fill` + `fill-opacity` preserved)
  * `<ellipse>` → Ellipse
  * `<line>` immediately followed by `<polygon>` → Arrow
  Unknown SVGs return `None` — the parser is deliberately narrow.
- `build_source_clip` intercepts `.svg` sources headed for an
  `Image` clip: if the file is one of our own SVG exports, it
  builds a `ClipKind::Drawing` clip with the decoded items +
  timing instead. Preview + export + SVG re-export keep working
  because everything downstream already handles drawing clips.
- 3 new unit tests (reject foreign SVGs, round-trip strokes +
  shapes + arrows + timing, static export gives `reveal_ns = 0`).
  Total drawing_svg tests: 8; overall suite 1100 / 1100.

### SVG sidecar export (`src/media/drawing_svg.rs` + brush popover)
- `drawing_to_svg(items, w, h, animation)` serialises a drawing clip
  as a self-contained SVG 1.1 document.
- Static mode renders all items at once.
- Animated mode emits SMIL: freehand strokes + arrow lines animate
  `stroke-dashoffset` from `pathLength=1` → 0 (natural draw-on
  reveal); rectangles, ellipses, and arrowheads animate `opacity`
  0 → 1. Each item is staggered by a configurable delay (default
  0.6 s reveal, 0.4 s stagger) and freezes at full visibility.
- Two buttons in the brush popover — "Static SVG…" and
  "Animated SVG…" — find the drawing clip under the playhead on
  the selected track, open a save dialog, and write the file.
  Surfaces an alert if no drawing clip is at the playhead.
- 5 unit tests (viewBox / SMIL staggering / opacity on shapes /
  arrow head / empty input) — all green.

### In-monitor HUD (`src/ui/transform_overlay.rs`)
- When the Draw tool is active, a dark pill in the canvas top-left
  shows current shape kind, a color chip, stroke width, a `+fill`
  suffix, and a short reminder of `1/2/3/4` + `Del` shortcuts. Combined
  with the crosshair cursor, toggling Draw gives two distinct visual
  signals.

## Remaining polish (not blocking use)

All of the items below are listed in ROADMAP.md under "Drawings &
Procedural Animations" — the feature itself is usable end-to-end
today (draw → animate → preview → export → save/load round-trip
→ SVG sidecar → re-import → animate again).

- **Option B — minimal SMIL interpreter for third-party animated
  SVGs.** Today only our own exports (stamped or pre-stamp, caught
  via the structural heuristic) round-trip into `ClipKind::Drawing`.
  A ~150-line SMIL evaluator over `usvg::Tree` handling
  `stroke-dashoffset` + `opacity` would cover arbitrary "draw-on"
  SVGs from the wider ecosystem; full SMIL (motion paths,
  keyframe splines, `animateColor`) stays out of scope.
- **Cache janitor.** `/tmp/ultimate-slice-drawing-*.{png,mov}` files
  accumulate indefinitely across sessions (content-hashed, never
  cleaned). An LRU / age-based sweep at startup would keep it
  under control.
- **Rectangle / ellipse "grow from corner" reveal style** as an
  alternative to the current alpha fade (per-clip setting).
- **Live cursor-follow ghost preview** for freehand strokes —
  today they commit on mouse-up only.
- **Drawing presets** (named `(color, width, fill)` combos in the
  brush popover).

## Testing what's wired

1. `cargo build` — clean (286 deprecation/dead-code warnings, 0 errors).
2. `cargo test --bin ultimate-slice` — **1088 passed**, 0 failed.
3. Launch the app, open any project.
4. Press `D`, drag on the program monitor → freehand stroke commits and
   a Drawing clip appears at the playhead on the selected track; the
   stroke renders in both preview and exported MP4.
5. Add a Title clip, open Inspector, set Animation to Typewriter → text
   types out character by character during playback; scrub the playhead
   to pin any intermediate state. Same for Fade (opacity ramp) and Pop
   (scale-from-zero).
6. Export → animated titles and drawings appear in the output MP4.
