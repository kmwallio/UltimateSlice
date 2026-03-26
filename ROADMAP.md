# UltimateSlice Roadmap

A Final Cut Pro–inspired non-linear video editor built with GTK4 and Rust.

---

Tracking docs:
- [`CHANGELOG.md`](CHANGELOG.md) — running history of implemented changes/progress
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — agent/contributor implementation guide

## ✅ Implemented

### Foundation
- [x] GTK4 + Rust project scaffold (`gtk4-rs 0.11`, `gstreamer-rs 0.25`, `glib 0.22`)
- [x] Dark theme via custom CSS (`src/style.css`)
- [x] GTK4/libadwaita-style control polish in dark theme (linked tabs, popovers, dropdown/combo controls, sliders, check/radio)
- [x] Preferences selector styling fix: narrowed ComboBox/DropDown CSS selectors to avoid nested/double borders on settings selectors
- [x] GApplication entry point with CSS loading
- [x] GNOME HIG-compliant app icon (`data/io.github.ultimateslice.svg`) — camera-cake slice concept
- [x] GitHub Actions workflows on push for native Cargo build/test and Flatpak manifest build
- [x] Non-deprecation warning cleanup pass for `cargo build --quiet` / `cargo test --quiet` (unused imports/vars/mut, `unused_must_use`, and targeted intentional dead-code allowances)
- [x] Legacy GTK deprecation-warning suppression pass for existing Dialog/ComboBoxText UI paths with narrowly scoped `#[allow(deprecated)]` (no runtime behavior change)
- [x] Runtime GTK slider warning cleanup: added a generic slider CSS reset (border/margin/padding/box-shadow none) plus explicit scale-thumb sizing to remove negative min-size warnings (`GtkGizmo ... slider ... -4`)

### Data Model
- [x] `Clip` — source path, source in/out (ns), timeline position, label, kind
- [x] `Track` — ordered list of clips, muted/locked flags, `TrackKind` (Video/Audio)
- [x] `Project` — title, frame rate, resolution, tracks, dirty flag
- [x] `MediaItem` — library entry (path, duration, label); separate from timeline clips
- [x] `SourceMarks` — shared in/out selection state for the source monitor
- [x] Unit tests for model, undo, and FCPXML parser (62 tests)

### Media Library Browser
- [x] Import media via file chooser (video/audio/image MIME filter)
- [x] Still-image detection by extension (PNG, JPEG, GIF, BMP, TIFF, WebP, HEIC, SVG) with 4-second default duration
- [x] GStreamer Discoverer probes duration and source timecode on import (background thread via `MediaProbeCache`)
- [x] Library list with clip name + filename display
- [x] Thumbnails auto-refresh when extraction completes (debounced batch redraw) without requiring manual panel/window redraw
- [x] Selecting a library item loads it in the source preview
- [x] Imported clips are **not** auto-added to the timeline
- [x] Import no longer auto-loads Source Monitor; selecting a library item loads preview on demand (avoids import-time playbin reconfiguration races)
- [x] Project replacement (New/Open/Open Recent and MCP create/open) clears the current media-browser list before syncing target-project media

### Source Preview / Monitor
- [x] GStreamer `playbin` + `gtk4paintablesink` video display
- [x] Source preview URI reload path hardened (`Null` reconfigure + duplicate selection suppression) to avoid `gstplaysink` assertion aborts on import/selection
- [x] Source scrubber `DrawingArea` with click-to-seek
- [x] In-point (green) / Out-point (orange) markers on scrubber
- [x] Selected region highlighted in scrubber
- [x] **Set In (I)** / **Set Out (O)** keyboard shortcuts and buttons
- [x] In/Out timecode labels
- [x] Play/Pause (Space), Stop transport buttons
- [x] Timecode label (`position / duration`)
- [x] Playback-only drop-late smoothness policy for source monitor (aggressive while playing, conservative while paused/stopped)
- [x] Strict source preview behavior when proxy mode is Off (always load original media; no proxy requests)
- [x] Adaptive VA-API source decode mode (hardware-first when available and enabled) with automatic software fallback on hardware-path errors
- [x] Source monitor playback-priority mode (Smooth/Balanced/Accurate) with frame-boundary seek deduplication for paused scrubbing

### Timeline
- [x] Cairo-rendered `DrawingArea` with ruler (adaptive multi-tier tick/label density while zooming)
- [x] Multi-track rows (currently 1 Video + 1 Audio track created on project init)
- [x] Clip rendering with rounded rectangles, labels, selected highlight
- [x] Trim handles (in-edge / out-edge) shown when clip is selected
- [x] Playhead (red line + triangle) updated at 100 ms intervals from player position
- [x] **Select tool** — click to select/deselect clips
- [x] **Razor/Blade tool** — B to toggle; click splits clip at playhead
- [x] **Clip move** — drag clip body to reposition on timeline
- [x] **Trim in-point** — drag left edge of selected clip
- [x] **Trim out-point** — drag right edge of selected clip
- [x] **Seek/Scrub** — click and drag on ruler/playhead for continuous timeline scrubbing (no snap-back to 0)
- [x] **Zoom** — scroll wheel zoom (10–2000 px/s range)
- [x] **Pan** — horizontal scroll
- [x] **Undo/Redo** — Ctrl+Z / Ctrl+Y / Ctrl+Shift+Z; full command history
- [x] **Delete** — Delete/Backspace removes selected clip
- [x] **Play/Pause** — Space bar toggles player
- [x] Tool indicator overlay (Razor mode)
- [x] **Image clips** — still images (PNG, JPEG, etc.) placed as `ClipKind::Image` with 4 s default, unlimited trim-out, `imagefreeze` playback, and `tpad`-based export

### Undo / Redo System
- [x] `EditCommand` trait with `execute` / `undo` / `description`
- [x] `EditHistory` with undo/redo stacks
- [x] Commands: MoveClip, TrimIn, TrimOut, DeleteClip, SplitClip
- [x] Live drag preview with commit-to-history on drag-end

### Inspector Panel
- [x] Right-side inspector showing selected clip properties
- [x] Fields: clip name, source path, source in/out, duration, timeline start

### Toolbar / Header
- [x] New / Open / Save / Export MP4 buttons
- [x] Recent projects menu limits to 10 entries and omits missing files
- [x] Undo / Redo buttons
- [x] Select / Razor tool toggle buttons
- [x] Export Project with Media action in **Export ▼** menu (`Export Project with Media…`) that writes XML plus colocated `ProjectName.Library` packaged media

### Append to Timeline
- [x] "Append to Timeline" button in media browser
- [x] Appends marked region (in → out) of selected source clip
- [x] Placed at end of first Video track

### Export
- [x] MP4/H.264 + AAC export via ffmpeg (`-filter_complex` concat + adelay/amix for audio)
- [x] Background thread with `mpsc::channel` progress reporting
- [x] Progress estimate based on ffmpeg `total_size` versus largest imported library file, capped to 99% until completion (100% only on successful finish)
- [x] Audio from embedded video-clip streams and standalone audio-track clips included in export
- [x] Clips without audio streams safely skipped via `ffprobe` probe
- [x] Extended grading parity bridge: export prefers FFmpeg frei0r (`coloradj_RGB`, `three_point_balance`) using the same calibrated mapping as Program Monitor preview, with automatic native-filter fallback when frei0r modules are unavailable
- [x] Exposure parity alignment: export exposure now follows preview-calibrated brightness/contrast delta mapping to reduce preview/export mismatch on extreme values
- [x] Primary static-control parity retune: export `brightness`/`contrast`/`saturation` now follows preview-calibrated mapping (plus calibrated contrast-brightness bias) for closer Program Monitor/export match on flat/high-contrast looks
- [x] Tonal warmth/tint creative boost: highlights/midtones/shadows warmth+tint now use a non-linear response with stronger endpoint effect and gentle center control, while keeping preview/export mapping aligned
- [x] Warmth slider direction consistency: midtones/highlights warmth now follow standard grading direction (left cooler, right warmer) in both preview and export
- [x] Shadows warmth deep-shadow direction consistency: 3-point mapping now inverts shadows warmth in curve-space so slider direction remains conventional (left cooler, right warmer) in preview/export bridge
- [x] Stronger shadows endpoint range: shadows warmth/tint endpoint gain increased to allow more pronounced blue/gold shadow looks near slider extremes while preserving directional semantics

### FCPXML
- [x] FCPXML 1.10-1.14 import (`quick-xml`) — parses assets, spine, asset-clip elements
- [x] FCPXML 1.14 export — writes resources/format/asset + library/event/project/sequence/spine
- [x] FCPXML format export metadata parity: emit canonical `format@name` only for known presets and preserve numeric format fields for all presets (avoids hardcoded 1080p24 name mismatches)
- [x] FCPXML export writes source media in nested `media-rep` entries (`original-media` for non-proxy files, `proxy-media` for detected proxy-cache paths)
- [x] Import compatibility for Apple-authored FCPXML 1.14 files: nested `media-rep` source paths, first-project timeline selection in multi-project files, and lane/media-type fallback track routing
- [x] Marker import compatibility: parse `chapter-marker` and convert nested clip marker times (`start`/`offset` aware) to correct timeline marker positions
- [x] Standard audio gain import mapping: parse `adjust-volume@amount` (dB values such as `-6dB` / `-96dB`) into UltimateSlice clip volume multipliers
- [x] Format preset fallback: derive frame rate/resolution from known format names (e.g. `FFVideoFormat1080p30`) when numeric format fields are absent
- [x] Standard Inspector mapping (phase 1): parse/write `adjust-transform` (scale/position/rotation), `adjust-compositing` (opacity), and `adjust-crop`/`crop-rect` (crop bounds) with `us:*` fallback
- [x] Transform coordinate parity: convert FCPXML `adjust-transform@position` using frame-height percentage semantics (both axes), mapped to/from UltimateSlice's scale-aware internal position model (with Y-axis inversion), including single-clip dirty-save patch path
- [x] Preserve unknown fields on clean round-trip save for imported FCPXML (verbatim open→save passthrough when project is unmodified)
- [x] Preserve unknown imported `asset-clip` attributes and child tags on regenerated dirty saves while updating edited scale values (`us:scale` / `adjust-transform@scale`)
- [x] Preserve unknown imported resource `asset` attributes/children (including Final Cut metadata/md payloads) on regenerated dirty saves, emit `<!DOCTYPE fcpxml>`, and keep canonical nested `media-rep` source references
- [x] Preserve unknown attrs/child tags across core FCPXML document structure on regenerated dirty saves (`fcpxml`, `resources`, selected `library`/`event`/`project`/`sequence`/`spine`, and selected sequence format attrs)
- [x] Project extension UX: default Save suggestion uses `.uspxml`, Open supports `.uspxml` + `.fcpxml` (plus `.xml` fallback), and desktop metadata advertises project XML association
- [x] Shared MIME registration for UltimateSlice projects: ship `application/x-ultimateslice-project+xml` shared-mime-info definition with `*.uspxml` glob and install it in Flatpak package metadata
- [x] Dirty imported transform edits prefer in-place XML patching (when `adjust-transform` exists), preserving original asset IDs/document structure instead of full regeneration
- [x] Import fallback remaps missing `/Volumes/...` assets across common Linux external-drive mount paths (plus opened FCPXML mount root), including URI-decoded paths (e.g. `%20`), and still exports original imported source paths
- [x] Export URI safety: writer now percent-encodes `media-rep@src` file paths (spaces/special characters) for standards-friendly `file://` references
- [x] Packaged export external-drive path normalization: **Export Project with Media** rewrites Linux external mount roots (`/media`, `/run/media`, `/mnt`) to `/Volumes/<drive>/...` in saved XML for cross-platform portability
- [x] Strict packaged-export FCPXML mode: **Export Project with Media** now emits DTD-safe XML (no `xmlns:us`/`us:*` attrs, no passthrough unknown attrs/children, DTD-friendly `adjust-blend` and structured `adjust-crop` with `crop-rect`)
- [x] Extension-based strict-save routing: normal Save now uses strict compatibility writer for `.fcpxml` outputs while `.uspxml` retains feature-rich round-trip output
- [x] Strict export DTD + multitrack hardening: strict writer now emits lane-based track mapping for multi-track fallback routing and enforces DTD asset-clip intrinsic ordering (video params before audio params), with strict-mode sequence-marker suppression for validator compliance
- [x] Strict FCPXML connected clip nesting: connected clips (lane ≠ 0) are nested inside the primary storyline clip per FCPXML spec, fixing Final Cut Pro import assertion failures
- [x] Native transition import/export parity (phase 1): parse native spine `<transition>` into clip transition fields and emit native `<transition>` between adjacent clips using mapped transition names/duration/offset
- [x] Native `timeMap/timept` import/export parity (phase 1): parse 2-point constant retimes (speed/reverse/freeze) from native time maps and emit native time maps for constant speed/reverse/freeze clips
- [x] Native `timeMap/timept` import/export parity (phase 2): support representable multi-point monotonic retimes (speed ramps) via speed keyframes, while preserving unsupported mixed-direction/partial-hold maps as passthrough
- [x] Native `timeMap/timept` preservation hardening (phase 3): preserve and emit unsupported imported native timeMap fragments in timing-params order (including strict output) instead of replacing them with generated approximations
- [x] Native `timeMap/timept` easing compatibility (phase 4): map `timept@interp` smooth modes to eased speed keyframes, emit `smooth2` for non-linear native retimes, and preserve `inTime`/`outTime` maps as passthrough
- [x] Import fallback for spine `ref-clip` and `sync-clip`: parse `ref-clip@ref` via asset mapping and traverse `sync-clip`/nested `spine` containers to import nested clip items
- [x] Import source-time normalization: rebase `asset-clip@start` by `asset@start` for absolute timecode-domain assets so layered video/audio lane clips seek correctly in Program Monitor
- [x] Export transform overflow clipping: overlay clips with positions exceeding the frame boundary now crop overflow edges before padding, so exported PIP positions match the Program Monitor preview exactly
- [x] Background-threaded project open (file I/O + XML parsing off main thread)

### MCP Server (`--mcp` flag)
- [x] `--mcp` flag enables the MCP (Model Context Protocol) server at startup
- [x] JSON-RPC 2.0 over stdio (MCP 2024-11-05 protocol)
- [x] `--mcp` flag is stripped from argv before GLib sees it
- [x] Background thread reads stdin; main-thread polling via `glib::timeout_add_local`
- [x] Tools: `get_project`, `list_tracks`, `list_clips`, `add_clip`, `remove_clip`, `move_clip`, `trim_clip`, `set_project_title`, `save_fcpxml`, `export_mp4`, `list_library`, `import_media`, `relink_media`
- [x] MCP performance profiling tool `get_performance_snapshot` (prerender queue/transition hit-rate/rebuild telemetry snapshot)
- [x] MCP preference controls expanded with `set_realtime_preview` and `set_experimental_preview_optimizations` for playback-path tuning automation
- [x] MCP preference control `set_background_prerender` for early boundary prewarm tuning automation
- [x] MCP color parity calibration harness hardening: `tools/calibrate_mcp_color_match.py` now covers full clip color controls, uses repeated seek/settle stabilization + sample re-apply before export capture, and reports threshold-based pass/fail summaries
- [x] MCP parity metrics normalization: calibration report now tracks absolute RMSE and delta-from-neutral pass metrics (`pass_absolute`, `pass_delta`, combined `pass`) plus per-slider delta summaries
- [x] MCP parity low-loss export mode: calibration harness now supports preset-based ProRes/MOV capture (`--export-mode prores_mov`) to reduce compression artifacts during parity evaluation
- [x] MCP parity smoke-check helper: `tools/mcp_parity_smoke_check.py` wraps low-sample calibration and enforces broad guardrails for normalized focus-slider regressions in automation/CI
- [x] MCP parity smoke-check multi-media profile: `tools/mcp_parity_smoke_check.py` supports repeated `--media` inputs and writes aggregate cross-media pass/fail summaries (`smoke_aggregate_report.json`)
- [x] MCP parity tint retune (frei0r bridge): export `coloradj_RGB` path now attenuates tint deltas for closer preview/export matching on chart and natural-footage sweeps
- [x] MCP parity targeted slider sweeps: calibration/smoke tools now support `--sliders` so retune iterations can focus on selected high-residual controls
- [x] MCP parity retry hardening: calibration/smoke tools now support median-attempt sample + neutral-baseline retries (`--sample-retries`, `--neutral-baseline-retries`) to reduce stale-frame outlier noise
- [x] MCP parity LUT coverage: calibration/smoke tools now support `--lut-path` so parity sweeps can include clip-level `.cube` LUT processing in both preview and export
- [x] MCP parity LUT/proxy correctness: calibration/smoke tools now support `--proxy-mode`, and LUT runs auto-switch to proxy-backed capture when proxy mode is Off
- [x] MCP parity signed-bias telemetry: calibration reports now include signed per-channel bias (`export - preview`) for neutral/sample captures and slider-level mean signed-bias summaries for direction-aware fitting
- [x] MCP parity baseline-vs-candidate comparator: `tools/compare_mcp_parity_reports.py` scores retune candidates and enforces endpoint regression guardrails for risky controls
- [x] MCP parity multi-profile comparator: `tools/compare_mcp_parity_profiles.py` gates candidates across multiple baseline/candidate report pairs with per-profile + aggregate scoring
- [x] MCP parity cool-side temperature harmonization hook: export coloradj bridge supports cool-side gain via `ProgramPlayer::export_temperature_parity_gain` with unity default + runtime override
- [x] MCP parity retune-cycle wrapper: `tools/run_mcp_parity_retune_cycle.py` runs sweep + single-profile compare + multi-profile compare in one command, with optional profile weights and automatic temperature endpoint guardrails
- [x] MCP parity gain optimizer: `tools/optimize_mcp_temperature_gain.py` sweeps export parity gain sets (temperature + optional tonal endpoint gains) via repeated retune-cycle runs and selects best aggregate-scoring candidate
- [x] MCP parity gain runtime overrides: ProgramPlayer export parity supports bounded env overrides (`US_EXPORT_COOL_TEMP_GAIN`, `US_EXPORT_SHADOWS_POS_GAIN`, `US_EXPORT_MIDTONES_NEG_GAIN`, `US_EXPORT_HIGHLIGHTS_NEG_GAIN`) for automation loops
- [x] MCP parity piecewise cool-temperature shaping: export parity now supports `US_EXPORT_COOL_TEMP_GAIN_FAR` + `US_EXPORT_COOL_TEMP_GAIN_NEAR` (with legacy fallback) for two-segment cool-range fitting
- [x] MCP `get_playhead_position` tool for playhead-speed/FPS regression measurements in automated perf harnesses
- [x] Unix domain socket transport (Preferences → Integration toggle) for connecting to a running instance
- [x] `--mcp-attach` stdio-to-socket proxy so standard MCP clients can use `.mcp.json` to attach
- [x] Python stdio-to-socket MCP bridge script (`tools/mcp_socket_client.py`) with `.mcp.json` server entry (`ultimate-slice-python-socket`)
- [x] Local perf tooling scripts: `tools/mcp_call.py`, `tools/proxy_perf_matrix.sh`, and `tools/proxy_fps_regression.py`
- [x] MCP color parity calibration script: `tools/calibrate_mcp_color_match.py` (slider sweeps + preview/export RMSE report + frei0r cross-runtime probe)
- [x] `take_screenshot` tool — captures a PNG of the full application window via GTK snapshot + GSK CairoRenderer, written to the current working directory
- [x] `select_library_item`, `source_play`, `source_pause` tools — select media in the library and control Source Monitor playback via MCP
- [x] `save_project_with_media` tool — package-save the project (`.uspxml` + `ProjectName.Library` media copy with rewritten XML media paths)

---

## 🔜 Planned

### Source Monitor Improvements
- [x] Clip name shown in source monitor header
- [x] Close button to hide source preview and clear current source selection
- [x] Frame-accurate jog/shuttle control
- [x] Mark-in / Mark-out visible as timecodes in a dedicated bar
- [x] Source preview uses proxies only when proxy mode is enabled; Off mode keeps original media without proxy requests
- [x] Source preview proxy fallback parity: use original media until proxy file is ready, and retry once with original URI on proxy load/decode error
- [x] Source preview seeks continuously to In/Out marker position while dragging markers on the scrubber
- [x] Source preview drag safety: accidental self-drops are consumed as no-ops, and source playback pauses/resumes during source-clip drag operations to reduce crash-prone decode churn
- [x] Source scrubber drag safety: playhead scrub drags pause/resume playback and force a final seek on drag release (with macOS deferring live drag seeks until release for crash resistance)
- [x] Source preview macOS decode stability: software-filtered mode down-ranks `vtdec`/`vtdec_hw` to prefer non-VideoToolbox decode during source interactions
- [x] Source scrubber macOS quiesce guard: re-preroll current URI before final scrub-release seek to reduce `qtdemux` crash frequency

### Timeline Improvements
- [x] Time-mapped clip filmstrip thumbnails in video track rows (background GStreamer extraction via `ThumbnailCache`)
- [x] Timeline preview toggle to switch between full thumbnail strips and start/end-only thumbnails
- [x] Snap-to-clip-edge when moving clips (10 px threshold, snaps both start and end edges)
- [x] Multiple video tracks and audio tracks (Add/Remove Track buttons below timeline)
- [x] Audio waveform rendering in audio track rows (background GStreamer decode, normalized peaks)
- [x] Drag-and-drop from media browser onto a specific timeline track/position
- [x] Snap-to-clip-edge when trimming (TrimIn and TrimOut snap to nearby edges)
- [x] Timeline markers / chapter points
- [x] Magnetic timeline mode (gap-free)
- [x] Cross-track clip dragging (same-kind restriction)
- [x] Reorder tracks in the timeline (drag track labels)
- [x] Active track highlighting (click empty area to select, visual accent bar)
- [x] Smart Append (auto-detects audio/video, targets active or first matching track)
- [x] Transitions pane with drag-and-drop transition application to timeline boundaries
- [x] Cross-dissolve transitions between clips
- [x] Ripple edit mode (Trim In/Out)
- [x] Roll edit mode
- [x] Slip/slide edit modes
- [x] Copy/Paste (Ctrl+C/V for clips, paste-attributes, paste-insert)
- [x] Copy/Paste Color Grade (Ctrl+Alt+C/V for color-grading-only copy/paste between clips)
- [x] Multi-Select (rubber-band selection, Shift+click range select, Ctrl+A select all)
  - [x] Phase 1: Shift+click range select (same-track + cross-track time-range), Ctrl/Cmd+click toggle selection, Ctrl+A select all
  - [x] Phase 2: rubber-band marquee selection
- [x] Ripple Delete (Shift+Delete closes gap by shifting subsequent clips)
- [x] Clip grouping / ungrouping (persist clip-group IDs; grouped move/delete as a unit)
  - [x] Visual group context: selecting a grouped clip highlights non-selected group peers with a secondary border
  - [x] Align grouped clips by audio or timecode
    - [x] Phase 1: Align grouped clips by stored timecode metadata
    - [x] Phase 2: Align grouped clips by audio (FFT cross-correlation via `rustfft`)
- [x] Audio/video linking (auto-link video and audio from same source)
  - [x] Manual clip linking / unlinking with synchronized selection, move, and delete behavior
  - [x] Auto-link same-source A/V clip creation on drag-and-drop
  - [x] Optional auto-link A/V mode for source monitor operations (Append/Insert/Overwrite): enabled creates linked pairs (with embedded video-track audio suppression while linked), disabled uses single-clip placement behavior; both retain single-kind fallback when only one matching track kind exists
- [x] Solo track (play only selected tracks, complement to muted/locked)
- [x] Freeze frame (hold single frame for arbitrary duration)
  - [x] Persist freeze-frame clip model fields (enable/source/hold duration) with backward-compatible serialization and helper semantics
  - [x] Add timeline UI command (keyboard/context/toolbar) to create undoable freeze-frame clips with configurable hold duration
  - [x] Program Monitor freeze-frame playback: hold a single sampled source frame for resolved freeze duration (including transition/composite timing)
  - [x] Program Monitor freeze-frame seek reliability: force accurate, non-key-unit decoder seeks for single-frame freeze windows so held-frame preview does not black out
  - [x] Export freeze-frame parity: ffmpeg output now matches preview hold timing and treats freeze-frame video clips as silent (video-only)
- [x] Through edit detection (dotted lines for contiguous same-source cuts, join-back)
  - [x] Model-side detection for join-safe through-edit boundaries (same source, contiguous source/timeline ranges, compatible kind, transition-safe)
  - [x] Timeline dotted boundary indicators
  - [x] Join-back edit action
- [x] Right-click clip context menu now shows only currently actionable clip operations (hides unavailable actions)
- [x] Select clips forward/backward from playhead for bulk operations
- [x] Clip display options / adjustable per-track height, clip color labels

### Speed Ramps (per clip)
- [x] Constant speed change per clip (e.g. 0.5× slow-mo, 2× fast-forward) via GStreamer rate seek + ffmpeg `setpts`/`atempo` on export
- [x] Speed indicator badge on clip in timeline (yellow "2×" badge)
- [x] Persist speed data in FCPXML (`us:speed` attribute)
- [x] Reverse playback: per-clip "Reverse" toggle in Inspector applies to Program Monitor preview and export (`reverse`/`areverse`), timeline shows `◀` badge, and state persists via `us:reverse` FCPXML attribute
- [x] Variable speed ramps: multiple keyframed speed segments within a single clip
- [x] Optical flow / frame-blending for smooth slow-motion (ffmpeg `minterpolate` on export)

### Keyframe Animation
- [ ] Property keyframes with interpolation (position, scale, opacity, volume, pan over time within a clip; `Vec<Keyframe>` per property; linear/bezier/ease interpolation)
  - [x] Phase 1 foundation: linear keyframes for position/scale/opacity/volume/pan across model, Inspector, Program Monitor preview, MCP, FCPXML round-trip, and export
  - [x] Rotation/crop keyframe lane support: model/runtime + Program Monitor preview + export + MCP set/remove/list + FCPXML vendor/native rotation round-trip
  - [x] Native FCPXML keyframe interop: parser reads FCP `<param>/<keyframeAnimation>/<keyframe>` elements; writer emits them alongside vendor attrs for bidirectional exchange with Final Cut Pro
  - [x] Keyframe navigation (◀/▶ buttons, `Alt+Left`/`Alt+Right` shortcuts, timeline marker click-to-seek, ◆ indicator)
  - [x] Animation mode: "Record Keyframes" toggle (`Shift+K`) auto-creates keyframes on transform drags and slider changes
  - [x] Additional interpolation modes: EaseIn, EaseOut, EaseInOut with cubic bezier evaluation (preview), quadratic FFmpeg expressions (export), FCPXML `interp` attribute round-trip, Inspector dropdown, and MCP `interpolation` parameter
- [x] Curve editor / dopesheet UI (visual editor for keyframe timing and bezier handles)
  - [x] Phase 1: Dopesheet panel appears as a dedicated panel beneath the timeline tracks (with show/hide control on the track-management bar), includes per-lane visibility toggles, keyframe point selection (including additive/range multi-select), drag-to-retime, add/remove controls, interpolation apply control, value-curve overlays, keyboard delete/nudge controls, time zoom/pan controls, and full undo/redo integration.
  - [x] Phase 2: Bezier-handle curve editing for per-segment shape/tangent authoring.
    - [x] Phase 2a: selected keyframe segments now show Bezier handles; dragging a handle updates segment easing (snapped to nearest preset interpolation mode) with undo/redo integration.
    - [x] Phase 2b: continuous custom tangent values (non-preset Bezier control points) across preview/export/FCPXML/MCP paths.
      - [x] Phase 2b.1: dopesheet handle drags now store exact per-segment Bezier controls on keyframes and preview/runtime evaluation uses those controls.
      - [x] Phase 2b.2: FCPXML/MCP parity for custom controls (export parity now uses piecewise cubic-bezier approximation from stored controls).
        - [x] Phase 2b.2a: MCP `set_clip_keyframe` supports optional `bezier_controls` and `list_clips` exposes stored custom controls in keyframe arrays.
        - [x] Phase 2b.2b: Native FCPXML representation/parity for custom controls beyond vendor attrs (`curve="smooth"` + `interp` native keyframe metadata import/export mapping).

### Program Monitor
- [x] Program Monitor panel showing assembled timeline playback
  - Dedicated `ProgramPlayer` advances clip-by-clip from the project model
  - Play/Stop transport controls; timecode display
  - Timeline seek (click ruler) also seeks the program monitor
  - Clips reload automatically on every project change
  - Project replacement resets cached monitor output so empty projects do not show stale prior frames
- [x] Program-monitor playback priority mode in Preferences (`Smooth` / `Balanced` / `Accurate`)
- [x] Docked Program Monitor and scopes are resizable via draggable splitter (position persisted; pane collapses fully when scopes are hidden)
- [ ] Detachable Program Monitor window (pop-out preview)
  - [x] Pop out Program Monitor into a separate top-level window for dual-display workflows
  - [x] Keep transport controls/timecode/playhead fully synchronized between docked + popped-out monitor
  - [x] Persist monitor window geometry and last docked/popped state across sessions
- [ ] Preview rendering performance pass
  - [x] Build a compositor-based preview pipeline (`compositor` + layered video tracks) so B-roll/overlays render in preview without clip switching — see Picture-in-Picture section under Video Transform
  - [x] Run decode + waveform/thumbnail extraction on background workers with bounded queues and cancellation to keep GTK main thread responsive
  - [x] Move media import probing (duration + audio-only detection) to background threads via `MediaProbeCache`
  - [x] Move FCPXML project open (file I/O + XML parsing) to background thread with polling timer
  - [x] Move MCP `open_fcpxml` read/parse path off the GTK main thread and trim parser attribute-allocation overhead
  - [x] Reduce timeline thumbnail/waveform warm-up spikes via lower extraction concurrency and lighter thumbnail tile density
  - [x] Add short frame cache around playhead (previous/current/next frames) to reduce stutter on scrubbing and pause/seek
    - [x] Frame-boundary seek deduplication: quantize paused scrub positions to frame boundaries and skip redundant pipeline work for same-frame seeks
    - [x] Add bounded previous/current/next paused-frame cache keyed by frame position + render signature, with robust invalidation on project/render-setting changes
    - [x] Use short-frame cache hits to tighten paused in-place seek settle budgets, reducing scrub blocking while preserving accurate decoder seeks
   - [x] Introduce proxy preview mode (quarter/half resolution decode, full-res export) for large media
   - [x] Managed local proxy cache root (`$XDG_CACHE_HOME/ultimateslice/proxies`, fallback `/tmp/ultimateslice/proxies`) with fallback to alongside-media `UltimateSlice.cache` when local-cache transcodes fail
    - [x] Managed proxy cache lifecycle cleanup (startup stale prune for ownership-index entries older than 24h, plus project unload/app-close cleanup of managed cache files)
    - [x] Eager near-playhead proxy priming during project reload (capped, proximity-ordered source requests before first program-player rebuild)
    - [x] Proxy readiness hardening: incomplete/unprobeable proxy files are treated as invalid for playback fallback, and proxy outputs are promoted atomically only after successful completion
    - [x] Preserve full-frame fit at reduced preview quality (`Half` / `Quarter`) so the monitor downscales the composed frame instead of cropping to the top-left region
   - [x] Apply preview quality divisor to Program Monitor processing resolution (slot effects/compositor), reducing heavy-overlap playback cost when `Half`/`Quarter` preview is selected
    - [x] Add adaptive `Auto` preview quality mode that derives effective quality from current Program Monitor canvas size while preserving manual `Full/Half/Quarter`
    - [x] Respect strict Off proxy mode during heavy overlap (no automatic proxy-enable assist)
      - [x] Ensure paused timeline seek in compositor preview re-prerolls after decoder seek so Program Monitor/transform overlay frame refresh remains reliable while scrubbing
       - [x] Use accurate decoder seeks during playback boundary rebuilds (2→3 / 3→2 active-track transitions) so long-GOP proxies do not snap B-roll back to an earlier keyframe
       - [x] Reduce playback boundary handoff blocking by removing redundant paused-transition/state checks and shortening playback-path preroll waits for 3+ tracks
         - [x] Stabilize paused scrub rebuild ordering so active decoder branches are added before paused preroll/seek, preventing persistent black preview frames after playhead moves
         - [x] Keep project-open seek path off `pipeline.set_state(Ready)` hot spots (`load_clips()` stays paused and `rebuild_pipeline_at()` uses `start_time` reset instead of Ready) to avoid intermittent futex deadlocks when seeking immediately after open
         - [x] Reduce paused seek rebuild overhead by caching per-path audio probe results, applying decoder thread caps in paused rebuilds, and skipping the second paused reseek pass when first-pass link/arrival checks are already satisfied
          - [x] Stage reload as deferred load→seek phases with ticket coalescing, and cap paused 3+ track settle waits for responsiveness so UI remains interactive during project open + immediate seek
          - [x] Suppress playback auto-resume for full project replacement actions (new/open/recent and MCP project open/create) so project load does not start playback unexpectedly
          - [x] Reduce overlap-transition playback churn by keeping audio probe cache warm across proxy-path refreshes and adding hysteresis/min-dwell to auto proxy assist (less flapping around 2↔3 track boundaries)
          - [x] Add minimum-dwell switching for Auto preview quality divisor while playing to reduce caps renegotiation thrash at transition boundaries
          - [x] Enable audio-master drop-late preview policy during 3+ track playback overlap (leaky display queue + sink QoS/max-lateness) so displayed frames stay closer to audio clock under decode pressure
          - [x] Apply adaptive per-slot queue drop-late policy during heavy-overlap playback to reduce compositor-branch backpressure at handoff
          - [x] Re-sync/pause audio-only preview pipeline around video boundary rebuilds so transition stalls do not let audio run ahead and end early versus video
           - [x] Add short look-ahead boundary prewarm (next active clip-set probe/path warm-up) to reduce synchronous work at transition handoff
           - [x] Optional background prerender mode: render upcoming complex overlap windows (3+ active video tracks) to temporary disk clips and use them at boundary rebuilds when available
           - [x] Background prerender boundary correctness hardening: track prerender-active clip sets for boundary transitions and normalize prerender segment timestamps to avoid freeze/black handoff regressions
           - [x] Background prerender safety fallback: when a prerender video slot fails to link at boundary rebuild, immediately rebuild with normal live slots; cache keys are versioned with timeline/track identity to prevent stale segment reuse
           - [x] Background prerender link-race guard: allow a short post-preroll link grace for prerender slots, then force live fallback + segment invalidation when still unlinked to prevent unstable playback states
           - [x] Background prerender priority over realtime boundary path for 3+ overlaps: when both settings are enabled, boundary handling now chooses prerender-capable rebuilds so prerender clips are consumed during full playthrough
           - [x] Background prerender scheduling bounded during playback: queue by upcoming boundaries only (not moving playhead ticks) to avoid excessive in-flight segment churn
           - [x] Background prerender slot sizing parity: prerender decode branch now scales to current preview-processing dimensions before compositor to avoid top-left crop artifacts at reduced preview quality
           - [x] Background prerender A/V prototype: prerender segments now include mixed audio and prerender playback uses a single prerender decoder branch for both video and audio
           - [x] Background prerender segment window now spans full overlap to next boundary (no fixed 4s truncation), preventing mid-overlap black tails when prerender is active
           - [x] Background prerender render dimensions now follow active proxy scale when proxy mode is enabled (Half/Quarter), reducing prerender decode/render load
           - [x] Correctness-first 3+ track boundary settle: relax playback arrival wait cap so compositor frames arrive before boundary resume (avoids audio-only/black-video handoffs), and add prerender queued/ready/failed telemetry logs
           - [x] Prerender promotion correctness: when prerender becomes ready mid-overlap, force full rebuild (bypass continue-decoder short-circuit) so prerender segment is actually consumed; add explicit unavailable/promote/used diagnostics
            - [x] Idle prerender warmup + shared status bar: when background prerender is enabled, schedule nearby prerender jobs while paused/stopped and surface progress in the existing proxy generation status bar
            - [x] Status-bar quick toggle for background prerender next to Track Audio Levels
             - [x] Status-bar Background Render toggle icon state cues (process-stop/system-run for off/on)
             - [x] Prerender-exit warmup: while a prerender slot is active, prewarm the immediate post-prerender boundary clip resources to reduce handoff stalls
             - [x] Prewarm incoming boundary clip decoder/effects resources ahead of handoff (lightweight Ready/Null warm-up)
             - [x] Adaptive transition prerender prewarm horizon: in Smooth mode, scan one extra upcoming boundary and farther lookahead for prerender scheduling, while limiting to baseline depth when prerender queue is already busy
             - [x] Transition prerender telemetry counters: log per-transition prerender hit/miss outcomes to guide future prewarm/priority tuning
             - [x] Transition prerender hit-rate auto-tune: when accumulated transition prerender hit rate is low (after minimum samples), temporarily expand Smooth-mode prewarm depth/lookahead while keeping busy-queue guardrails
             - [x] Transition prerender overlap padding: add small frame padding around overlap boundaries (with incoming pre-overlap `tpad` hold) to reduce edge handoff misses at transition entry/exit
             - [x] Transition-priority prewarm scheduling: when Smooth-mode queue budget is tight, prioritize worst hit-rate transition boundaries first so limited prewarm slots target highest-risk misses
             - [x] Transition overlap audio-padding parity: delay incoming transition audio during prerender pre-padding so overlap audio starts at boundary (no early incoming bleed)
             - [x] Distance-aware transition prewarm priority: blend transition risk and boundary proximity so queue-constrained prewarm still favors near-term boundaries while targeting high miss-risk transitions
             - [x] Recency-weighted transition metrics: apply periodic decay to prerender hit/miss counters so scheduling reacts to current playback behavior
             - [x] Priority-aware prerender queue admission: cap in-flight prerender queue depth and allow limited overflow only for meaningfully higher-priority requests
             - [x] Prerender ready-cache pruning: bound cached ready segments and evict far-from-playhead entries first (while keeping active key) to reduce stale-cache churn
             - [x] Prerender cache hit telemetry: track cache hit/miss counters and expose hit-rate in performance snapshot/logging for tighter tuning feedback
             - [x] Prerender LUT guard for proxy-backed inputs: skip LUT re-application in prerender when source media is already proxy-backed/LUT-baked
             - [x] Track meter continuity during prerender playback: map prerender level telemetry to active prerender tracks so per-track audio monitors stay visible
           - [x] Adaptive rebuild wait budgets: scale preroll/arrival/link waits dynamically from a ring buffer of recent rebuild durations (tighter after fast rebuilds, conservative after slow ones)
          - [x] Audio pipeline continuity: skip audio_pipeline pause/resync at boundaries where only video tracks change
           - [x] Phase-level rebuild telemetry: per-phase timestamps in rebuild_pipeline_at
           - [x] Debounce duplicate playback boundary rebuild attempts (same desired clip set within ~120ms) to reduce transient rebuild thrash
           - [x] Tighter post-seek budgets after prewarm: reduce arrival wait when sidecar proved file decodable
           - [x] Skip preroll for already-settled decoders: avoid redundant blocking in wait_for_paused_preroll
          - [ ] Remove-only incremental boundary path — BLOCKED: same GstVideoAggregator limitation as add-only; aggregator timing/segment state goes stale after pad removal without compositor.seek_simple reset, causing ≤1 frame/sec on retained decoders
          - [ ] Add-only incremental boundary path — BLOCKED: GstVideoAggregator requires compositor.seek_simple to reset aggregation state, which propagates upstream corrupting retained decoders. Future approach: gst_pad_set_offset() for running-time alignment
           - [x] Pre-preroll incoming boundary clips before switch so decoder/link work is shifted earlier than the handoff tick
            - [x] Occlusion-based video decode skip: clips fully hidden behind an opaque full-frame overlay build audio-only slots (decoder with audio caps only), skipping video decode/effects/compositor
             - [x] Occlusion audio continuity fallback: if an occluded clip's audio-only slot cannot be created, preview falls back to a full slot so audio is preserved
             - [x] Stricter occlusion classification for correctness: only centered/unrotated/unflipped/uncropped opaque full-frame overlays trigger occlusion skip, reducing false-positive audio muting
             - [x] Correctness guard for multitrack audio: temporarily disable occlusion audio-only substitution during active rebuilds to preserve reliable mixed audio
              - [x] Boundary audio-drop guard: when overlap rebuilds encounter delayed video-pad linking, keep already-linked slot audio active (do not EOS the audio pad solely because video linking is late)
              - [x] Boundary pre-link EOS deferral for active handoffs: when playback is already running across a boundary, avoid forcing early pre-link EOS on newly added overlap slots so late pad-added links can settle before post-seek arrival checks
              - [x] Audiomixer flush parity: flush the audiomixer alongside the compositor during boundary rebuilds so their output running-times stay in sync, preventing audio buffer late-drop after a video-path flush
              - [x] Continuing decoders fast path: reuse existing decoder slots at boundary crossings when adjacent clips share the same source file, avoiding teardown/rebuild overhead (~60-75% boundary latency reduction for same-source transitions)
            - [x] Fix paused-seek preview: scrubbing within the same clip now seeks decoders in-place (no pipeline teardown/rebuild), eliminating the black-screen and first-frame flash caused by the pipeline going through `Ready` state and decoders prerolling at position 0
    - [x] Regenerate proxies when proxy size changes in Preferences (was reusing old-resolution file)
   - [x] LUT-baked proxies: clip proxy re-generated when a LUT is assigned/cleared, enabling grade preview
   - [x] Proxy shutdown cleanup policy: always clean managed local/tmp proxies on unload/close; preserve tracked `UltimateSlice.cache` proxies only when Proxy mode is enabled (clean sidecar proxies too when disabled)
   - [x] Enabled-mode sidecar proxy mirroring: when Proxy mode is enabled, local proxy transcodes are mirrored to alongside-media `UltimateSlice.cache` as well
   - [x] Preview LUTs preference: when Proxy mode is Off, generate/use project-resolution LUT-baked preview media for LUT-assigned clips
   - [x] Export/proxy progress percentage now uses bitrate×duration size estimates with ffmpeg `total_size` tracking, capped below 100% until ffmpeg completion
   - [x] Parallel proxy transcoding: 4 worker threads process ffmpeg transcodes concurrently instead of sequentially
  - [x] Optimized effects pipeline: single-pass `videoconvertscale` for decode→RGBA downscale, early downscale before effects, conditional element creation for no-op effects, leaky scope queue to prevent display backpressure
  - [x] Throttle UI redraws to monitor refresh rate and coalesce timeline invalidations (avoid redundant `queue_draw`)
  - [x] ~~Reuse per-clip filter bins/elements across seeks where possible instead of rebuilding pipeline state on every handoff~~ *(superseded by compositor rewrite — full rebuild at clip boundaries)*
  - [x] ~~Reduce boundary stutter with pre-emptive clip handoff and non-blocking switch path during active playback~~ *(superseded by compositor rewrite)*
  - [x] ~~Reduce black flash on track switches by avoiding `Ready` sink reset during active source handoff~~ *(superseded by compositor rewrite — pipeline goes through Ready to reset running-time)*
  - [x] ~~Fix preview halting with 3+ video tracks — ensure preroll before seek during mid-playback clip switches, plus timeline-position safety check~~ *(superseded by compositor rewrite — wall-clock position tracking)*

### Audio
- [x] Audio track clip display with waveform (see Timeline Improvements above)
- [x] Volume / pan controls per clip in the inspector (volume slider now dB-based: `-100 dB` to `+12 dB`, mapped to linear gain for playback/export, persisted in FCPXML)
- [x] Basic audio mixing (level meters)
  - [x] Program Monitor master stereo VU meter (L/R)
  - [x] Per-track stereo meters in timeline track labels (timeline track order)
  - [x] Status-bar eye toggle to show/hide track audio levels
- [x] Audio crossfades (automatic crossfade at audio edit points, equal-power or linear, adjustable duration)
  - [x] Persisted crossfade preferences (enabled, curve, duration) in UI state and Preferences UI, with MCP read/write support via `get_preferences` and `set_crossfade_settings`
  - [x] Program Monitor preview crossfades at adjacent same-track audio edit points, honoring preference curve/duration with short-clip-safe clamping
  - [x] Export-time automatic crossfades at adjacent same-track audio edit points (audio tracks + eligible embedded clip audio), honoring preference curve/duration with short-clip-safe clamping

### Color & Effects
- [x] Basic color correction (brightness / contrast / saturation) via GStreamer `videobalance`
- [x] Extended color grading — exposure, black point, highlights/midtones/shadows warmth & tint; Inspector sliders, FCPXML round-trip (FCP `filter-video` "Color Adjustments" import/export), MCP `set_clip_color` support; preview/export parity improved by reusing calibrated preview mapping in export with FFmpeg frei0r bridge (`coloradj_RGB`, `three_point_balance`) and native-filter fallback
- [x] Shadows and Highlights — imported from FCP `<filter-video>` params, Inspector sliders, MCP support
- [x] Denoise filter per clip (GStreamer `gaussianblur` positive sigma; ffmpeg `hqdn3d` on export)
- [x] Sharpness / unsharp-mask per clip (GStreamer `gaussianblur` negative sigma; ffmpeg `unsharp` on export)
- [x] LUT import / apply
- [x] Apply multiple LUTs to a clip (multi-LUT UI in inspector with numbered list, add/clear all, copy/paste support)
- [x] Color scopes (waveform, vectorscope, RGB parade, histogram)
- [ ] Preview/Export color parity improvements
  - [x] GStreamer real-time LUT element — apply LUTs in the GStreamer preview pipeline via CPU-based trilinear 3D LUT pad probe at preview resolution, with parsed-LUT caching and automatic double-apply prevention when source is already LUT-baked
  - [ ] Prerender keyframe interpolation — support brightness/contrast/saturation/temperature/tint keyframes in the prerender pipeline (currently only static values are applied; animated color adjustments are not visible in proxy mode)
  - [ ] Configurable prerender quality — expose CRF / encoding preset in Preferences (currently CRF 20 veryfast) to let users trade cache size and prerender speed for higher color fidelity
  - [ ] Preview/export comparison overlay — a split-screen or A/B toggle in the Program Monitor that shows the prerender frame beside a single-frame export render, allowing direct visual parity inspection without a full export cycle
- [x] Advanced color grading
  - [x] Match Clip Colors — automatic Reinhard-style color transfer: analyzes source and reference clip frames in CIE L\*a\*b\* space to compute slider adjustments (brightness, contrast, saturation, temperature, tint) and optional 17³ 3D LUT for fine-grained matching. Inspector "Match Color…" button, `Ctrl+Alt+M` shortcut, and `match_clip_colors` MCP tool with full undo support.
- [ ] Color management pipeline via OpenColorIO (OCIO)
  - [ ] Rust FFI bindings for OpenColorIO C++ library (bindgen wrapper against OCIO C API; build.rs pkg-config detection + static/dynamic linking)
  - [ ] OCIO config loading (ACES 2.0, Rec.709, sRGB built-in configs; user-supplied config file path in Preferences)
  - [ ] Display transform in Program Monitor (source colorspace → display colorspace via OCIO processor; GStreamer element or per-frame CPU path)
  - [ ] GPU-accelerated color transforms (OCIO GPU shader extraction applied via OpenGL/Vulkan in preview pipeline)
  - [ ] Per-clip input colorspace override (Inspector dropdown: Auto-detect, sRGB, Rec.709, Rec.2020, S-Log3, LogC, Protune, etc.)
  - [ ] Export colorspace selection (output color profile in export dialog; OCIO baked into ffmpeg filter or pre-transform frames)
  - [ ] Working colorspace preference (scene-linear, ACEScg, Rec.709 — controls internal processing space)
- [ ] HDR workflow via libplacebo
  - [ ] libplacebo Vulkan integration for GPU-accelerated video rendering in Program Monitor
  - [ ] HDR tone mapping (PQ/HLG → SDR) using libplacebo algorithms (hable, bt2446a, st2094-40) for accurate SDR preview of HDR sources
  - [ ] Inverse tone mapping (SDR → HDR) for HDR display output and export
  - [ ] High-quality upscaling/downscaling (libplacebo polar/orthogonal scalers as alternative to GStreamer `videoscale`)
  - [ ] HDR export metadata (PQ/HLG transfer characteristics, mastering display color volume, MaxCLL/MaxFALL)
  - [ ] HDR passthrough mode for native HDR display output
- [x] Frei0r video effects plugin support
  - [x] Load and enumerate installed Frei0r plugins via GStreamer `frei0r` element (auto-discover from standard paths)
  - [x] Effects browser UI listing available Frei0r filters with categories and search
  - [x] Per-clip Frei0r effect application with parameter controls in Inspector
  - [x] Effect stacking (multiple Frei0r filters per clip, reorderable)
  - [x] GStreamer preview pipeline integration with live parameter updates
  - [x] FFmpeg export pipeline integration (frei0r filter_complex chain)
  - [x] FCPXML round-trip via `us:frei0r-effects` vendor attribute (JSON quotes escaped to `&quot;` on write; backward-compatible sanitizer for older files)
  - [x] MCP tools: `list_frei0r_plugins`, `add_clip_frei0r_effect`, `remove_clip_frei0r_effect`, `set_clip_frei0r_effect_params`, `reorder_clip_frei0r_effects`, `list_clip_frei0r_effects`
  - [x] Five undo commands (add, remove, reorder, set params, toggle)
  - [x] Graphical curve editor for curves plugin — 240×240 DrawingArea with Catmull-Rom spline, 2–5 draggable control points, channel selector (R/G/B/RGB/Luma), double-click to add/remove points
  - [x] Graphical levels editor for levels plugin — transfer function visualization (240×80), input/output black/white sliders, gamma slider (0.1–4.0 mapped from frei0r 0–1), channel selector (R/G/B/Luma)
- [x] Blur as creative effect (controllable radius for censoring, depth-of-field, background defocus) — Inspector slider (0.0–1.0), GStreamer gaussianblur preview, FFmpeg boxblur export, keyframe animation, FCPXML persistence, MCP access, color grade copy/paste
- [x] Titles / text overlay (`textoverlay`)
- [x] Titles Browser with 9 built-in templates (Standard, Cinematic, Informational categories)
- [x] Standalone `ClipKind::Title` clips — transparent/solid-color background, no source media required
- [x] Extended title styling — font picker, color picker, outline stroke, drop shadow, background box, secondary text
- [x] Live title style editing — all 11 styling controls (font, color, outline, shadow, bg box) update preview in real-time
- [x] Debounced title reseek — compositor-only flush during title edits (avoids expensive all-slot decoder re-seek)
- [x] Velocity-adaptive scrub waits — 30ms arrival/pulse budgets during rapid scrubbing (2.6× faster)
- [x] Title clips in background prerender — lavfi color + drawtext source for title overlay clips
- [x] Frei0r effects in background prerender — applied in ffmpeg filter chain, hashed in signature
- [x] MCP tools: `add_title_clip`, `set_clip_title_style`
- [x] Transition effects (fade to black, wipe right, wipe left)
- [x] Transition preview matching — program monitor now previews cross-dissolve, fade-to-black, wipe-right, and wipe-left transitions using compositor alpha animation and videocrop, matching FFmpeg `xfade` export output

### Visual Effects
- [x] Chroma key (green/blue screen) — remove color range for transparency compositing, hue/tolerance/edge-softness controls; GStreamer `alpha` element in preview, ffmpeg `colorkey` filter in export; Inspector panel with enable toggle, green/blue/custom color presets, tolerance and edge-softness sliders
- [x] AI background removal — offline ONNX Runtime inference (MODNet segmentation model) produces alpha-channel VP9 WebM files; BgRemovalCache with 2-thread worker pool; preview and export use pre-processed result; Inspector toggle + threshold slider; MCP `set_clip_bg_removal` tool; FCPXML persistence
- [x] Video stabilization — analyze and compensate camera shake via libvidstab (two-pass workflow); Inspector enable/smoothing controls; export-time analysis + vidstabtransform + post-sharpening; proxy-baked preview when proxy mode enabled; FCPXML persistence; MCP `set_clip_stabilization` tool
- [x] Blend modes (Multiply, Screen, Overlay, Add, Difference, Soft Light, etc.)
- [x] Adjustment layers / adjustment tracks — a special clip (or dedicated track) whose filters and color corrections apply to all clips/tracks below it in the composite stack; the adjustment only affects the region covered by the adjustment clip's bounding box (position, scale, crop) so effects can be scoped to a portion of the frame or a time range on the timeline
  - [x] Phase 1: Full-frame adjustment layers with `ClipKind::Adjustment`. Color grading (brightness, contrast, saturation) applied to composited output via permanent GStreamer videobalance element (real-time preview). Frei0r user effects, LUTs, temperature/tint, blur applied on export via time-gated FFmpeg filter chain. Purple hatched timeline rendering, inspector visibility, FCPXML round-trip, MCP tool, undo support, right-click context menu.
  - [x] Phase 1b: Background prerender support for adjustment layer frei0r effects — when Background Render is enabled, prerender the adjustment frei0r/LUT/blur effects into temporary clips so the Program Monitor shows the full effect chain without real-time GStreamer topology changes
  - [ ] Phase 2: Bounding box scoping (position, scale, crop constraints on adjustment layer effect region)
- [ ] Shape / freeform masking — rectangle, ellipse, bezier path masks with feathering for selective effects

### Video Transform (per clip)
- [x] Scale / resize clip (zoom in/out within frame) via GStreamer `videoscale` + `videobox`
- [x] Crop clip (left / right / top / bottom margins) via GStreamer `videocrop`
- [x] Rotate clip by arbitrary angle via Inspector dial/numeric control and GStreamer `rotate` preview path
- [x] Flip horizontal / flip vertical via GStreamer `videoflip`
- [x] Position offset (X / Y translation within the output frame) via GStreamer `videobox`
- [x] Transform edits (Scale/Position) now refresh immediately in Program Monitor preview/playback without stale black-bar framing
- [x] Program Monitor transform chain now stays active even when optional `gaussianblur` is unavailable (uses identity fallback)
- [x] Program Monitor zoom chain enforces square-pixel output (`pixel-aspect-ratio=1/1`) to prevent persistent display-aspect black bars on wide-source media
- [x] Persist transform settings in FCPXML (`us:crop-*`, `us:rotate`, `us:flip-h/v`, `us:scale`, `us:position-x/y` attributes)
- [x] Interactive transform overlay in program monitor — when a clip is selected, show drag handles on the preview frame so the user can:
  - **Move**: drag the frame to adjust Position X/Y
  - **Scale**: drag corner handles to zoom in/out
  - Overlay updates Inspector sliders in real time and vice-versa
  - Visual feedback: dark vignette outside canvas, yellow canvas border (shadow + accent + corner L-marks), white dashed clip bounding box (only when scale≠1 or pos≠0), blue-ringed corner handles, center dot, scale label
  - Canvas border is always drawn at the exact canvas/export boundary; clip bbox only shows when it differs from the canvas
- [x] Zoomable program monitor preview — zoom in/out to work on fine-grained transforms:
  - **–/+ buttons** in program monitor title bar; zoom levels: 25%, 50%, 75%, 100%, 150%, 200%, 300%, 400%
  - **Fit button** resets to 100% (video fills the monitor)
  - **Ctrl+Scroll** on the preview also adjusts zoom
  - Scrollbars appear automatically when zoomed > 100%; panning by scrolling shows content outside the canvas boundary
  - Transform overlay handles scale correctly at all zoom levels
- [ ] **Picture-in-Picture / layered video compositing** — when multiple video tracks have clips active at the same position and the upper track does not fully cover the canvas, the lower track should be visible in the uncovered areas:
  - [x] Program Monitor now composites the top active video clip over the nearest active lower track at the playhead, so uncovered regions from scale/position transforms reveal lower-track video.
  - [x] Per-clip opacity control (0.0–1.0) in Inspector and MCP (`set_clip_opacity`), persisted in FCPXML (`us:opacity`).
  - [x] Export overlays now preserve transparency for zoom-out padding and apply per-clip opacity in the ffmpeg overlay chain.
  - [x] Compositor-based preview pipeline using GStreamer `compositor` element to layer all active video tracks simultaneously (replaces the clip-switching approach for multi-track compositing)
  - [x] Upper tracks render on top; alpha from the per-clip scale/position transform (black borders become transparent so lower tracks show through)
  - [x] Lower tracks fill any canvas area not covered by upper tracks (true compositing, not just B-roll switching)
  - [x] Export pipeline updated similarly — all concurrent clips composited via ffmpeg `overlay` filter chain before final output
  - Inspector shows which track layer a clip is on; layer order controls composite z-order
  - [x] Per-clip opacity control so tracks can blend softly over each other
- [x] Crop handles in transform overlay — edge midpoint handles (top/bottom/left/right) to adjust crop_left/right/top/bottom directly in the preview
- [x] Rotation handle in transform overlay — drag the top-center handle to set clip rotation, synchronized with Inspector rotation controls
- [x] Shift-constrain while scaling — hold Shift during corner drag to lock aspect ratio
- [x] Keyboard nudge in transform overlay — arrow keys adjust position by 0.01 per press (0.1 with Shift); `+`/`-` adjust scale; activated when a clip is selected
- [x] Transform overlay drag now pauses playback at interaction start, so the Program Monitor frame stays fixed while editing (no background timeline advancement)
- [x] Support anamorphic desqueeze (1.33x, 1.5x, 1.8x, 2.0x desqueeze via Inspector and MCP; persists in FCPXML)

### Monitoring
- [x] Safe area overlays (title safe 80%, action safe 90%) — Program Monitor "Safe Areas" toggle with persisted state
- [ ] False color overlay — map luminance to color spectrum for exposure evaluation
- [ ] Zebra stripes — diagonal lines on areas exceeding configurable IRE threshold
- [ ] Focus peaking — highlight in-focus edges with colored overlay

### Project Management
- [x] Project save / load as FCPXML (wired to New/Open/Save buttons in toolbar)
- [x] Recent projects list
- [x] Auto-save (60s timer, writes to /tmp/ultimateslice-autosave.fcpxml when project is dirty)
- [ ] Proxy media generation and management
- [x] Auto-backup with versioned history (timestamped backups to `$XDG_DATA_HOME/ultimateslice/backups/`, per-project pruning, restore UI, configurable in Preferences, MCP `list_backups` tool)

### Media Management
- [x] Relink dialog — general-purpose UI to find and repoint all offline/missing media
- [ ] Bins / folders — hierarchical media browser organization for large projects
- [x] Offline / missing media indicators — visual badge on clips when source_path doesn't exist
- [ ] Consolidate / collect files — copy all referenced media into one directory for archival or transfer
- [ ] Metadata display & filtering — show resolution, codec, frame rate, duration, file size in media browser

### Canvas / Sequence Settings
- [x] Canvas size dialog (project resolution: 1080p, 4K, custom W×H)
- [x] Frame rate selector in project settings (23.976, 24, 25, 29.97, 30, 60 fps)
- [x] Aspect ratio presets (16:9, 4:3, 9:16 vertical, 1:1 square)
- [x] Persist canvas settings in FCPXML `<format>` element

### Export
- [x] Advanced export dialog (replace current single-button export)
  - Codec selection: H.264, H.265/HEVC, VP9, ProRes, AV1
  - Container selection: MP4, MOV, WebM, MKV
  - Output resolution presets with downscale support (4K → 1080p → 720p → custom)
  - Bitrate control: CRF / target bitrate mode
  - Audio codec: AAC, Opus, FLAC, PCM
  - Audio sample rate and channel layout (stereo / mono)
- [x] Export presets: save/load named configurations (e.g. "Twitter 720p", "Archive ProRes")
- [ ] ProRes / WebM / GIF export options
- [ ] Batch export / render queue (queue multiple export jobs to run sequentially)
- [x] Chapter markers in export (embed project markers as MP4/MKV chapter metadata via ffmpeg FFMETADATA)
- [x] Still frame export (GUI menu/button to export current Program Monitor frame as PNG/JPEG/PPM via toolbar Export dropdown)
- [ ] EDL export (CMX 3600) — for online editing, color grading handoff, broadcast
- [ ] AAF export — standard interchange for audio post-production (Pro Tools)
- [x] Export progress dialog with cancel (ProgressBar + status label)

### Polish
- [x] Keyboard shortcut reference overlay (? or / key opens a modal dialog)
- [x] Preferences dialog with categorized sections + hardware acceleration toggle wired to source preview playback
- [x] About dialog in Preferences (General page) with third-party crate/library credits and license notices
- [x] GTK renderer preference (Auto / Cairo / OpenGL / Vulkan) for low-memory devices
- [x] Launch-screen clarity polish (empty-state guidance, wider side panels, and cleaner toolbar/inspector hierarchy)
- [ ] Accessibility: keyboard navigation in all panels
- [x] Welcome window for choosing recent project or new one (Stack-based overlay with New/Open/Recent, crossfade transition to editor)
- [ ] Help documentation and tutorials
- [ ] Application icon and desktop integration (`.desktop` file)
- [ ] Customizable keyboard shortcuts (shortcut config file + preferences UI)
- [x] Timecode entry / go-to timecode (HH:MM:SS:FF to jump playhead)
- [x] Drag-and-drop from file manager (import by dragging files into media browser or timeline)
- [ ] Customizable workspace layouts (save/restore panel arrangements for different tasks)
- [ ] Named project snapshots (create named versions at milestones without separate files)

### Professional Workflow (The "Pro" Edge)
- [ ] Multicam editing (sync by audio or timecode)
  - [x] Audio cross-correlation sync for selected clips (FFT-based, background thread, MCP tool)
  - [x] Automatic timecode extraction from media files on import (GST_TAG_DATE_TIME)
- [x] Remove Silent Parts: right-click context menu action to detect and remove silent segments via ffmpeg `silencedetect`, with configurable threshold/duration and single-undo support
- [ ] Nested Timelines / Compound Clips
- [x] 3-Point and 4-Point editing (Insert/Overwrite from Source)
- [x] J/K/L scrubbing (shuttle control in program monitor; pitch-corrected audio via Rubberband is a planned enhancement)
- [x] Match Frame (`F` shortcut to find timeline clip in media library, load in source monitor, seek to matching frame; MCP `match_frame` tool)
- [ ] Proxy Workflow: One-click toggle between original and proxy media
- [ ] Keyword ranges + favorite/reject ratings in browser
- [ ] Auditions / clip versions (swap alternate takes nondestructively)
- [ ] Plugin architecture for third-party video effects (e.g. OFX/LV2 bridge)

### Advanced Audio
- [ ] Pitch-corrected audio time-stretching via Rubberband
  - [ ] Rubberband C library integration (FFI bindings or GStreamer `rubberband` element)
  - [ ] Pitch-preserved playback at variable speeds (J/K/L shuttle, constant speed changes)
  - [ ] Independent audio clip time-stretch without pitch shift (fit audio to duration)
  - [ ] Pitch-shift effect per clip (transpose audio without changing duration)
- [ ] Audio Roles (Dialogue, Effects, Music) with submixing
- [ ] Support for LV2 / LADSPA audio plugins
- [ ] Voiceover recording tool with countdown and punch-in
- [ ] Automatic Ducking (music volume lowers during dialogue)
- [x] Audio normalization and peak-matching (LUFS + peak modes via FFmpeg `ebur128`/`volumedetect`; Inspector button, MCP `normalize_clip_audio` tool, undo, measured loudness display + FCPXML persistence)
- [x] Built-in parametric EQ (3-band: Low/Mid/High with freq/gain/Q per band; GStreamer `equalizer-nbands` preview, FFmpeg `equalizer` export, gain keyframes, Inspector UI, FCPXML persistence, MCP `set_clip_eq` tool, undo)
- [x] Waveform sync (align external audio to camera reference audio by waveform analysis; "Sync & Replace Audio" context menu action links clips and mutes camera embedded audio; MCP `sync_clips_by_audio` with `replace_audio` flag)

### AI & Automation
- [ ] Custom background removal model — train/export a self-hosted segmentation model with secure distribution and in-app download (Preferences → Models); replace third-party MODNet dependency
- [ ] Speech-to-Text: Automatic subtitle generation and transcription
- [ ] AI Scene Cut Detection for long source files
- [ ] Smart Collections based on metadata (keywords, resolution, frame rate)
- [ ] Optical Flow slow-motion (AI frame interpolation)
- [ ] AI Music Generation (MusicGen / MusicGPT)
  - [ ] Phase 1 — Draw-region UX: draw a time-range box on an audio track to define a generated-audio region (reusable for silence/tone generation too)
  - [ ] Phase 2 — Local model backend: Preferences entry for local model path; candidate runtimes include the `musicgpt` Rust crate (native, no Python dependency), MusicGen-small ONNX via `ort`, or Python `audiocraft` subprocess; background generation with status-bar progress (same pattern as proxy transcoding)
  - [ ] Phase 3 — Prompt UI: popover on drawn region with text prompt input, optional reference audio, duration auto-calculated from region length; generated audio written as a WAV clip and placed in the region

### Script-to-Timeline (Create Project from Script & Clips)
- [ ] **Script import**: parse Final Draft (FDX) and Fountain screenplay files to extract scene headings, dialogue lines, and scene order
- [ ] **Speech-to-text transcription**: run STT (e.g. Whisper via `whisper-rs` or subprocess) on every imported clip in the background; produce a timestamped transcript per clip
- [ ] **Transcript-to-script alignment**: use fuzzy text matching (e.g. Smith-Waterman or token-level diff) to align each clip's transcript against the full script; score every clip against every scene and pick the best-fit placement
- [ ] **Dialogue-aware ordering**: clips are placed on the timeline in the order their best-matching script position falls, so the assembled cut follows the screenplay beat-for-beat
- [ ] **Sub-clip trimming from transcript**: if a clip's transcript spans multiple scenes, split the clip at the scene boundary timestamps provided by the STT alignment
- [ ] **Auto-assembly wizard**: multi-step dialog — (1) load script, (2) import clips folder, (3) background STT + alignment pass with progress bar, (4) review/confirm clip↔scene mapping, (5) generate timeline
- [ ] **Timeline population**: clips inserted in script scene order at correct timeline positions with scene-heading title overlays
- [ ] **Unmatched clips bin**: clips whose transcript could not be confidently aligned appear in a dedicated "Unassigned" library group for manual placement
- [ ] **Confidence indicators**: low-confidence matches shown with a warning badge on the clip in the wizard review step
- [ ] **Re-order by script**: right-click timeline command to re-run alignment and re-sequence existing clips against a newly loaded or updated script
- [ ] Persist script path, scene mapping, and transcript cache in FCPXML (`us:script-path`, `us:scene-id`, `us:transcript-cache` attributes)

### Performance & Integration
- [ ] Hardware-accelerated decoding/encoding (VA-API, NVENC)
- [ ] Background rendering for complex effect stacks
- [ ] OpenTimelineIO (OTIO) import/export
- [ ] Shared Project/Library support for collaborative editing

---

## Architecture Notes

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the codebase layout,
key data-flow decisions, and agent contribution guidelines.
