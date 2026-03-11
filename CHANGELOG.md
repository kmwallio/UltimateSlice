# Changelog

All notable project changes and progress should be recorded here.

## Unreleased

### Added
- **macOS build instructions**: Added Homebrew (`brew install`) dependency setup for GTK4, GStreamer, and FFmpeg in `README.md` and `docs/user/getting-started.md`, including the required `PKG_CONFIG_PATH` export for Apple Silicon and Intel Macs.

### Fixed
- **Program Monitor MP4 `not-negotiated` playback regressions**: Stabilized live program playback for keyframed clips by avoiding aggressive pre-preroll EOS injection on not-yet-linked rebuild slots, reducing not-negotiated recovery debounce for faster retry, and preferring compositor sink-pad zoom/position updates over per-frame zoom caps renegotiation that could trip `qtdemux` negotiation failures.
- **MCP stress-run preview warning churn**: Reduced repeated fallback/log noise during proxy-generation races by warning once per unresolved proxy key (instead of per-slot rebuild spam), retaining warn state across proxy path updates until a proxy resolves, and downgrading expected short-budget compositor-arrival timeouts to debug-level diagnostics.
- **Source Monitor in/out scrub preview accuracy**: Dragging the In/Out handles on the source scrubber now issues frame-accurate seeks during drag (and on drag end), so preview updates to the exact frame under the marker instead of keyframe-snapped positions.
- **MCP insert/overwrite seek round-trip overhead**: `insert_clip` and `overwrite_clip` now accept optional `timeline_pos_ns`, allowing edits at an explicit timeline position without requiring a separate `seek_playhead` call.
- **MCP media import repeated probing**: Import now uses a single Discoverer metadata probe pass for duration/audio flags instead of separate probe calls, reducing per-import overhead.
- **MCP edit burst UI rebuild overhead**: MCP command processing now debounces project-change refreshes on the GTK thread, coalescing rapid edit bursts into a single UI/program refresh instead of rebuilding after every individual command.
- **MCP command dispatch wakeup latency**: Replaced fixed-interval MCP command polling on the GTK thread with event-driven dispatch via `MainContext::invoke`, so command handling is wakeup-driven instead of waiting for the next poll tick.
- **MCP edit refresh overhead for safe timeline mutations**: MCP now uses a selective light-refresh mode for mutation commands that do not require proxy/background-removal re-resolution, avoiding unnecessary cache/request work while preserving full refresh for operations that do.
- **macOS crash during timeline editing (SIGSEGV in unpack_NV12)**: On macOS, Apple's VideoToolbox hardware decoder (`vtdec`) outputs IOSurface-backed NV12 frames. When GStreamer's `videoconvertscale` used its parallelized task runner to convert these frames, worker threads could read from an IOSurface that had already been released, causing `EXC_BAD_ACCESS` in `unpack_NV12`. Fixed by setting `n-threads=1` on all `videoconvertscale` elements (both explicitly created and those auto-added by `uridecodebin`) when running on macOS, forcing single-threaded conversion and eliminating the race.
- **macOS crash in media browser / source monitor (SIGSEGV in unpack_NV12)**: Same IOSurface race in the `playbin`-based source monitor pipeline. Fixed by setting `n-threads=1` on the `prescale` `videoconvertscale` element in the `video-filter` bin and connecting `deep-element-added` on `playbin` to intercept any additional `videoconvertscale` elements auto-created by its internal `decodebin`.
- **Proxy mode Off now behaves strictly for preview**: Source Monitor and Program Monitor preview paths no longer request or auto-enable proxy transcodes when `Proxy preview mode` is set to `Off`. Proxy generation remains active only when proxy mode is explicitly `Half Res`/`Quarter Res` (and separate Preview LUT media generation remains controlled by `Preview LUTs`).
- **Source Monitor proxy fallback parity**: Source Monitor now mirrors Program Monitor fallback behavior by requiring proxy files to be ready before loading them, continuing on original media until proxy readiness, and retrying once with the original URI if a proxy load/decode error occurs.
- **Source Monitor In/Out drag frame sync**: Dragging In/Out markers on the source scrubber now seeks preview continuously to the marker position so the displayed frame follows the marker being moved.
- **Source Monitor marker-drag stability**: In/Out marker drag seeks are now throttled with a guaranteed final seek on drag release, and marker drags temporarily pause/resume active playback, reducing demux/decoder churn that could trigger crashes during aggressive marker drags.
- **Source Monitor self-drop hardening (macOS crash fix)**: Dragging a source clip onto the source preview now becomes a no-op (explicit self-drop sink), and source playback pauses/resumes during source-clip drags to reduce demux churn during accidental self-drops.
- **Source Monitor scrubber-drag stability (macOS crash fix)**: Playhead drags on the source scrubber now pause/resume playback while dragging and always issue a final seek on drag release; on macOS, live drag seeks are deferred until release to avoid repeated demux churn that can crash `qtdemux`.
- **Source Monitor decoder hardening (macOS crash fix)**: In software-filtered Source Monitor mode, Apple VideoToolbox decoders (`vtdec`/`vtdec_hw`) are now down-ranked so playbin prefers non-VideoToolbox decode paths, reducing recurrent `qtdemux`/`vtdec` crash combinations during source scrubbing.
- **Source Monitor scrubber quiesce on macOS (crash guard)**: FLUSH seeks are now automatically deferred when the pipeline has an async state transition in progress (e.g. preroll after `load`), preventing a GStreamer `qtdemux` NULL-dereference race condition during scrubber and marker drags. On macOS, marker-drag seeks are also deferred to drag release for consistency.
- **AI background removal**: Offline background removal using ONNX Runtime inference (MODNet segmentation model). Enable per-clip in the Inspector's "Background Removal" section. Processes video frames through a neural network to produce an alpha-channel WebM (VP9 alpha) file. Both preview and export use the pre-processed result for exact visual match. Controlled via Inspector checkbox + threshold slider, FCPXML persistence, and MCP `set_clip_bg_removal` tool.
- **Export presets + MCP preset automation**: The Export dialog now supports named preset workflows (save-as, update, delete, and recall). Presets persist in local UI state and can also be automated via MCP (`list_export_presets`, `save_export_preset`, `delete_export_preset`, `export_with_preset`) for repeatable renders. New installs (and older UI-state files missing `export_presets`) now start with bundled defaults: **Web H.264 1080p**, **High Quality H.264 4K**, **Archive ProRes 4K**, and **WebM VP9 1080p**.
- **Go to timecode navigation**: Added Program Monitor **Go To** control and global `Ctrl+J` shortcut to jump the playhead via `HH:MM:SS:FF` (or `MM:SS:FF`) input, with frame-rate-aware parsing/formatting and project-end clamping feedback.

### Improved
- **Transition boundary playback smoothness**: Program Monitor boundary-change detection now compares active clip membership independent of slot ordering (avoids false boundary churn), and Smooth playback can automatically use the lightweight realtime boundary update path for overlap-exit transitions (removals-only) even when the realtime-preview toggle is off.
- **Transition overlap background prerendering**: Background prerender now considers transition-overlap boundaries (not only 3+ layer stacks), and prerender generation supports two-clip overlap transitions with FFmpeg `xfade` for supported transition kinds (`cross_dissolve`, `fade_to_black`, `wipe_right`, `wipe_left`). In Smooth playback mode, transition prerender remains eligible even when clips have phase-1 keyframes, prioritizing reduced stutter.
- **Transition playback drop-late tuning (Smooth mode)**: Late-frame dropping (`qos`/queue leaky policy) now activates during active 2-clip transition overlaps in Smooth mode (not just 3+ layer stacks), reducing transition stutter on constrained systems while preserving non-transition quality behavior.
- **Transition prerender exit handoff stability**: Realtime boundary fast-path updates are now skipped while synthetic prerender slots are active, avoiding extra boundary churn at overlap exit and reducing transition handoff stutter.
- **Adaptive transition prerender prewarm horizon**: Smooth-mode playback now scans one additional upcoming boundary for background prerender scheduling (while capping back to baseline when too many prerender jobs are already pending), extends transition lookahead distance, and scales prerender prewarm window size upward from recent rebuild telemetry when transitions are slow.
- **Transition prerender hit/miss telemetry**: Program Monitor now records per-transition prerender consumption outcomes (`cross_dissolve`, `fade_to_black`, `wipe_right`, `wipe_left`) and emits periodic hit-rate diagnostics in logs, making transition-prerender tuning data-driven.
- **Transition prerender auto-tune from live hit rate**: Smooth-mode prewarm scheduling now automatically expands boundary depth/lookahead when accumulated transition prerender hit rate falls below target (after a minimum sample size), while still respecting queue-pressure caps to avoid overscheduling.
- **Transition overlap prerender edge padding**: Transition prerender segments now include a small frame padding window around overlap boundaries; incoming transition input is held with `tpad` before the overlap start so pre-padding does not advance source timing. This reduces edge handoff misses and boundary stutter near transition entry/exit ticks.
- **Transition-priority prewarm scheduling under queue pressure**: When Smooth-mode prerender queue budget is constrained, upcoming transition boundaries are prioritized by worst observed prerender hit rate (with confidence weighting), so scarce prewarm slots are spent on transitions most likely to miss.
- **Transition overlap audio padding parity**: Transition prerender pre-padding now also delays the incoming transition audio branch (`adelay`) until overlap boundary time, preventing early incoming-audio bleed before the visual transition begins.
- **Distance-aware transition prewarm priority**: Queue-constrained Smooth-mode prewarm now combines transition risk score with boundary proximity bonus, reducing far-ahead starvation of near-term boundaries while still favoring poor-hit transitions.
- **Recency-weighted transition prerender metrics**: Transition hit/miss counters now apply periodic decay, so prewarm tuning and priority decisions track recent playback behavior instead of becoming anchored to stale historical samples.
- **Priority-aware prerender queue admission**: Background prerender now caps in-flight queue depth and only permits limited overflow for materially higher-priority requests, reducing low-value queue churn during heavy overlap playback.
- **Prerender ready-cache pruning**: Ready prerender segment cache is now bounded and evicts far-from-playhead segments first (while preserving any currently active prerender key), reducing cache bloat and stale segment churn.
- **Prerender cache hit/miss telemetry**: Program Monitor now records prerender segment cache lookup hit/miss counters with periodic log summaries and exposes hit-rate fields in MCP performance snapshots for data-driven tuning.
- **Proxy shutdown cleanup policy**: On project unload/app close, managed local cache (`$XDG_CACHE_HOME`/`/tmp`) is cleaned regardless of mode; when Proxy mode is enabled, only alongside-media `UltimateSlice.cache` proxies are preserved, and when disabled those sidecar proxies are cleaned too.
- **Enabled-mode sidecar proxy mirroring**: When Proxy mode is enabled, successful local proxy transcodes are now also mirrored into alongside-media `UltimateSlice.cache`, so sidecar caches are pre-populated for reuse in addition to managed local cache.
- **Prerender LUT double-apply guard with proxy sources**: Background prerender now tracks whether each input source is proxy-backed and skips LUT re-application for proxy inputs, avoiding double LUT grading when proxy media is already LUT-baked.
- **Per-track meter continuity during prerender playback**: When a synthetic prerender slot is active, per-track audio meters remain populated by mapping prerender audio-level telemetry to the active prerender track set, instead of dropping track-level monitor activity.
- **MCP performance snapshot endpoint**: Added `get_performance_snapshot` tool for automation/profiling scripts. It returns compact Program Monitor metrics including prerender queue state, active prerender key, recent rebuild timings (latest/p50/p75), and transition prerender hit/miss breakdowns with hit rates.
- **MCP batched automation calls**: Added `batch_call_tools` MCP endpoint to execute multiple tool invocations in-order within a single request and return per-call results; now also supports optional `stop_on_error` to halt remaining calls after the first failure and `include_timing` to emit per-call/batch elapsed milliseconds for profiling.
- **MCP batch internal dispatch efficiency**: `batch_call_tools` sub-calls now execute through a direct payload dispatch path instead of nested JSON-RPC response/text re-parsing, reducing internal batch overhead while preserving response compatibility.
- **MCP batch repeated-read efficiency**: `batch_call_tools` now reuses repeated read-only tool results within a batch and clears that cache on mutating calls, reducing duplicate dispatch work while preserving mutation correctness.
- **MCP compact listing mode for automation loops**: `list_tracks` and `list_clips` now accept optional `compact: true` to return smaller payloads for high-frequency stress/profiling scripts.
- **MCP cross-request read efficiency**: Added a short-lived top-level MCP read cache for repeated `get_project`/`list_tracks`/`list_clips` calls across requests in the same session, with automatic cache clear on mutating tool calls to preserve correctness. `batch_call_tools` now also consults this session cache before dispatching those reads.
- **Timeline keyframe marker visibility**: Keyframe ticks on clip bodies are now easier to see (larger, higher-contrast markers with outlines), and timeline marker hit-testing/rendering now includes the pan keyframe lane in addition to scale/opacity/position/volume.
- **Timeline keyframe discoverability**: Clips with phase-1 keyframes now show a `KF <count>` badge, a `◆` label prefix, and stronger in-clip keyframe guides/diamonds, making imported keyframes much easier to spot at a glance.
- **Inspector keyframe control parity for transform lanes**: Rotation and all crop controls (`crop_left/right/top/bottom`) now include explicit **Set/Remove Keyframe** buttons, matching existing transform/audio keyframe UX and making all currently keyframe-capable inspector properties directly manageable from the Inspector.
- **Timeline keyframe hover tooltips**: Hovering a keyframe marker now shows a tooltip with the clip name, keyframe time, and the full list of properties impacted at that moment. Marker lanes now cover all current phase-1 properties, including `rotate` and `crop_*`.
- **Timeline keyframe lane editing controls**: Keyframe columns on clip bodies are now directly editable in the timeline: drag selected keyframe columns to retime, `Delete`/`Backspace` removes selected keyframe columns, `Alt+Drag` performs keyframe box-select on a clip, and interpolation can be set from the timeline with `1/2/3/4` (Linear/Ease In/Ease Out/Ease In/Out).
- **Variable speed-ramping foundation**: Added `speed_keyframes` to clip/runtime models, exposed `speed` in MCP keyframe schemas plus `speed_keyframes` in `list_clips`, integrated keyframed speed into runtime source-position mapping, and added prototype keyframed export timing via dynamic `setpts` expressions.
- **Rotation/crop keyframe runtime + MCP coverage**: Added phase-1 keyframe lanes for `rotate` and `crop_left/right/top/bottom` in the clip model and Program Monitor runtime. Transform overlay + Inspector playhead-follow paths now evaluate those keyframes at the current timeline position, `list_clips` exposes the new keyframe arrays, and MCP `set_clip_keyframe` / `remove_clip_keyframe` now accept `rotate` and all four crop properties.
- **Core color keyframe runtime + MCP coverage**: Added phase-1 keyframe lanes for `brightness`, `contrast`, `saturation`, `temperature`, and `tint` in `Clip`/`ProgramClip`, Program Monitor runtime now evaluates and applies these values per timeline position, and MCP `set_clip_keyframe`/`remove_clip_keyframe` plus `list_clips` now expose the new color keyframe lanes.
- **Core color keyframe export + FCPXML parity**: FFmpeg export now evaluates keyframed `brightness`/`contrast`/`saturation`/`temperature`/`tint` per frame (dynamic `eq`, `colortemperature`, and `colorbalance` expressions), and FCPXML now round-trips these color keyframes via vendor attrs (`us:*-keyframes`) for lossless import/export.
- **Rotation/crop keyframe export + FCPXML parity**: FFmpeg export now treats rotate/crop lanes as transform keyframes and applies animated rotation plus animated crop masking in the keyframed path. FCPXML now round-trips `rotate` and `crop_*` keyframe lanes via vendor attrs (`us:rotate-keyframes`, `us:crop-*-keyframes`) and also parses/writes native `adjust-transform` rotation keyframes.
- **Audio pan keyframes end-to-end**: Added Inspector pan keyframe controls (set/remove), audio keyframe navigation now covers both volume and pan, animation mode auto-keys pan slider moves, MCP `set_clip_keyframe`/`remove_clip_keyframe` now accept `pan`, FCPXML now round-trips pan keyframes via both native `<adjust-panner>` keyframes and `us:pan-keyframes` vendor attrs, and export now applies static/keyframed pan automation.
- **Bezier / ease keyframe interpolation**: Keyframes now support **Ease In**, **Ease Out**, and **Ease In/Out** interpolation modes in addition to Linear. Inspector dropdown selects the mode for new keyframes; "Set Keyframe" buttons, animation mode auto-keyframes, and MCP `set_clip_keyframe` (optional `interpolation` param) all respect the selection. FCPXML `interp` attribute is parsed on import (`easeIn`/`easeOut`/`ease`) and emitted on export. FFmpeg export uses quadratic easing expressions. Preview uses cubic bezier evaluation matching CSS standard curves.
- **Keyframe navigation and animation mode**: Inspector now includes ◀ Prev / Next ▶ keyframe navigation buttons and a ◆ Keyframe indicator that shows when the playhead is on a keyframe. `Alt+Left`/`Alt+Right` keyboard shortcuts jump between keyframes across all properties. Clicking a keyframe marker tick on the timeline selects the clip and seeks the playhead to that keyframe. New "⏺ Record Keyframes" toggle (`Shift+K`) enables animation mode where transform overlay drags and inspector slider changes automatically create/update keyframes at the current playhead position.
- **Native FCPXML keyframe interop with Final Cut Pro**: The FCPXML parser now reads native `<param>/<keyframeAnimation>/<keyframe>` elements inside `<adjust-transform>`, `<adjust-blend>`/`<adjust-compositing>`, and `<adjust-volume>`, converting FCP position (frame-height-percentage 2D points), scale (X Y pairs), opacity, and volume (dB) keyframes to the internal model. The writer emits these same native elements when keyframes exist, enabling FCP to read UltimateSlice animated clips. Vendor `us:*-keyframes` attributes are preserved for lossless UltimateSlice round-trip and take priority over native keyframes when both are present. Also adds `<adjust-volume>` emission (previously omitted) with static and keyframed values.
- **Timeline keyframe visibility**: Clips now render color-coded phase-1 keyframe ticks (scale, opacity, position X/Y, volume) directly on clip bodies so animation timing is visible without opening the Inspector.
- **Keyframe animation phase-1 foundation (position/scale/opacity/volume)**: Added per-clip linear keyframe data model + timeline evaluation, Inspector Set/Remove keyframe controls at playhead for phase-1 properties, MCP automation (`set_clip_keyframe`, `remove_clip_keyframe`) with `list_clips` keyframe visibility, FCPXML `us:*keyframes` round-trip persistence, and export support for phase-1 animated transform/opacity/volume values.
- **Program Monitor short frame cache around playhead**: Added a bounded previous/current/next paused-frame cache (frame-position + render-signature keyed) with explicit invalidation on project/render-setting changes, deferred capture support for async 3+ track seek pulses, cache hit/miss/invalidation diagnostics, and cache-hit-driven tighter paused in-place seek settle budgets to reduce scrub/pause stutter while keeping accurate seek behavior.
- **Timeline clip display controls (height + color labels)**: Added per-track height presets (Small/Medium/Large) via track-header context menu, per-clip color labels in the Inspector, variable-height timeline geometry/hit-testing support, clip-color rendering, FCPXML persistence (`us:track-height`, `us:color-label`), and MCP automation (`set_track_height_preset`, `set_clip_color_label`) with list output coverage.
- **Through-edit boundary detection model groundwork**: Added deterministic helper logic to detect join-safe through-edit cut boundaries on adjacent same-track clips (same source path, contiguous source/timeline ranges, compatible clip kinds, and no boundary transition metadata), with reusable APIs for timeline/UI consumers and future join-back operations plus focused edge-case tests.
- **Timeline clip context menu now hides non-actionable items**: Right-click clip actions are now filtered to only show currently available operations (join/freeze/link/unlink/group-align/audio-sync), reducing disabled-menu noise while preserving existing selection behavior and the no-popup case when nothing applies.
- **Timeline through-edit visual indicators**: Timeline rows now render subtle dotted boundary lines at join-safe through-edit cuts (contiguous same-source edits), automatically tracking timeline edits, zoom/scroll state, and track reordering while staying out of the way of transition markers and active clip-selection borders.
- **Through-edit join-back action**: Added an undoable timeline join-back operation (right-click clip context action and `Ctrl+Shift+B`) that merges a single selected join-safe through-edit boundary into one clip while preserving compatible clip metadata/effect settings and carrying the right segment's outgoing transition metadata forward.
- **Through-edit consistency hardening**: Through-edit indicator rendering now uses the same clip-metadata compatibility gate as join-back, and new regression tests cover transition metadata edge cases (left-boundary transition blocking, right-side outgoing transition passthrough, whitespace transition kind handling) to keep detector/UI/join behavior aligned.
- **Freeze-frame workflow end-to-end (model/UI/preview/export)**: Freeze-frame metadata now persists with backward-compatible clip defaults and FCPXML vendor attrs (`us:freeze-frame`, `us:freeze-source-ns`, `us:freeze-hold-duration-ns`), timeline freeze creation is undoable and discoverable via `Shift+F` + context menu + track toolbar, and Program Monitor/ffmpeg export now match on single-frame hold timing while treating freeze-frame video clips as intentionally silent.
- **Freeze-frame transition safety on insert**: Creating a freeze frame now clears copied transition metadata on the generated hold (and the new pre-freeze boundary) so insertion does not accidentally inherit stale transitions from the source clip.
- **Freeze-frame insertion can ripple all tracks**: Creating a freeze frame now ripples all tracks after the playhead (not just the selected track). Overlapping clips on non-selected tracks are split at the playhead and only the right segment is shifted, preserving global timeline alignment while keeping the edit undoable as one operation.
- **Source monitor A/V auto-linking is now optional for Append/Insert/Overwrite**: Source-monitor edits can be configured to either create linked same-source video+audio clip pairs (when matching track kinds exist) or use single-clip placement behavior. When linked pairs are created, embedded audio on the video clip is muted so the dedicated audio-track clip is the only audio source.
- **MCP Add/Insert/Overwrite source placement parity**: MCP `add_clip`, `insert_clip`, and `overwrite_clip` now use the same source-operation placement plan as Source Monitor Append/Insert/Overwrite, including optional linked A/V pair placement, embedded-video-audio suppression for linked pairs, audio-only/silent-video handling, single-kind fallback, and no-match behavior.
- **Startup project argument open**: Passing a project file path as a program argument now opens that `.uspxml`/`.fcpxml` immediately during app startup, using the same background parse path as the Open dialog so UI startup remains responsive.
- **Solo track workflow across UI/preview/export/MCP**: Track headers now include an `S` solo toggle (plus `S` shortcut on the active track), Program Monitor/export honor active solo+mute track sets, `list_tracks` reports `soloed`, MCP can set solo state via `set_track_solo`, and FCPXML vendor attrs now persist per-track mute/lock/solo flags (`us:track-muted`, `us:track-locked`, `us:track-soloed`).
- **Transition preview matching**: The program monitor now previews cross-dissolve, fade-to-black, wipe-right, and wipe-left transitions in real time, matching FFmpeg `xfade` export output. Uses compositor pad alpha animation for dissolve/fade transitions and videocrop animation for wipe transitions. Both playback and scrubbing show the transition effect.
- **Automatic audio crossfades across playback/export + MCP controls**: Timeline crossfade settings (`crossfade_enabled`, `crossfade_curve`, `crossfade_duration_ns`) now drive Program Monitor preview and export fades at adjacent same-track audio edit points (with short-clip-safe clamping), with MCP automation via `get_preferences` and `set_crossfade_settings`.
- **Consistent LUT + color correction order across preview/export/prerender**: LUT is now applied first in all three pipelines (export, prerender, proxy-based preview), so color corrections (temperature, tint, brightness, contrast, etc.) operate on the LUT-graded image consistently. Previously export applied LUT last, causing visible mismatches when adjusting temperature on LUT-graded clips. Also added missing temperature/tint filters to the prerender FFmpeg path.
- **Calibrated frei0r preview–export color matching**: Replaced hand-tuned frei0r parameter formulas with empirically calibrated degree-4 polynomial curves fitted against FFmpeg export via multi-start L-BFGS-B optimization (`tools/calibrate_frei0r.py`). Improvements by slider: temperature ~79 %, midtones ~51 %, highlights ~48 %, shadows ~21 %, tint ~15 %. Temperature and tint polynomials give absolute coloradj_RGB values; shadows/midtones/highlights contribute deltas to the 3-point neutral control points.
- **Calibrated preview–export color matching**: Replaced hand-tuned linear preview mappings with empirically calibrated polynomial curves. An offline pipeline (`tools/calibrate_color.py`) sweeps each color slider through FFmpeg export filters on SMPTE color bars, optimizes GStreamer `videobalance` parameters via L-BFGS-B, and fits degree-4 polynomials. Improvements by slider: brightness ~53 %, contrast ~35 %, saturation ~46 %, temperature ~35–52 %, shadows 74–94 %, highlights 78–88 %. All eight sliders (brightness, contrast, saturation, temperature, tint, shadows, midtones, highlights) now produce a closer preview of the final FFmpeg export result. Denoise/sharpness are unchanged (fundamentally different algorithms).
- **Per-channel RGB temperature/tint preview via frei0r**: Temperature and tint now use `frei0r-filter-coloradj-rgb` for per-channel RGB gain adjustment in the preview pipeline, matching FFmpeg's `colortemperature` filter (Tanner Helland algorithm). White areas now shift correctly with the temperature slider. Falls back to hue-rotation approximation if frei0r is unavailable.
- **Per-luminance-range shadows/midtones/highlights via frei0r**: Shadows, midtones, and highlights now use `frei0r-filter-3-point-color-balance` for independent luminance-range control in the preview pipeline, replacing the global videobalance polynomial approximation. Falls back to calibrated polynomial mapping if frei0r is unavailable.

### Fixed
- **Transform overlay reflects keyframe-interpolated position**: The on-screen transform tool (bounding box + handles) now evaluates phase-1 keyframes at the current playhead position, so scale and position handles track the clip's actual rendered location during scrubbing and playback instead of showing the static fallback values.
- **FCPXML keyframe attribute loading parity**: Import now XML-unescapes attribute values before parsing `us:*keyframes` JSON payloads, so app-saved keyframe arrays (`&quot;...&quot;`) round-trip correctly. The parser also repairs legacy/malformed unescaped keyframe JSON attributes on load, preventing dropped phase-1 keyframes in preview/export after reopen.
- **GLib file-open startup warning**: Enabled GTK application file-open handling (`HANDLES_OPEN`) so launching with a file argument no longer logs `This application can not open files.`
- **Proxy generation with LUTs broken**: Proxy transcodes failed because the temp file used a `.partial` extension that ffmpeg could not auto-detect as MP4. Added explicit `-f mp4` format flag so ffmpeg writes the correct container regardless of the temp filename.
- **Color grading sliders not updating live preview**: When clips started with default values, the effects bin was built without `videobalance`/`gaussianblur` elements (an optimization). Moving a slider triggered a pipeline rebuild, but `compute_reuse_plan` compared the clip against its own in-place-updated entry — always matching — so the slot was reused without creating the missing elements. Added `slot_satisfies_clip()` to verify the slot actually has the GStreamer elements the desired clip needs. Also unified `need_balance` checks across `build_effects_bin`, `effects_topology_matches`, `slot_satisfies_clip`, and `update_current_effects` to include shadows/midtones/highlights (which are approximated via `videobalance`).
- **Freeze-frame preview showing black in Program Monitor**: Freeze clips now force decoder seeks to use accurate, non-keyframe-seeking flags in the live preview path, and the preview seek stop window now spans one timeline frame duration (instead of 1 ns). This prevents dropped held frames during freeze seeks and fixes black freeze holds while preserving export behavior.
- **Freeze-frame playback black regression (program preview)**: Program preview freeze clips now include an explicit `imagefreeze` hold stage in the per-slot effects chain, and slot topology compatibility checks now treat freeze-hold requirements as part of reuse eligibility. This keeps freeze clips visible during active playback instead of falling to black.
- **Primary-track export sequence drop with mixed transitions + cuts**: MP4 export no longer drops later primary-track clips when a transitioned boundary is followed by a normal cut. The transition chain now applies `xfade` only on explicit transition boundaries and uses hard `concat` for non-transition boundaries, preserving full timeline video duration (including clips like `GX010092` in the release showcase).

### Documentation
- **README recommended system specs**: Added a three-tier (Minimum / Recommended / Ideal) hardware spec table with matching UltimateSlice preference settings for 1080p through 4K real-time editing, plus notes on VA-API codecs, FFmpeg export, and Flatpak GPU access.
- **Roadmap AI Music Generation planning**: Added a phased roadmap item under AI & Automation for MusicGen/MusicGPT integration — draw-region UX on audio tracks (phase 1), local model backend via `musicgpt` crate / ONNX / Python `audiocraft` subprocess (phase 2), and prompt popover with auto-duration (phase 3).
- **Roadmap clip-group sync planning**: Added a roadmap item for aligning grouped clips by audio or timecode so the planned grouped-edit workflow is tracked alongside existing clip-group features.

### Added
- **Color temperature & tint correction**: Per-clip temperature (2000–10000 K, default 6500 daylight) and tint (−1.0 green to 1.0 magenta) sliders in the Inspector Color Correction section. Preview uses GStreamer `videobalance` hue approximation for real-time feedback; export uses ffmpeg `colortemperature` filter for accurate Kelvin-based correction and `colorbalance` for tint. Includes FCPXML persistence (`us:temperature`, `us:tint`), MCP `set_clip_color` tool parameters, and full pipeline integration for both primary and overlay tracks.
- **Chroma key (green/blue screen removal)**: Per-clip chroma key compositing with enable toggle, green/blue/custom color presets, tolerance and edge-softness sliders in the Inspector panel. Uses GStreamer `alpha` element for live preview and ffmpeg `colorkey` filter for export. Keyed clips become transparent at the target color, revealing lower video tracks through the existing compositor pipeline. Includes FCPXML persistence, MCP `set_clip_chroma_key` tool, and full undo/redo support via project-changed cycle.
- **Auto-linked same-source A/V placement**: Drag/drop and MCP clip placement now auto-create linked video+audio clip pairs for eligible sources with both video and audio streams, target matching video/audio tracks together, and suppress duplicate embedded video-track audio while the linked audio-track peer exists. Source monitor operations (Append, Insert, Overwrite) now respect the optional source-monitor A/V auto-link mode (enabled: linked A/V pair; disabled: single-clip placement).
- **Timeline clip linking with MCP support**: Added manual clip link/unlink workflows (`Ctrl+L`, `Ctrl+Shift+L`) with synchronized selection/move/delete behavior, persistent `link_group_id` FCPXML round-tripping, LINK timeline badges, cyan linked-peer inset highlight, and new MCP `link_clips` / `unlink_clips` tools.
- **Timeline link context menu**: Right-clicking a selected clip now opens clip actions for **Link Selected Clips** / **Unlink Selected Clips**, while preserving the current multi-selection when you right-click within it.
- **Grouped clip timecode alignment**: Added **Align Grouped Clips by Timecode** to the timeline clip context menu plus MCP support, using persisted source-time metadata to realign grouped clips and preserving that metadata through UltimateSlice FCPXML round-trips.
- **Automatic timecode extraction on import**: GStreamer Discoverer now extracts `GST_TAG_DATE_TIME` from media files during background probe and populates `source_timecode_base_ns` on `MediaItem`, making grouped-clip timecode alignment work out-of-the-box without manual entry or FCPXML import.
- **Audio cross-correlation sync**: Added **Sync Selected Clips by Audio** to the timeline right-click context menu and MCP `sync_clips_by_audio` tool. Uses GCC-PHAT (Generalized Cross-Correlation with Phase Transform) via `rustfft` to align 2+ clips by matching audio content — the primary sync engine for multi-cam workflows. Includes 300–3 kHz bandpass prefilter for robustness across different microphone types, 15-second windowed extraction to handle clips of very different lengths, and smoothed PHAT (β=0.73) normalization for sharp correlation peaks resistant to reverb and recording-level differences. Runs on a background thread with confidence scoring and undo support. Status bar now shows "Syncing clips by audio…" while the operation is in progress.

### Fixed
- **Inspector slider domain mismatches**: The volume slider handler stored raw dB values into `clip.volume` (expects linear), causing the slider to jump to mute on refresh. The `connect_color_slider` helper incorrectly applied `db_to_linear_volume()` to all 8 color effect sliders (brightness, contrast, saturation, denoise, sharpness, shadows, midtones, highlights) which are already linear, mangling stored values. Volume now converts dB→linear before storing; color sliders now pass values through directly.
- **Audio sync seek race condition**: Fixed `extract_raw_audio` seeking from Playing state where `uridecodebin` pads weren't linked yet, causing seeks to silently fail and clips to extract audio from position 0. Now uses Paused→wait→seek→wait→Playing pattern (matching `thumb_cache.rs`), `ACCURATE` seek flag instead of `KEY_UNIT`, and seek result checking. Also improved error reporting to show confidence values and detect zero-offset (already aligned) results.
- **Source monitor Append/Insert/Overwrite placing dual clips**: Auto-linked A/V placement was incorrectly creating paired video+audio clips for every source monitor operation, even for single-clip sources. Append, Insert, and Overwrite now use simple single-clip placement matching the expected behavior; drag-and-drop retains auto-link pairing for multi-track workflows.
- **Media Library thumbnail refresh after import/open**: Thumbnails now trigger a debounced batch redraw when extraction completes, and media-grid sync now detects content changes (not just item count), so thumbnail tiles update without requiring manual resize/redraw.
- **GTK slider min-size runtime warnings**: Added a generic `slider` CSS reset (`border/margin/padding/box-shadow: none`) and retained explicit scale-thumb sizing, eliminating startup `GtkGizmo ... (slider) reported min width/height -4` warnings.
- **About/Credits license notice placeholder**: Replaced the remaining `TBD` project license label in Preferences → About/Credits with the actual `GPL-3.0-or-later` license text.
- **Legacy GTK UI deprecation noise cleanup**: Added narrowly scoped `#[allow(deprecated)]` on existing Dialog/ComboBoxText-based UI entry points in Preferences, Toolbar dialogs, and Timeline shortcuts help to keep warning output clean without changing runtime behavior.
- **GTK CSS provider deprecation in app startup**: Replaced deprecated `CssProvider::load_from_data` with `load_from_string` in `src/app.rs` to keep startup CSS loading on the current GTK API without behavior changes.
- **Non-deprecation build/test warning cleanup pass**: Cleared current non-deprecation warning set from `cargo build --quiet` / `cargo test --quiet` by removing unused imports/variables, fixing ignored `Result` handling in thumbnail extraction, and adding narrowly scoped `#[allow(dead_code)]` only for intentionally reserved APIs.
- **Ctrl+Shift+click selection precedence**: `Ctrl/Cmd+Shift+Click` now follows Ctrl/Cmd toggle semantics (single clicked clip toggle) instead of triggering Shift range selection, preventing unintended multi-clip cross-track selections.
- **Slide edit neighbor over-extension**: Slide drag delta is now clamped by neighbor bounds, preventing over-extension of adjacent clips; edge clips now keep Slide active with available-side-only compensation.
- **Timeline modifier multi-select reliability**: Ctrl/Cmd+click and Shift+click selection state is now preserved when a drag gesture begins, preventing unintended collapse back to single selection during timeline interactions.
- **Proxy-in-progress playback stall**: Proxy selection now ignores incomplete/unusable proxy files and keeps Program Monitor playback on original media until a valid proxy is ready; proxy transcodes are now published atomically (temp file → final path) to avoid exposing half-written proxies.
- **Export/proxy percentage realism**: Export and proxy progress now estimate output size from bitrate×duration and track ffmpeg `total_size` against that estimate, while keeping in-progress values capped below 100% until ffmpeg completion.
- **Prerender-exit boundary cold-starts**: While playing inside a prerender slot, the next boundary now prewarms post-prerender clip resources immediately (instead of relying on logical incoming-only diffing), reducing handoff stalls when exiting prerender playback.
- **Background prerender proxy-resolution alignment**: When proxy mode is enabled, complex-section prerender segments now render at the active proxy scale (Half/Quarter) instead of full project resolution, reducing prerender cost and matching configured proxy behavior.
- **Prerender overlap truncation causing black tail**: Background prerender segment duration now spans the full active 3+ overlap window up to the next boundary (instead of a hard 4s cap), avoiding mid-overlap prerender exhaustion that could black the Program Monitor.
- **Background prerender A/V prototype path**: Prerender segments now include mixed audio and the prerender slot links both video and audio to the main pipeline, so overlap playback can use a single prerender decoder branch instead of synthetic per-clip audio-only slots.
- **Prerender top-left zoom/crop artifact in Quarter/Half preview**: Prerender playback slots now pass through a preview-processing-size scale/caps stage before the compositor, matching live slot sizing and preventing full-resolution prerender frames from being clipped to the compositor's smaller output caps.
- **Background prerender queue flood during active overlap playback**: Playing-time prerender scheduling no longer queues a new segment per poll tick from the moving playhead position. Scheduling now stays boundary-based, preventing excessive in-flight jobs and improving odds that the intended overlap segment is ready in time.
- **Background prerender not used during full playthrough with Real-time preview enabled**: Boundary transitions now prefer prerender-capable full rebuilds for 3+ track overlaps when background prerender is enabled, so ready prerender segments are actually consumed instead of being bypassed by the realtime incremental path.
- **Background prerender link-race regression**: Boundary rebuild now gives prerender slots a short link grace window, then forces a normal live rebuild if the prerender slot is still unlinked. This avoids lingering unstable prerender states that could cause visible playback glitches.
- **Background prerender promotion path**: When a prerender segment finished while playback was already inside that overlap window, promotion could be short-circuited by the continuing-decoder fast path, so prerender was never actually used. Promotion now forces a full rebuild so prerender slot selection runs, and rebuild logs now explicitly report unavailable/ready/promote/used states.
- **3-track audio-only/black-video handoff under load**: Playback rebuild waits for 3+ active slots were capped too aggressively, allowing boundary resume before compositor frames arrived. Playback wait budgets for heavy overlap were relaxed, and prerender telemetry was added (queued/ready/failed) to make boundary behavior diagnosable from logs.
- **Background prerender video handoff robustness**: Rebuild now immediately falls back to normal live slot construction when a synthetic prerender video slot fails to link within the boundary link budget, preventing black-video states while audio continues. Background prerender cache keys are also versioned and now include clip timeline/track identity to avoid stale or mismatched cached composite segments being reused.
- **Background prerender playback regressions**: Fixed prerender boundary handoff issues that could freeze on a single frame or go black after section changes. Program Monitor now tracks active prerender clip sets correctly across boundaries and uses normalized prerender segment timestamps for composition playback.
- **Preview transform scaling regression**: Corrected Program Monitor zoom/position math to use current preview-processing dimensions, fixing mis-scaled proxy/transform positioning after preview-quality processing-resolution optimization.
- **Preferences selector double-frame styling**: Narrowed ComboBox/DropDown CSS selectors so only the interactive button surface is styled, removing the nested/double border look in Preferences selectors.
- **Arbitrary rotation follow-up fixes**: Resolved three regressions after the arbitrary-angle rotation rollout: (1) rotated clips no longer get prematurely clipped when scaled down before rotation, (2) transform overlay handles now follow the clip's rotation angle, and (3) export rotation direction now matches Program Monitor/Inspector direction.
- **Live preview rotation at identity start**: Rotation now updates immediately in Program Monitor even when a clip initially loaded with `0°` rotation. The rotation/flip transform path is now always present in slot effects bins, so changing from 0° to any angle no longer requires a pipeline rebuild.
- **Tool/preview rotation direction parity**: Program Monitor rotation now uses the same clockwise-positive convention as the transform overlay and Inspector controls (GStreamer `rotate` angle sign corrected).
- **Preview/export rotation parity**: Export rotation mapping now matches Program Monitor orientation; ffmpeg rotate angle conversion now preserves the clip's signed degree value directly so clockwise/counterclockwise direction is consistent between preview and output.
- **FCPXML rotation conformance on dirty imported saves**: In-place transform patching now updates standard `adjust-transform@rotation` (and synced `us:rotate`) alongside scale/position, so edited rotation is persisted in FCPXML-standard transform fields instead of only custom metadata.
- **Recent projects menu hygiene**: The Recent menu now displays at most 10 entries and skips paths that no longer exist on disk.
- **Export progress premature 100%**: Export progress now estimates completion from bitrate×duration and ffmpeg `total_size`, and in-progress updates are capped at 99% until ffmpeg exits successfully. This prevents showing 100% while muxing/final file growth is still in progress.
- **GTK gesture_group assertion failure**: Fixed `Gtk-CRITICAL: gtk_gesture_group: assertion … failed` warning at startup. The timeline's click and drag gestures were being grouped before the drag gesture was added to the widget. Reordered to add the controller first, then group.
- **Audio-only track ending early in preview**: When a video track and an audio-only track ended at the same timeline position, the audio could go silent before the end during playback. The audio pipeline drifted ahead of the wall-clock timeline during video pipeline rebuilds (clip boundary transitions) because it kept running while the rebuild blocked. The audio pipeline is now re-synced after every video rebuild so its position matches the reset wall clock.
- **Export transform position mismatch**: Overlay clips with `position_y > 1.0` (extending past the bottom frame edge) were mispositioned in exports because ffmpeg's `pad` filter clamped the placement to fit within the output frame. The export now pre-crops overflow on all four edges before padding, matching the preview's GStreamer `videobox` behavior which clips content beyond the frame boundary.

### Performance
- **Idle background prerender scheduling + status bar integration**: Background prerender now also queues nearby complex sections while paused/stopped (throttled around the playhead), and the existing bottom proxy progress bar now reports prerender activity (or combined proxy+prerender activity).
- **Background disk prerender for complex overlap sections**: Added an optional background prerender mode that renders upcoming 3+ track overlap windows to temporary disk clips and uses them when available at rebuild boundaries, with automatic fallback to live rebuilds when cache misses occur.
- **Boundary rebuild debounce**: Playback boundary handling now debounces duplicate full-rebuild attempts for the same desired clip set within a short window (~120ms), reducing rebuild thrash during transient overlap churn.
- **Realtime prewarm gating**: Pre-preroll sidecar prewarming is now constrained to active playback near imminent boundaries and torn down outside the boundary window, reducing steady-state realtime-preview overhead.
- **Reuse-path matching expansion**: Same-count decoder reuse now matches desired clips to reusable slots by compatible source/effects topology (not only positional index) and re-applies compositor z-order from desired track order.
- **Occlusion hot-path probe reduction**: Audio-only occlusion/reuse hot paths now use fast cached audio-presence lookups (defaulting non-blocking when unknown) to avoid blocking Discoverer probes in tight rebuild loops.
- **Program Monitor quarter/half preview workload reduction**: Preview quality divisor now applies to ProgramPlayer processing resolution (per-slot effects + compositor caps), not just final monitor output scaling. This reduces 3-track overlap playback cost when preview quality is lowered.
- **Real-time preview boundary transitions**: When "Real-time preview" is enabled in Preferences → Playback, clip boundary transitions use an incremental pad-offset approach instead of full pipeline teardown/rebuild. Departing clips are hidden (alpha=0, volume=0) without removing pads, and entering clips get new decoder slots with `pad.set_offset()` for running-time alignment. This avoids the GstVideoAggregator flush that previously caused timing corruption. Expected improvement: ~0ms for remove-only transitions, ~200-400ms for add-only (single-decoder preroll), down from 800-3000ms for full rebuilds. Falls back to full rebuild when the preference is off or when >3 clips enter simultaneously.
- **Continuing decoders at boundary crossings**: When adjacent clips share the same source file (common after splits, rough cuts from single footage), the program player now reuses existing decoder slots instead of tearing down and rebuilding the full pipeline. This avoids codec init, element creation, and stream discovery overhead, reducing boundary crossing latency from ~800-2800ms to ~200-600ms (~60-75% improvement). Falls back to full rebuild when source files differ, slot count changes, audio presence differs, speed/reverse settings differ, or effects topology changes.

### Added
- **Grouped peer visual highlight**: Selecting a clip in a `Ctrl+G` clip group now applies a secondary border to the other non-selected clips in that group, improving grouped-edit visibility while keeping the primary selected clip border unchanged.
- **Cross-track Shift+click range selection + selected-set move-together**: Shift+click now supports cross-track time-range selection (anchor↔click across all tracks), and dragging a selected clip now moves the active multi-selection together while still expanding grouped clips as a unit.
- **Playhead-relative bulk selection shortcuts**: Added timeline `Ctrl+Shift+→` (select clips forward from playhead) and `Ctrl+Shift+←` (select clips backward from playhead) for faster multi-clip operations.
- **Timeline clip grouping**: Added persistent clip grouping with `Ctrl+G` (group) and `Ctrl+Shift+G` (ungroup). Group members now move and delete as a unit (including ripple delete), with undo/redo support, and group IDs round-trip through FCPXML via `us:group-id`.
- **Ripple Delete shortcut**: Added track-local ripple delete on `Shift+Delete` / `Shift+Backspace` for selected timeline clip(s), with undo/redo integration.
- **Timeline multi-select completion**: Added Shift+click same-track range selection, Ctrl/Cmd+click toggle selection, Ctrl+A select-all, and rubber-band marquee selection in empty timeline body drags, while keeping Inspector focus on a primary selected clip.
- **Timeline copy/paste shortcuts**: Added `Ctrl+C` (copy selected timeline clip), `Ctrl+V` (paste-insert at playhead), and `Ctrl+Shift+V` (paste copied attributes onto selected clip), with undo/redo integration.
- **Background prerender preference + MCP control**: Added a new Playback preference and MCP tool (`set_background_prerender`) to toggle temporary disk prerender behavior for complex sections.
- **Status-bar Background Render toggle**: Added a bottom status-bar toggle next to **Track Audio Levels** to quickly enable/disable background prerender without opening Preferences.
- **Preview LUTs preference + MCP control**: Added a new Playback preference and MCP tool (`set_preview_luts`) to generate/use project-resolution LUT-baked preview media when Proxy mode is Off.
- **MCP playhead query for FPS checks**: Added `get_playhead_position` MCP tool returning `timeline_pos_ns`, used by automated relative FPS regression checks.
- **Perf/FPS regression harness scripts**: Added `tools/mcp_call.py`, `tools/proxy_perf_matrix.sh`, and `tools/proxy_fps_regression.py` for repeatable local profiling and relative FPS regression testing.
- **MCP occlusion toggle**: Added `set_experimental_preview_optimizations` MCP tool to toggle occlusion optimization runtime behavior for automated playback testing and tuning.
- **Proxy-path trace diagnostics**: Program Monitor slot build/rebuild logs now include source-vs-resolved URI details (proxy key, proxy/fallback mode, path existence) to verify when quarter proxies are actually used.
- **Still-frame export UI**: Added **Export Frame…** for the currently displayed Program Monitor frame (`.png`, `.jpg`/`.jpeg`, `.ppm`), with PNG/JPEG conversion via ffmpeg and brief title-bar success/failure feedback.
- **Program Monitor safe area guides**: Added a **Safe Areas** toggle in the Program Monitor header that draws broadcast framing guides on the canvas (action-safe 90% and title-safe 80%). The toggle state is persisted in UI state and restored on launch.
- **Basic audio mixing meters**: Added thinner per-track stereo level meters (timeline track order) in **timeline track labels** while keeping the Program Monitor master stereo VU meter. Program playback publishes per-track peak levels from per-slot GStreamer `level` elements, and audio-only/reverse-audio paths also feed track meters so mixed monitoring reflects active timeline audio layers. The bottom-left status bar now includes a **Track Audio Levels** eye toggle (`view-visible-symbolic` / `view-conceal-symbolic`) to show/hide track-header meters, while proxy queue label/progress are shown only when proxies are actively generating.
- **GitHub Actions CI on push**: Added two workflows under `.github/workflows/` — `cargo-ci.yml` (installs GTK/GStreamer deps, runs `cargo build --locked` and `cargo test --locked`) and `flatpak-ci.yml` (builds `io.github.kmwallio.ultimateslice.yml` and uploads a `.flatpak` artifact).
- **Real-time preview preference**: New "Real-time preview" checkbox in Preferences → Playback. When enabled, upcoming decoder slots are pre-built so clip transitions during playback are near-instant. Uses more CPU and memory. Disabled by default. Also available via the `set_realtime_preview` MCP tool.
- **Experimental preview optimizations**: New "Experimental preview optimizations" checkbox in Preferences → Playback. When enabled, fully-occluded clips (hidden behind opaque full-frame clips above them) use lightweight audio-only decoder slots during preview playback, skipping video decode to reduce CPU/GPU usage at multi-track overlap boundaries. Falls back to full slots if audio-only slot creation fails. Disabled by default.
- **Rust unit tests**: Added `#[cfg(test)]` test modules to `src/model/clip.rs`, `src/model/track.rs`, `src/model/project.rs`, `src/undo.rs`, and `src/fcpxml/parser.rs`. Tests cover clip construction and duration calculations, track clip management, project settings and markers, all undo/redo command types, FCPXML time/frame-rate parsing, and full FCPXML document parsing. 62 tests total.
- **MCP `take_screenshot` tool**: New MCP server command that captures a PNG screenshot of the full application window using the GTK snapshot API and GSK `CairoRenderer`. The PNG is written to the current working directory with a timestamped filename (`ultimateslice-screenshot-<unix_epoch>.png`). The tool returns `{"ok": true, "path": "..."}` on success.
- **Shadows / Midtones / Highlights color grading**: Three new sliders in the Inspector's Color section allow per-luminance-band grading. Preview uses a `videobalance` approximation for real-time feedback; export uses ffmpeg `colorbalance` for accurate per-band rendering. Values persist in the project file (`shadows`, `midtones`, `highlights` on each clip, default 0.0).
- **License**: Added `LICENSE` file (GPL-3.0-or-later). This license is required for Flatpak distribution because the build includes x264 (GPL-2.0-or-later) and FFmpeg compiled with `--enable-gpl` (which enables GPL-licensed components such as libx264). GPL-3.0-or-later is compatible with GPL-2.0-or-later and with all MIT/Apache-2.0 Rust crate dependencies. The `Cargo.toml` package manifest now also declares `license = "GPL-3.0-or-later"`.
- **Reverse playback per clip**: A new "Reverse (play clip backwards)" checkbox in the Inspector's Speed section marks a clip for reversed playback. On export, the ffmpeg `reverse` filter (video) and `areverse` filter (audio) are applied before any speed adjustment, so the clip plays frame-by-frame in reverse order. The timeline shows a `◀` badge on reversed clips (e.g. `◀ 2×` when also sped up). The `reverse` flag persists in the project file via the `us:reverse` FCPXML attribute and is loaded correctly on project open.
- **Unsaved changes confirmation flow**: New project replacement/exit actions now prompt when the current project is dirty, offering **Save**, **Discard**, or **Cancel** before continuing. The guard is wired into toolbar New/Open/Open Recent actions and window close, and successful saves now clear the dirty flag and persist the current project path.

### Changed
- **Background Render status toggle icon states**: The status-bar Background Render toggle now shows `process-stop-symbolic` when disabled and `system-run-symbolic` when enabled.
- **Export split-button workflow**: Toolbar export is now a linked split control (**Export | ▼**) styled as a single control. The main **Export** action opens video export settings, while the dropdown hosts extra actions (currently **Export Frame…**). The standalone Program Monitor **Export Frame…** button was removed.
- **Export split-control styling polish**: Updated the **Export | ▼** control to use a regular button + popover for the dropdown segment (instead of a `MenuButton`) so both segments share identical primary-button styling.
- **Still-frame export behavior update**: Exported frames now capture at project canvas resolution (full compositor frame, not preview-divisor monitor resolution). During frame export while playing, playback is paused internally for capture and then resumed automatically.
- **Arbitrary clip rotation controls**: Transform rotation is now arbitrary-angle instead of preset-only. The Inspector now uses a dial/knob plus numeric degree input (−180° to 180°), the Program Monitor transform overlay includes a rotation handle, preview rotation uses GStreamer's arbitrary-angle `rotate` element path, and export applies matching ffmpeg `rotate` filtering so output matches preview.
- **GTK4/libadwaita control styling parity**: Expanded `src/style.css` with Adwaita-style dark-theme rules for controls already used in the app, including linked scope tabs, flat/round buttons, transitions list styling (`small-btn`, `boxed-list`), popovers (Recent menu), `DropDown`/`ComboBoxText`, slider trough/knob styling, check/radio controls, and color-scopes panel chrome. This keeps native GTK4 widgets visually consistent with UltimateSlice's existing dark palette.
- **Timeline ruler zoom density**: The timeline ruler now uses multi-tier marks as you zoom (major, mid, and minor ticks), and high zoom levels add intermediate labels with sub-second precision for finer navigation without overcrowding at lower zoom.
- **FCPXML version support**: Parser now accepts FCPXML files through version 1.14, and project export now writes FCPXML version 1.14 by default.
- **FCPXML 1.14 import compatibility**: Parser now resolves media paths from nested `<media-rep src="...">` when `asset@src` is absent, imports only the first project timeline from multi-project FCPXML files, and falls back to lane/media-type-based track routing when UltimateSlice `us:track-*` metadata is not present.
- **FCPXML marker + format compatibility**: Parser now imports `<chapter-marker>` elements, converts clip-contained marker/chapter timestamps from source-time into correct timeline positions using clip `start`/`offset`, and falls back to known format-name presets (for example `FFVideoFormat1080p30`) when numeric format attributes are omitted.
- **FCPXML standard volume import mapping**: Parser now reads `adjust-volume@amount` values (including dB strings like `-6dB` and `-96dB`) and maps them to the Inspector's linear per-clip volume multiplier, so Apple-authored clip-level mute/attenuation imports more accurately.
- **Inspector volume control now uses dB scale**: Audio volume slider range is now `-100 dB` to `+12 dB` (internally mapped to linear gain), so imported FCPXML values like `adjust-volume amount="-96dB"` are represented naturally in the UI while still driving playback/export gain correctly.
- **FCPXML format export metadata hardening**: Export no longer hardcodes `format@name="FFVideoFormat1080p24"` for every project; it now emits known canonical names only for supported presets and otherwise relies on numeric `width`/`height`/`frameDuration` values for cross-editor compatibility.
- **FCPXML reference documentation scope clarification**: `docs/FCPXML.md` now explicitly states it is a broad format reference and calls out UltimateSlice's currently implemented subset (1.10–1.14 core import/export paths), reducing ambiguity between schema reference and shipped feature coverage.
- **FCPXML export media-rep resources**: Resource `<asset>` entries now write source media URIs inside nested `<media-rep>` tags. Non-proxy files are tagged `kind="original-media"` and detected proxy-cache files are tagged `kind="proxy-media"`.
- **Inspector ↔ standard FCPXML mapping (phase 1)**: Transform fields now map to standard FCPXML elements on import/export (`adjust-transform` for scale/position/rotation, `adjust-compositing` for opacity, and `adjust-crop`/`crop-rect` for crop bounds) while existing `us:*` attributes remain for backward-compatible fallback.
- **FCPXML clean-save passthrough**: Imported FCPXML documents are now retained verbatim and written back unchanged when saved without edits, preserving unknown attributes/fields across the full document so external application settings are not dropped on open→save.
- **FCPXML dirty-save unknown-field preservation**: Imported unsupported `asset-clip` attributes and child tags are now preserved and re-emitted on regenerated saves; scale edits continue to update `us:scale` / `adjust-transform@scale`.
- **FCPXML dirty-save asset metadata + DOCTYPE parity**: Regenerated saves now include `<!DOCTYPE fcpxml>`, preserve imported resource `<asset>` unknown fields (including nested `<metadata><md .../></metadata>` payloads), and continue writing source paths via nested `<media-rep>` instead of legacy `asset@src`.
- **FCPXML dirty-save unknown-tag preservation expanded**: Regenerated saves now also retain unknown attrs/child tags across core document structure (`fcpxml`, `resources`, selected `library`/`event`/`project`/`sequence`/`spine`, plus selected `format` attrs), reducing metadata loss when editing imported projects.
- **Project extension UX defaults**: Open dialogs now include `*.uspxml` and `*.fcpxml` (with `*.xml` fallback), Save dialogs now suggest `project.uspxml` by default while still allowing explicit `.fcpxml` saves, and the desktop entry now advertises project XML MIME associations.
- **Shared MIME info for `.uspxml`**: Added `data/io.github.kmwallio.ultimateslice.mime.xml` with `application/x-ultimateslice-project+xml` and `*.uspxml` glob, and installed it via the Flatpak manifest so desktop environments can associate/open UltimateSlice project files.
- **Closer dirty-save output for imported FCPXML transform edits**: When an imported project is dirty due clip transform changes and matching `adjust-transform` nodes are present, save now patches those transforms in-place on the original XML (including nested clips) instead of forcing full regeneration, preserving source asset IDs/structure much more closely.
- **FCPXML `/Volumes` path remap fallback**: On import, missing `/Volumes/...` source media paths are retried under common Linux external-drive mounts (`/media/<user>/...`, `/run/media/<user>/...`, `/media/...`, `/run/media/...`, `/mnt/...`) plus the opened FCPXML mount-root fallback; percent-encoded URI segments (for example `%20`) are now decoded before lookup/remap. Runtime clip paths use the found file, while export preserves the original imported path in XML.
- **Proxy cache root + fallback hardening**: Proxy generation now targets a managed local cache root (`$XDG_CACHE_HOME/ultimateslice/proxies`, fallback `/tmp/ultimateslice/proxies`) for faster external-drive playback, with deterministic fallback to alongside-media `UltimateSlice.cache` when local-cache writes/transcodes fail.
- **Managed proxy cleanup lifecycle**: On startup, UltimateSlice prunes stale managed cache entries older than 24 hours via an ownership index; on project unload/close it removes managed cache files for the current session/project and clears in-memory proxy mappings.
- **Eager near-playhead proxy priming**: On project reload, UltimateSlice now pre-requests a capped, proximity-ordered set of near-playhead video sources before first program-player rebuild, reducing first-hit proxy latency on slower storage while keeping existing background proxy refresh behavior.
- **Media Library import button**: Replaced the always-visible big "**+ Import Media…**" button with context-sensitive controls. When the library is empty, the big button is shown as before. Once any media is present, the big button hides and a compact **+** button appears in the **Media Library** panel header, keeping the interface cleaner while files are loaded.
- **New app icon**: Replaced the previous katana-and-cinema-camera icon with a GNOME HIG-compliant design. The new icon (`data/io.github.ultimateslice.svg`) shows a camera body on a warm caramel squircle background; a kitchen knife cuts diagonally across the camera, revealing horizontal layers of sponge cake and cream inside — making the "UltimateSlice" wordplay literal. Uses GNOME colour palette (Orange 3–5 background, Dark 2–4 camera body, Blue 2–5 lens, Red 4 record button). Readable at all sizes from 16 px to 512 px.
- **App icon GNOME HIG 3D compliance**: Updated `data/io.github.ultimateslice.svg` to follow GNOME HIG icon style more closely. Removed the diagonal gradient from the squircle background (now a flat `#ffbe6f`). Removed linear gradients from flat surfaces (camera body and film magazine now use flat GNOME Dark palette colours). Added darker front-face profile rectangles (≤4 nominal pixels) to the camera body, film magazine, and carry handle, giving the cinema-camera-cake object the isometric 3D depth required by GNOME HIG. Curved surfaces (lens, knife blade) retain their gradients as permitted.

### Planned (Roadmap additions)
- **Script-to-Timeline**: Added roadmap feature — "Create Project from Script & Clips". Users will be able to import a Final Draft (FDX) or Fountain screenplay alongside a folder of media clips. Each clip is transcribed via speech-to-text (Whisper); transcripts are fuzzy-aligned against the script to find the best-matching scene position. Clips are then placed on the timeline in screenplay order, with sub-clip splits at scene boundaries where a single clip spans multiple scenes. Includes a multi-step wizard with a background STT+alignment pass, confidence indicators for low-confidence matches, an unmatched-clips bin, a re-order-by-script command, and FCPXML persistence for script path, scene IDs, and transcript cache.

### Changed
- **Transform overlay precision controls**: The Program Monitor overlay now supports draggable crop edge handles (left/right/top/bottom), Shift-constrained corner scaling, and keyboard nudges for selected clips (`←/→/↑/↓` = ±0.01 position, `Shift+Arrow` = ±0.1, `+`/`-` = scale up/down). Overlay edits stay synchronized with Inspector sliders and live Program Monitor updates.
- **Agent documentation rule update**: `docs/ARCHITECTURE.md` now requires contributors/agents to verify license compatibility for any newly added crate and to keep the dependency listed in both the in-app **About & Open-source credits** view and `README.md`.
- **Optimized video effects pipeline**: Replaced separate `videoconvert` + `videoscale` with single-pass `videoconvertscale` element for ~2.6× faster color conversion and downscaling per clip. Effects chain now downscales to project resolution early (before effects processing), reducing per-frame cost for high-resolution sources (e.g. 5.3K GoPro → 1080p). No-op effects elements are conditionally skipped, and the scope branch queue is now leaky to prevent backpressure from the waveform/histogram appsink blocking the display path.
- **Adaptive proxy assist for heavy overlaps**: When manual Proxy mode is `Off`, live preview now auto-enables proxy playback in regions with 3+ overlapping video tracks, requests needed proxies in the background, and automatically returns to original media when overlap drops below 3 tracks. Auto mode also selects Quarter proxies when preview quality is reduced to Quarter for smoothness-first playback.
- **Lower boundary handoff stutter in live preview**: `rebuild_pipeline_at()` now avoids a redundant `Paused` state transition and redundant per-decoder state polling before seeks, and `wait_for_paused_preroll()` now uses shorter per-decoder waits during active playback (while keeping conservative paused-scrub waits). This reduces main-thread blocking during clip-set handoffs without changing paused seek correctness behavior.
- **Seek stress performance pass (3+ tracks)**: Program Monitor rebuild now reuses cached audio-stream probe results per media path (avoids repeated Discoverer probes), applies FFmpeg decoder thread caps in paused seek rebuilds, and skips the second paused settle/reseek pass when first-pass pad-link + compositor-arrival checks already succeed.
- **Responsiveness-first open/seek staging**: Program reload now runs in two deferred phases (load first, seek next frame) with ticket-based coalescing so repeated edits/seeks drop stale reload work instead of stacking long GTK callbacks. Timeline seek dispatch is also coalesced to the latest request.
- **Boundary playback smoothness tuning**: Reduced transition hot-path churn by preserving audio-stream probe cache across proxy-path updates and adding proxy auto-assist hysteresis/refresh throttling (minimum dwell before toggles). This cuts repeated Discoverer probes during boundary rebuilds and reduces proxy mode flapping near 2↔3-track overlap boundaries.
- **Auto preview-quality stability**: In `Auto` mode, preview quality divisor changes now use a minimum dwell while playing, reducing rapid Full↔Half↔Quarter renegotiation churn during overlap transitions.
- **Audio-master drop-late preview policy**: During active playback with heavy overlap (3+ active video slots), the display path now switches to drop-late mode (`display_queue` downstream-leaky with tighter buffering, sink QoS enabled, finite max-lateness) to keep shown frames closer to audio time; it automatically restores normal non-leaky buffering when overlap drops or playback pauses/stops.
- **Adaptive slot-queue drop-late policy**: During heavy-overlap playback (3+ active video slots), per-slot compositor branch queues now switch to downstream-leaky mode, reducing branch backpressure and helping keep boundary handoffs responsive; queues automatically return to non-leaky mode outside heavy-overlap playback.
- **Boundary look-ahead prewarm**: While playing, Program Monitor now prewarms the next upcoming clip-boundary active set (within a short window) by resolving effective media paths and priming audio-stream probe cache before the handoff point, reducing synchronous probe work during transition rebuilds.
- **Incoming boundary resource prewarm**: Look-ahead boundary prewarm now also performs lightweight incoming-clip resource warm-up (decoder Ready/Null + effects-bin construction) before handoff, reducing first-use setup work at transition ticks.
- **Pre-preroll sidecar pipelines**: Boundary prewarm now creates an asynchronous sidecar pipeline (`uridecodebin → fakesink`) for each incoming clip, transitions it to Paused with a seek to the clip's source position, and lets GStreamer decode the first frame in background threads. This warms the OS file/page cache and codec initialization state ahead of the real rebuild. Sidecars are torn down when the boundary rebuild starts, at project load, or on stop.
- **Frame-boundary seek deduplication**: Paused timeline scrubbing now quantizes seek positions to the nearest video frame boundary (based on the project frame rate) and skips redundant pipeline work when the playhead hasn't moved to a new frame. This eliminates unnecessary decoder seeks during slow scrubbing where multiple drag events land on the same video frame.
- **Adaptive rebuild wait budgets**: Playback-boundary rebuild waits (decoder preroll, compositor arrival, link settling) now scale dynamically based on a ring buffer of recent rebuild durations. Fast recent rebuilds tighten wait budgets (down to 0.6× nominal), reducing main-thread blocking during transitions; slow rebuilds widen budgets (up to 1.5×) for reliability. Telemetry resets on project load.
- **Launch-screen UI hierarchy polish**: Improved first-use clarity with media/program-monitor empty-state guidance, wider default side panels, a cleaner toolbar grouping separator, and an Inspector empty state that hides dense controls until a clip is selected.

### Fixed
- **Boundary audio dropout from audiomixer running-time drift**: During playback boundary rebuilds, the audiomixer aggregator is now flushed alongside the compositor so both aggregator output segments reset in sync. Previously only the compositor was flushed, causing the audiomixer to classify post-seek audio buffers as "late" and drop them — producing silence on video-clip audio tracks across clip transitions.
- **Boundary playback blip reduction**: Audio pipeline is no longer paused at clip boundaries when only video tracks change, eliminating the audible gap for video-only transitions. Rebuild telemetry now logs per-phase timing (teardown/build/link/preroll/seek). Post-seek wait budgets are tightened when boundary prewarm succeeded (warm file cache), and `wait_for_paused_preroll` skips decoders that already settled.
- **Boundary overlap audio drop when Video 2 enters**: Program Monitor rebuild no longer sends EOS to a slot's audio-mixer pad solely because that slot's video pad has not linked yet. During overlap boundaries, audio now remains active when the clip's audio path is already linked, preventing unintended video-track audio dropouts on slower/heavier media.
- **Boundary rebuild pre-link race during active playback**: When a clip boundary rebuild starts from an already-running slot set, Program Monitor now avoids forcing early pre-link EOS for newly added overlap slots. This gives late `pad-added` links a short settle window and reduces boundary handoff audio holes on heavier media while keeping cold-start/non-playback safety guards intact.
- **FCPXML asset-start source timing for layered clips**: Import now normalizes `asset-clip@start` against `asset@start` when building clip source ranges, so Apple-authored timelines using absolute source-time domains no longer seek out of range in Program Monitor (restores expected Video 2/Audio 1 playback in nested-lane cases).
- **FCPXML transform position parity with Final Cut**: `adjust-transform@position` now round-trips using height-based percentage semantics (both X and Y percentages based on frame height, with Y inversion), mapped into UltimateSlice's scale-aware internal position model; the single-clip dirty-save patch path updates both scale and position consistently, and imported offsets are no longer forcibly compressed to ±1 in playback/export.
- **Multitrack video-clip audio muting regression**: Program Monitor now uses full decode slots for active video clips during rebuilds (occlusion audio-only substitution temporarily disabled), restoring reliable mixed audio when multiple video tracks overlap.
- **Occluded video clips dropping audio in preview**: Program Monitor occlusion rebuilds now fall back to a full decoder slot when the audio-only occlusion slot cannot be created, so clips classified as visually occluded keep playing audio instead of being dropped.
- **Over-aggressive occlusion muting in preview**: Occlusion-based decode skip now requires stricter full-frame transform conditions (centered, unrotated, unflipped, uncropped, opaque, scale ≥ 1) before classifying lower clips as fully occluded, reducing false-positive audio muting on partially covered clips.
- **Reverse clip playback parity (preview + export)**: Program Monitor now honors the Inspector **Reverse** checkbox for timeline playback (video and audio direction, including restored reverse-audio preview routing), keeps non-reversed timeline layers audible while reverse preview is active, reverse seek bounds now satisfy GStreamer segment requirements (`start <= stop`), and export reverse-video processing no longer collapses to near-empty video due non-monotonic timestamp remapping.
- **Media browser stale items after project switch**: Creating a new project or opening another project could leave previous-project library items visible until new items were merged in. Project-replacement flows now clear Source Monitor/media-browser selection and clear the library list before syncing the target project's media.
- **Program monitor stale cached frame after project switch**: Creating/opening a project with no timeline clips could still show the previous project's last rendered frame. Project-replacement reloads now explicitly stop the program player and hide program-monitor picture layers when there are no clips.
- **Source preview freeze on high-resolution HEVC media**: Importing a 5.3K GoPro HEVC file (5312×2988, H.265, ~60 Mbps) and playing it in the source preview could freeze the application. The VA-API decoder outputs GPU memory (`video/x-raw(memory:VAMemory)` / DMABuf) which cannot be CPU-mapped for the `videoconvertscale`/`videobalance` effects chain. Five fixes: (1) lowered VA-API decoder ranks (`vah264dec`, `vah265dec`, etc.) to `MARGINAL` so GStreamer auto-plugging selects software decoders (`avdec_h264`, `avdec_h265`) that output plain `video/x-raw`; (2) added a leaky queue (`max-size-buffers=2, leaky=downstream`) after the prescale capsfilter to decouple the decode/prescale thread from the effects/display chain; (3) source preview now checks for and uses proxy files when available, and auto-requests proxy transcodes for video media loaded into the source monitor; (4) added GStreamer bus error monitoring on the source preview pipeline so decoder/negotiation errors are logged instead of silently freezing; (5) fixed STATE_LOCK deadlock in `Player::play()`/`pause()`/`stop()` where `set_state()` could block indefinitely if the pipeline had a pending async state change — state transitions now defer via a repeating timeout when async work is in progress.
- **Source preview low FPS on high-resolution media**: The source preview effects chain processed video at 1920×1080 even though the widget is typically ~320×200 pixels (~30× more pixels than needed). Four optimizations: (1) adaptive prescale — default resolution lowered to 640×360 with runtime updates to 2× widget size via a 100ms polling timer; (2) removed `gaussianblur` from the filter chain — it only accepts AYUV format, forcing two expensive I420↔AYUV format conversions per frame even when sigma=0 (neutral); denoise/sharpness is preserved during export; (3) eliminated all 4 `videoconvert` elements from the chain — `videocrop`, `videobalance`, `videoflip` all accept I420 natively; (4) added `format=I420` hint to the prescale capsfilter to prevent random RGBA format negotiation. Combined improvement: ~50× reduction in per-frame CPU work at typical widget sizes with neutral effects.
- **Source preview smoothness under load**: Added a playback-only drop-late policy in the source player (`queue leaky=downstream` + tighter buffering + sink QoS/max-lateness while Playing, restored to non-leaky conservative buffering when Paused/Stopped). Source proxy selection is now adaptive when proxy mode is Off (Quarter for small source-monitor sizes, Half for larger), and proxy encoding is tuned for faster decode (`-tune fastdecode`, no B-frames, 1 reference frame, `yuv420p`) to improve preview FPS on CPU-bound media.
- **Adaptive VA-API source decode with safe fallback**: Source preview now detects VA decoder availability and, when hardware acceleration is enabled, prefers a hardware-fast decode mode for smoother playback in `cargo run`/Flatpak. If the hardware path fails (e.g. negotiation/DMABuf/VA errors), the player automatically falls back to the software-filtered decode mode and reloads the current source URI, preventing persistent black/frozen playback.
- **Source monitor seek responsiveness parity improvements**: Added frame-boundary deduplication for paused Source Monitor seeks (skips redundant same-frame re-seeks during scrubbing), plus a dedicated Source Monitor playback-priority setting (`smooth`/`balanced`/`accurate`) exposed in Preferences and MCP (`set_source_playback_priority`). Smooth/Balanced use keyframe seeks for lighter scrub/playback behavior; Accurate preserves frame-precise seeks.
- **Static-like preview after Video 4 clip exits (3→2 transition)**: Incremental boundary fast paths could leave transition updates visually stalled while audio continued. Boundary transitions now use the proven full rebuild path until incremental add/remove correctness is fully hardened.
- **Transition preview frame regression after incremental add path**: Add-only incremental boundary handoff could miss clip-enter visual updates in some transitions and with half-sized proxy preview in the three-track sample. The playback boundary path now uses the proven full rebuild for clip-enter transitions.
- **Incremental boundary slot diffing correctness**: Re-enabled incremental add-only and remove-only boundary paths with 5 bug fixes: (1) reset `comp_arrival_seq` on retained slots before snapshotting so `wait_for_compositor_arrivals` requires genuinely fresh post-seek buffers; (2) use `FLUSH|ACCURATE` seeks during playback to prevent long-GOP keyframe snap-back on proxy media; (3) extracted shared `seek_all_slots`/`reset_slot_arrival_seqs` helpers; (4) wire `teardown_prepreroll_sidecars()` and `record_rebuild_duration_ms()` for proper cleanup and adaptive wait telemetry; (5) use adaptive arrival wait budgets matching the full rebuild path. Falls back to full rebuild automatically if incremental path cannot handle the transition.
- **Incremental boundary paths disabled (retained-decoder regression)**: Incremental add/remove boundary paths are disabled behind a compile-time constant while a retained-decoder frame-refresh regression is investigated. After an incremental update, retained decoders stop producing new frames during playback, leaving the compositor stuck on a single static frame while audio continues. The fixed methods are preserved for re-enabling once the root cause (likely compositor aggregator segment/running-time mismatch on retained pads after flush+seek) is resolved.
- **Incremental boundary paths — frame drop and screencast rendering fix**: Fixed two bugs in the "no-touch-retained" incremental strategy: (1) add-only path didn't seek retained decoders or reset start_time, causing new decoders' running-times to be misaligned (frames dropped as "late" by the sink — screencast video never rendered); (2) remove-only path removed elements while the pipeline was still Playing, risking streaming thread races. Both paths now pause the pipeline, seek ALL decoders individually (per-decoder FLUSH only propagates through its own branch — no compositor-level flush), reset start_time, and wait for all preroll + arrivals before resuming.
- **Incremental boundary add-only disabled (aggregator state regression)**: Disabled the add-only incremental path after confirming that individual per-decoder FLUSH seeks don't trigger GstVideoAggregator's coordinated seek handler — the compositor's internal segment/position tracking stays stale, leaving retained decoders' video frozen. Root cause: only `compositor.seek_simple` (src-pad seek) properly resets aggregator state, but it propagates upstream to ALL decoders causing double-flush corruption. Remove-only incremental path remains enabled (safe — no seeking/flushing of retained decoders). Add-only transitions use the proven full rebuild.
- **Choppy playback after remove-only incremental transition**: Remove-only incremental paths recorded their fast rebuild times (~100ms) into the adaptive wait ring buffer, contaminating the p75 calculation. Subsequent full rebuilds (e.g. 2→3 track transitions) used dangerously tight preroll/arrival budgets (scale=0.6 instead of ~1.0), causing persistent frame drops that never recovered. Fix: remove-only incremental paths no longer record their duration in the telemetry ring buffer.
- **Remove-only incremental disabled (aggregator timing stale after pad removal)**: Disabled the remove-only incremental path after confirming retained decoders produce ≤1 frame/sec after compositor sink pads are removed. The aggregator's internal timing/segment state requires `compositor.seek_simple` to reset, which is the same fundamental GstVideoAggregator limitation that blocked the add-only path. All boundary transitions now use the proven full rebuild.
- **Occlusion-based video decode skip**: Clips fully occluded by an opaque, full-frame clip above them now build audio-only slots (uridecodebin with audio-only caps), completely skipping video decode, effects processing, and compositor pad allocation. Reduces CPU/GPU load and boundary rebuild time in multi-track projects where lower tracks are hidden behind a full-frame overlay.
- **Audio ending early vs video during transition-heavy preview playback**: While active video-slot rebuilds were running at clip boundaries, the audio-only playbin could continue advancing independently, causing cumulative A/V drift (audio finishing early) in multi-track projects. Playback-boundary rebuilds now pause and re-sync the audio-only pipeline to the current timeline position before resuming playback, keeping audio and video end timing aligned.
- **Intermittent project-open seek freeze (`futex_wait`)**: Hardened reload/seek state transitions to avoid pipeline-wide `Ready` resets in the hot path: `ProgramPlayer::load_clips()` now keeps the main compositor pipeline in `Paused` after slot teardown, and `rebuild_pipeline_at()` now resets `start_time` instead of forcing `pipeline.set_state(Ready)`. This avoids pad-deactivation lock contention seen when opening a project and seeking immediately.
- **UI lock-up risk during paused 3+ track seeks**: Paused preroll/arrival waits are now budget-capped in responsiveness mode, and rebuild fallback second-pass settle is skipped when it would exceed the UI budget. Added seek/rebuild phase timing logs to make remaining stalls diagnosable.
- **Autoplay after project load/new**: Program Monitor reload now suppresses playback resume for full project replacement actions (Toolbar New/Open/Recent and MCP `open_fcpxml`/`create_project`), so loading a project no longer starts playback automatically.
- **Preview frames advancing on project open**: Opening a project caused visible frame advancement in the Program Monitor (without audio) because the compositor playing pulse briefly set the pipeline to Playing to flush preroll. The initial seek after project load now skips the playing pulse entirely and transitions the player state from Stopped to Paused, relying on Paused-state preroll to display the first frame.
- **Transform overlay background playback while dragging**: Starting a transform/crop drag in the Program Monitor now pauses timeline playback immediately, and transform live mode keeps the pipeline paused so the frame does not keep advancing in the background during edits. Releasing the drag still performs the final reseek refresh.
- **Audio-less clips stalling audiomixer**: Clips without an audio stream (e.g. screencasts, image sequences) still had an audiomixer sink pad requested, leaving an unfed pad that could stall the aggregator. Fixed by setting `is-live=true` on the background `audiotestsrc` so the audiomixer operates in live mode (clock-paced, won't wait indefinitely for unlinked pads). Added `probe_has_audio_stream()` using GStreamer Discoverer to detect audio-less clips, and the slot builder now skips the `audioconvert → audiomixer` path entirely for such clips.
- **Third video track not rendered with 3+ active tracks**: When three or more video tracks were active at the playhead, the top-priority track's video was missing from the preview. After the two-pass compositor settle confirmed all decoder buffers had arrived, a redundant `reseek_slot_for_current()` flushed only the top slot's compositor pad, invalidating the compositor's preroll state. The subsequent async playing pulse could not reliably re-aggregate all pads within the 250 ms window. Removed the redundant reseek. Additionally, single-slot reseeks (used by transform drag-end refresh) now flush the compositor and reseek ALL decoder slots so the compositor can produce a complete composited frame from every video track.
- **Transform edits not applied to non-top-track clips**: `update_transform_for_clip` only pushed crop/rotate/flip/scale/position to GStreamer when the edited clip was the highest-priority (top-track) clip. Clips on lower tracks stored model changes but never applied them to the pipeline until a rebuild. The fix removes the `current_idx` guard so transforms are applied to any active slot, and also wires scale/position through `apply_zoom_to_slot` (previously ignored during live edits).
- **No preview refresh after transform overlay drag ends**: Added an `on_drag_end` callback that triggers a paused-frame reseek when the user releases the mouse after dragging crop, scale, or position handles. This ensures the final composited frame reflects the last transform state.
- **Crop not applied during export**: Added ffmpeg `crop` + `pad` filter to the export pipeline so per-clip crop values (left/right/top/bottom) are applied when exporting. Primary-track clips pad cropped regions with black; secondary-track (overlay) clips pad with transparent pixels so lower tracks show through.
- **Cropped clips showing black instead of lower tracks in preview**: Moved the GStreamer `videocrop` element from source resolution (before downscale) to project resolution (after RGBA conversion), and added a `videobox` re-pad step with `border-alpha=0` so cropped areas become transparent. The compositor now correctly reveals lower-track video through cropped regions of upper-track clips.
- **Video 2 (B-Roll) restarting when Video 4 enters/exits during preview playback**: Playback-boundary rebuilds used `KEY_UNIT` decoder seeks in smooth/balanced modes. On long-GOP proxies (e.g. keyframes at 0s and ~10.4s), seeks around 9–10s snapped back to the 0s keyframe, making B-roll appear to restart whenever a track-boundary rebuild occurred. The playing rebuild path now forces `FLUSH|ACCURATE` seeks so all active clips resume from the correct source position across 2→3 and 3→2 track transitions.
- **Clips restart from beginning at track boundaries during playback**: When a new clip entered or left the active set during playback (e.g., Video 4 starting), `rebuild_pipeline_at()` tore down all slots and rebuilt them, but the pipeline remained in Ready state when decoder seeks were issued. GStreamer decoders in Ready state silently reject seeks, so when the pipeline finally transitioned to Playing, decoders prerolled at position 0 — restarting all clips. The fix explicitly transitions the pipeline to Paused and waits for decoder preroll before seeking, ensuring seeks succeed.
- **Frozen compositor output across seeks (multi-track)**: The compositor produced identical (black) frames regardless of playhead position because stale preroll frames from the initial pipeline setup were never cleared. Per-decoder flush seeks reset individual compositor sink pads but left the compositor's aggregation state and downstream buffers untouched. The fix atomically flushes the compositor (`compositor.seek_simple(FLUSH, ZERO)`) before per-decoder seeks, clearing stale preroll so the compositor re-aggregates with fresh decoder buffers. Applied in `seek_slots_in_place`, `reseek_all_slots_for_export`, and the paused settle path of `rebuild_pipeline_at`. Additionally, `wait_for_compositor_arrivals()` spin-waits until post-seek buffers from every decoder slot have actually reached the compositor, preventing races where the playing pulse fires before decoder output propagates through the effects/queue chain.
- **Proxy cache infinite retry loop**: `ProxyCache::request()` re-enqueued failed proxy transcodes on every 250 ms poll cycle, causing 18 proxy requests for 3 clips. A `failed` set now permanently tracks failed transcodes and prevents re-queuing.
- **Proxies not used on initial project open**: Proxy path resolution now happens before `load_clips()` in the same deferred callback, so the first `rebuild_pipeline_at()` uses pre-rendered proxies immediately. Previously, proxy paths were populated in a separate deferred callback that ran after the pipeline was already built with original source files.
- **Program monitor deadlock on project change**: `teardown_slots()` now pre-flushes all compositor/audiomixer sink pads before removing decoder branches. Streaming threads could be blocked in downstream pushes (holding the pad STREAM_LOCK), preventing `set_state(Null)` from deactivating pads. The flush unblocks those threads first, eliminating the `futex_wait` deadlock that froze the UI when reloading clips (e.g. after moving a clip or editing properties during playback).
- **Black preview when scrubbing while paused**: Two fixes for the program monitor showing a black frame instead of the composited image when moving the playhead while paused:
  1. **Fast-path preroll wait**: `seek_slots_in_place` now waits for decoders to preroll *before* the Playing pulse. Flush seeks clear the compositor's input buffers; without this wait the Playing pulse fired before new decoded frames arrived, so the compositor had nothing to aggregate.
  2. **Paused rebuild after project changes**: `on_project_changed` now calls `seek()` when paused (not just when playing) so the pipeline is rebuilt with the correct composited frame. Previously, `load_clips()` tore down all decoder slots but skipped the rebuild when paused, leaving the monitor black until the user manually seeked.
- **Freeze with 3+ video tracks during paused seek**: `wait_for_paused_preroll()` now waits only on decoder elements instead of the full pipeline. `pipeline.state()` blocked the GTK main thread waiting for `gtk4paintablesink` to complete Paused preroll, but the sink needs the main loop for `gdk_paintable_invalidate_contents()` — deadlocking with 3+ tracks where decoding takes long enough that the sink hasn't prerolled before we block. Decoder-only waits avoid this while still ensuring frames are decoded at the compositor inputs; the display sink prerolls asynchronously when control returns to the main loop.
- **Playback stall with 3+ overlapping clips**: `rebuild_pipeline_at()` now waits for decoder re-preroll after flush seeks before entering Playing state. The flush seeks reset the internal multiqueue preroll, so the compositor entered Playing with empty input pads and stalled indefinitely (non-live GstAggregator). Additionally, unlinked decoder pads and failed seeks now send EOS to their compositor/audiomixer pads so the aggregator never waits for buffers that will never arrive.
- **Import-time `gstplaysink` abort mitigation**: Media import and external-drop paths no longer auto-select/auto-load the newly imported clip into the Source Monitor. Loading now happens on explicit user selection, avoiding import-time overlap between Discoverer probing and source `playbin` reconfiguration on problematic media.
- **Source import preview stability**: Hardened source-monitor `playbin` URI reloads by transitioning through `Null` before setting a new URI, and ignoring duplicate same-item selection callbacks. This prevents rapid reconfiguration races that can abort with `gstplaysink` assertions during import/selection.
- **Import abort in new projects (`gstplaysink` assertion)**: Import UI paths were invoking source selection twice (`select_child` callback + an explicit second callback), which could trigger overlapping `playbin` reconfiguration and crash with `gstplaysink.c:1475: try_element: assertion failed: (!element_bus)`. Import now selects the item once and relies on the existing selection callback to load preview media.
- **MCP `import_media` hang after project open**: Importing media through MCP no longer triggers `on_project_changed()`. Importing into the library is now library-only (matching UI behavior), which avoids an unnecessary program-player reload that could stall subsequent MCP commands.
- **Crash/hang during project updates**: `teardown_slots()` now detaches slot branch elements from the pipeline, transitions the removed elements to `Null`, and only then releases compositor/audiomixer request pads. This avoids both the prior race (in-flight audio query hitting a released pad) and the new hang observed when forcing `Null` while branches were still attached.
- **Playhead seek preview**: Seeking the timeline playhead while paused now correctly shows the frame at the target position instead of a black screen or the first frame of the clip. Three complementary fixes:
  1. **In-place seek fast path**: When the same clips are already loaded for the target position, decoders are seeked in-place (no pipeline teardown/rebuild). The previous always-rebuild approach went through GStreamer's `Ready` state (flashing black background) and allowed decoders to preroll at position 0 before the seek was applied (flashing first frame).
  2. **Rebuild path display pulse**: After a full pipeline rebuild (cold start, clip boundary crossing), a brief `Playing` pulse (150 ms) is applied before returning to `Paused`. Without this, the GStreamer compositor holds its composited output until the clock advances; the pulse releases that back-pressure so the frame actually reaches `gtk4paintablesink` and the GTK paintable is updated.
  3. **Fast-path display pulse**: The in-place seek fast path now also performs a `Playing → Paused` pulse after seeking decoders. Per-decoder FLUSH events stop at the compositor's sink pads and are not forwarded downstream, so the display sink stays prerolled with its old frame; the pulse starts the clock briefly to flush the new composited frame through to `gtk4paintablesink`.

### Added
- **About dialog with dependency credits**: Added an **About & Open-source credits** dialog on **Preferences → General** listing core crates/libraries used by UltimateSlice and their license families (including GTK/GStreamer stack, Rust crates, and Flatpak build deps like FFmpeg/x264), plus a license notice section for attribution visibility.
- **Python MCP socket client commands**: Added `tools/mcp_socket_client.py`, a Python stdio↔Unix-socket bridge for MCP, plus a new `.mcp.json` server entry (`ultimate-slice-python-socket`). Added user docs for Python commands in `docs/user/python-mcp.md` and linked usage from `README.md` and Preferences docs.
- **Agent verification rule**: `docs/ARCHITECTURE.md` now explicitly requires MCP completion checks before declaring tasks done: new-project import, existing-project open, and MCP validation of new/modified functionality when feasible.
- **MCP transport controls**: `play`, `pause`, and `stop` commands added to the MCP server, allowing external clients and automation scripts to control program monitor playback.
- **MCP seek and frame export tools**:
  - `seek_playhead` seeks the timeline/program-monitor playhead to an absolute nanosecond position.
  - `export_displayed_frame` writes the current displayed program frame to an image file (PPM/P6), useful for automated visual debugging of seek/playhead behavior.
- **Playback performance optimizations**: Reduced CPU usage by ~26%, peak thread count by ~25%, and peak memory by ~1.5GB during 3-stream HEVC playback:
  - Decoder thread cap: `avdec_h264` limited to 2 threads, `avdec_h265`/`avdec_vp9` limited to 4 threads (was unlimited, defaulting to all CPU cores) during active playback rebuilds. Implemented via `deep-element-added` signal on `uridecodebin`.
  - Multiqueue tuning: GStreamer's internal multiqueue tuning now applies on active-playback rebuilds only (10MB byte cap per slot, unlimited time), while paused scrubbing rebuilds keep default multiqueue behavior for seek safety.
  - Background extraction pause: Thumbnail and waveform extraction is suspended during playback and resumed on pause/stop, eliminating I/O contention from competing `typefind` threads.
  - Scope frame skip: When the Scopes panel is hidden, the appsink frame copy is bypassed entirely (~7MB/s of saved allocations at 30fps).
- **3-Point editing (Insert/Overwrite from Source)**: Professional insert and overwrite edit operations from the source monitor to the timeline.
  - **Insert** (`,`): Places source selection at playhead, shifting all subsequent clips right to make room (ripple insert). Button: ⤵ Insert.
  - **Overwrite** (`.`): Places source selection at playhead, replacing existing material in the time range — clips are trimmed, split, or removed as needed. Button: ⏺ Overwrite.
  - Both operations support full undo/redo via `SetTrackClipsCommand`.
  - MCP tools: `insert_clip` (insert at playhead with ripple) and `overwrite_clip` (overwrite at playhead).
  - Source monitor transport bar now has Insert and Overwrite buttons alongside Append.
- **Slip/Slide edit modes**: New Slip and Slide timeline tools completing the professional edit mode suite alongside Ripple and Roll.
  - **Slip** (`Y`): Drag a clip to shift its source content window (source in/out) without moving the clip on the timeline or changing its duration. Toolbar button: ↔ Slip.
  - **Slide** (`U`): Drag a clip to reposition it on the timeline while neighboring clips' edit points adjust to compensate — total timeline duration stays constant. Toolbar button: ⇔ Slide.
  - Both modes support full undo/redo via `SlipClipCommand` and `SlideClipCommand`.
  - MCP tools: `slip_clip` (shift source window by delta) and `slide_clip` (move clip by delta, adjusting neighbors).
  - Tool indicator overlay on timeline (yellow text) when Slip or Slide mode is active.
  - Keyboard shortcut overlay updated with all edit tool shortcuts (R, E, Y, U).
- **GTK Renderer preference**: New "GTK renderer" setting in Preferences → Playback lets users choose between Auto, Cairo (Software), OpenGL, and Vulkan backends. Cairo mode uses zero GPU memory, resolving `VK_ERROR_OUT_OF_DEVICE_MEMORY` errors on devices with limited GPU memory. Requires application restart. Also exposed via `set_gsk_renderer` MCP tool and included in `get_preferences` response.
- **Preview quality preference**: New "Preview quality" setting in Preferences → Playback scales down the compositor output resolution (Full / Half / Quarter) for smoother preview playback on low-memory devices. Quarter mode renders at 480×270 instead of 1920×1080, using 16× less memory per frame. Takes effect immediately without restart. Also exposed via `set_preview_quality` MCP tool and included in `get_preferences` response.
- **MCP socket transport**: UltimateSlice can now listen on a Unix domain socket (`$XDG_RUNTIME_DIR/ultimateslice-mcp.sock`) so AI agents can connect to an already-running instance. Enabled via Preferences → Integration → Enable MCP socket server. The toggle takes effect immediately without restarting.
- **`--mcp-attach` CLI flag**: A built-in stdio-to-socket proxy that bridges stdin/stdout to the running instance's MCP socket, letting standard MCP clients (which expect stdio) connect via `.mcp.json` `ultimate-slice-attach` entry.
- **Auto preview quality mode**: Preview quality now supports an `Auto` setting that adapts Program Monitor compositor resolution to the current monitor canvas size; manual `Full/Half/Quarter` modes remain available.

### Changed
- **Scopes expand when program monitor is popped out**: The vectorscope, histogram, waveform, and RGB parade panels now expand to fill the available vertical space when the program monitor preview is detached into a separate window. When docked, the scopes retain their compact size below the video preview.
- **Docked Program Monitor/scopes splitter**: In docked mode, the Program Monitor preview and scopes area are now separated by a draggable splitter so users can resize them interactively. When scopes are hidden, the scopes pane is fully removed (no empty split area). The docked split position is persisted across sessions.
- **Batch-sort clips during FCPXML import**: Clips are now appended unsorted during XML parsing and sorted once per track at the end, reducing O(n² log n) sorting overhead on large projects to O(n log n).
- **Parallel proxy transcoding**: `ProxyCache` now uses 4 worker threads instead of 1, transcoding up to 4 proxy files concurrently via ffmpeg.
- **Optimized media library sync**: `on_project_changed` now deduplicates clip source paths before syncing and avoids cloning library paths into a `HashSet<String>`, reducing allocations on every project change.
- **MCP project-open responsiveness**: `open_fcpxml` file-read/parse now runs on a background worker before main-thread apply, parser hot-path allocations were reduced, and proxy-request dedupe was tightened to avoid redundant work while preserving existing project-load behavior.
- **Timeline warm-up load shaping**: Thumbnail/waveform warm-up now uses lower extraction concurrency and lighter thumbnail request density, reducing post-open background thread and memory spikes while preserving timeline thumbnail/waveform functionality.
- **Timeline preview preference**: Added a Timeline setting (`Show timeline preview`) that controls video thumbnail generation strategy. Enabled keeps the full strip behavior; disabled renders only start/end thumbnails per video clip for lower thumbnail workload.

### Added
- **Clip opacity controls**: Added per-clip opacity (`0.0–1.0`) in the Inspector Transform section, plus MCP support via `set_clip_opacity`. Opacity is now included in `list_clips` output and persisted in FCPXML as `us:opacity`.

### Changed
- **Program Monitor compositor rewrite**: Replaced the 3-playbin hot-swap architecture with a single GStreamer pipeline built around `compositor` (video) + `audiomixer` (audio). Each active video clip now gets its own `uridecodebin → effects → compositor` branch with correct z-ordering, per-clip effects, and proper audio boundary handling via seek stop positions. Timeline position is tracked via wall-clock (no seek-anchor heuristics). Eliminates the playhead-freeze, audio-overrun, and 2-layer limit bugs structurally.
- **Program Monitor layered preview**: The monitor now composites the top active video clip over the nearest active lower track, allowing scale/position uncover areas to reveal lower-track video in preview.
- **Export compositing parity**: Secondary-track overlays now use transparent zoom-out padding (`pad ... black@0`) and apply per-clip opacity via ffmpeg `colorchannelmixer=aa=...`, improving preview/export consistency for layered shots.

### Fixed
- **Program monitor paused playhead scrubbing (follow-up)**: Timeline scrubbing keeps deterministic rebuild+seek ordering, with retry-backed decoder seeks, a longer video-pad-link wait window, and paused rebuild ordering that seeks before first preroll. This improves reliability of rendering the frame at the playhead in the preview/transform monitor.
- **Program monitor paused frame repaint (follow-up)**: While paused, the monitor pictures now explicitly queue redraws each poll tick so post-seek paintable updates become visible even when timeline position remains unchanged between timer iterations.
- **Program monitor black frame after seek (follow-up)**: Paused seek now performs a short sink-refresh pass after rebuild (brief play→paused transition with preroll wait) to ensure the program monitor shows decoded clip content instead of a stale black frame.
- **Timeline ruler drag behavior**: Left-drag on the ruler now performs continuous playhead scrubbing (seek updates), while middle/right-drag keeps the existing ruler-pan behavior.
- **Startup hang in paused seek path**: Removed paused-path `decoder.seek(...)` segment seeks (which could deadlock in `gst_element_send_event` during startup/rebuild) and use `seek_simple(FLUSH|ACCURATE)` for paused decoder seeks.
- **MCP playback command hangs (follow-up)**: Starting playback from a cold/stopped state now rebuilds via the playback path, and stop no longer forces a paused seek rebuild. This prevents MCP `play`/`stop` from hanging after project load.
- **Program monitor black frame on paused scrubbing (follow-up)**: In the compositor rebuild path, paused-state transition now happens after all active decoder branches are added, paused seeks use accurate seek flags, and the rebuild waits briefly for dynamic video pad links before seek/preroll settle. This prevents the background preroll path from winning before clip branches are ready, which could leave the Program Monitor and transform overlay stuck on black after timeline playhead moves.
- **Program monitor frame refresh on timeline seek (compositor)**: Paused timeline seeks now use deterministic rebuild+seek+preroll ordering, enforce decoder readiness before seek, and apply a short gap fallback when resolving active clips near clip boundaries. Dragging/clicking the playhead now updates the preview frame reliably and the transform overlay no longer sits over a black frame.
- **Program monitor preview-quality framing**: Reduced preview quality (`Half` / `Quarter`) now scales the fully composed frame to fit the monitor instead of showing a top-left cropped quadrant, and switching quality now forces an immediate pipeline rebuild so caps renegotiation applies cleanly at runtime.
- **GStreamer element disposal warnings on playback end**: Fixed critical warnings ("Trying to dispose element ... but it is in READY instead of the NULL state") that appeared when timeline playback reached the end. Added `PipelineGuard` RAII wrapper that sets temporary GStreamer pipelines to NULL on drop, ensuring proper cleanup even on early-return error paths in waveform extraction, thumbnail extraction, and single-frame capture. Also added state-change waits in `teardown_slots()` so video slot elements fully reach NULL before being dropped.
- **App freeze on early interaction during project load**: Fixed deadlock when interacting with the timeline or transport controls before a project finishes loading. In `rebuild_pipeline_at()`, the pipeline-wide `set_state(Ready)` was called *before* tearing down individual decoder slots. If decoders were mid-transition (still opening files asynchronously), the pipeline state change blocked the GTK main thread waiting for those transitions to complete — while `gtk4paintablesink` needed the main thread to finish its own transition, causing a deadlock. Swapped the order so slots are torn down individually first (setting each decoder to Null), then the pipeline transitions to Ready with only lightweight background sources remaining. Also added empty-clips guards to `play()`, `seek()`, and `stop()` to prevent unnecessary pipeline operations when no project is loaded.
- **App freeze on timeline edit during playback**: Fixed a second deadlock in `teardown_slots()` where setting decoders to Null *before* disconnecting them from the compositor caused the main thread to block on pad locks held by the compositor's streaming thread. Reordered teardown to: (1) remove elements from pipeline (`gst_bin_remove` unlinks pads using only the object lock — safe), (2) release compositor/audiomixer request pads (already unlinked), (3) set to Null (pad deactivation is fast on unlinked pads). This also prevents FLOW_NOT_LINKED errors from reaching the pipeline bus, which could corrupt the pipeline state and produce black video or audio static.
- **Black preview when scrubbing**: The faster teardown (above) eliminated implicit settling time that the old synchronous `set_state(Null)` calls provided. New decoders hadn't reached Paused state when seeks were issued, so seeks were silently ignored. Now `rebuild_pipeline_at` always waits for preroll when paused (scrubbing), only skipping the wait during playback boundary crossings to avoid stutter.
- **Timeline draw performance and loading freeze**: Optimized waveform rendering from O(n) individual `stroke()` calls per pixel to 3 batched strokes per clip (one per color band: green/yellow/red). Added a `loading` guard to the timeline that suppresses click and drag events while a project file is being parsed, preventing interaction before the timeline is ready. Capped concurrent thumbnail and waveform extraction threads at 4 each (previously unlimited — large projects could spawn 200+ simultaneous GStreamer pipelines). Also fixed off-screen clip culling to use actual widget width instead of a hardcoded 4000px limit.
- **Thumbnail/waveform cache deadlock**: Fixed a bug where failed background extraction threads (e.g., corrupted file, missing audio stream) never signalled completion, permanently consuming a concurrency slot. After 4 failures, both caches would stall entirely — no new thumbnails or waveforms could load for the rest of the session. Extraction threads now always signal completion regardless of success or failure.
- **Excessive per-frame waveform computation**: Waveform peak resampling (`get_peaks`) and Cairo path building were computed for the *full* clip width (potentially 60,000+ pixels for long clips) every frame, even when only ~1,000 pixels were visible. Now only the visible portion of each clip's waveform is computed and drawn, reducing per-frame work by 10–60× for scrolled or zoomed-out timelines.
- **Program monitor playhead freeze (compositor sync)**: Fixed compositor-based pipeline deadlocking during mid-playback clip boundary rebuilds. The always-on `videotestsrc` background accumulated running-time while newly-created decoders started at running-time 0 after flush-seek, causing the compositor to wait for decoders to catch up. Now the pipeline transitions through Ready state during rebuilds to reset the running-time base. Also fixed `audioconvert` element leak — per-slot audioconvert elements were not tracked or cleaned up during slot teardown, causing orphaned elements to accumulate across rebuilds.
- **Program monitor playhead freeze with 3+ video tracks** *(pre-compositor; superseded by compositor rewrite)*: Previously fixed in the old 3-playbin architecture by prerolling through PAUSED before seeking. Root cause was stale `query_position` values after pipeline hot-swap. Now structurally eliminated by the compositor rewrite which uses wall-clock position tracking instead of GStreamer position queries.
- **Export overlay transparency with 3+ video tracks**: Fixed secondary-track overlay clips with zoom-out (scale < 1.0) rendering opaque black borders instead of transparent padding during export. The `format=yuva420p` conversion was applied after the scale/position pad filter, so `black@0` had no alpha channel to work with. Reordered the filter chain so color/denoise/sharpen/LUT effects run in yuv420p first, then format conversion to yuva420p occurs before scale/position padding, making overlay borders truly transparent and revealing lower tracks beneath.
- **Program Monitor PiP live preview**: Made the top Program Monitor layer background transparent so per-pixel alpha from scale/position transforms can reveal the lower active video track during live preview.
- **Program Monitor regression fixes**: Restored +/-/Fit zoom behavior when baseline canvas size is not yet available by falling back to scroll-viewport/project dimensions, and improved PiP preview reveal by forcing transparent `videobox` borders in the zoom/position chain so lower-track underlay video can show through uncovered regions.
- **Program Monitor layered zoom alignment**: Both monitor layer pictures now force `halign/valign=Fill` and share the same preview CSS class, so B-roll/underlay content follows the same +/-/Fit zoom geometry as the primary layer while clips are moved.
- **Program Monitor underlay zoom-out floor**: Enabled `GtkPicture::set_can_shrink(true)` on both monitor layers so the underlay/B-roll layer can scale below 100% together with the main layer.
- **Program Monitor underlay zoom-out floor (follow-up)**: Set both monitor pictures to a minimal size request and excluded the top overlay picture from size measurement, preventing the underlay layer from clamping near 75% when zooming to 50%/25%.
- **Program monitor transform refresh**: Scale/Position edits now reliably refresh the
  active preview clip by syncing ProgramPlayer's cached clip transform state and
  forcing an immediate in-place re-seek of the current segment. This prevents stale
  framing where black bars could remain visible in preview/playback even though the
  transform overlay and inspector values were updated. Program Monitor now also keeps
  the scale/position chain active when `gaussianblur` is unavailable by inserting an
  identity stage instead of dropping back to a color-only filter, and enforces
  square-pixel (`pixel-aspect-ratio=1/1`) caps in the zoom chain so wide-source clips
  don't retain display-aspect letterboxing after scaling.
- **Accurate canvas preview**: The Program Monitor now constrains the video display
  area to the project's canvas aspect ratio (e.g. 16:9). Previously, clips whose
  native resolution differed from the canvas (e.g. a 21:9 source on a 16:9 canvas)
  filled the preview without letterbox bars, making it hard to judge placement and
  scale. Now the preview matches the export output: a 21:9 clip on a 16:9 canvas
  will show black bars in the program monitor, exactly as it appears in the exported
  video. The canvas ratio updates automatically when project settings change.
- **Transform overlay syncs with inspector sliders**: Adjusting the Scale, Position X,
  or Position Y sliders in the inspector now immediately updates the transform overlay
  handles in the program monitor. Previously the handles only moved when dragged
  directly in the monitor.
- **Transform handles visible outside canvas**: The transform overlay DrawingArea is now
  placed on an outer overlay that covers the full scroll viewport, rather than being
  confined inside the canvas AspectFrame. When a clip is scaled > 1× (zoomed in so the
  clip extends beyond the canvas), the bounding-box handles are visible when the user
  zooms out the program monitor (using the −/Fit buttons or Ctrl+Scroll). The canvas
  boundary overlay alignment is unchanged: `video_rect()` and `AspectFrame` use the
  same letterbox geometry, so the drawn canvas border stays pixel-accurate.

### Added
- **Zoomable program monitor**: Preview can now be zoomed in/out independently of
  clip scale. Use the **−/+** buttons or **Fit** in the program monitor title bar,
  or **Ctrl+Scroll** on the preview. Zoom levels: 25%–400%. When zoomed > 100%,
  scrollbars appear so you can pan to see content outside the canvas boundary (useful
  when working with clips scaled > 1× in the transform overlay).
- **Canvas border vignette**: A dark semi-transparent overlay now fills the areas
  outside the canvas boundary in the program monitor, making it immediately clear
  what will be included in the exported video. The yellow canvas border (shadow +
  accent + corner L-marks) is always drawn when a clip is selected; the white dashed
  clip bounding box only appears when the clip doesn't fill the canvas exactly
  (scale ≠ 1 or position ≠ 0), eliminating the visual confusion where both rects
  appeared identical.
- **Interactive transform overlay**: When a clip is selected on the timeline, a
  transparent overlay appears on the program monitor showing the clip's bounding box
  and corner handles. Drag a **corner handle** to change the clip's zoom scale; drag
  **inside the video frame** to pan the position X/Y. Inspector sliders update in real
  time during the drag without triggering a full pipeline reload. Visual elements:
  yellow output-frame outline, white dashed clip bounding box, blue-ringed corner
  handles, center dot, and a scale label (e.g. "1.50×") with a dark background pill.
- **Scale / Position per clip**: Inspector Transform section now has Scale (0.1–4.0),
  Position X (−1 to 1), and Position Y (−1 to 1) sliders. Scale > 1 zooms into the
  clip (crops the frame); scale < 1 shrinks the clip with black letterbox/pillarbox.
  Position X/Y shifts the viewport within the zoomed or shrunk frame. Applied in the
  program monitor via a GStreamer `videoscale` + `videobox` chain appended to the
  existing filter bin, and on export via ffmpeg `scale`+`crop`/`pad` filters. Settings
  are saved to project JSON and round-trip through FCPXML (`us:scale`, `us:position-x`,
  `us:position-y`). MCP server exposes a new `set_clip_transform` tool.
- **`create_project` MCP tool**: Discards the current project and creates a new empty one.
  Accepts an optional `title` parameter (defaults to "Untitled"). Resets playhead,
  scroll, zoom, selection, and undo history. Mirrors the "New" toolbar button behaviour.
- **Flatpak cargo-sources**: Added `cargo-sources.json` (generated by
  `flatpak-cargo-generator.py`) to the flatpak manifest, enabling fully offline Rust
  builds inside the sandbox. Updated `io.github.kmwallio.ultimateslice.yml` to use
  `cargo --offline fetch` with the pre-generated sources.
- **`.mcp.json`**: Added `create_project` tool entry so AI agents can reset the project
  state without restarting the server.

### Changed
- `validate_mcp_transition.py` now uses the installed Flatpak
  (`flatpak run io.github.ultimateslice --mcp`) instead of the debug binary, and
  tests `create_project` as the first operation before adding clips.

- **Transition effects — Fade to black, Wipe right, Wipe left**: Three new transition
  types added to the transitions pane alongside Cross-dissolve. Drag any transition to a
  clip boundary to apply it. Preview uses dual-pipeline opacity blending (Fade to black
  has its own curve: outgoing fades to black then incoming fades up; wipes approximate
  with crossfade in preview). Export uses the correct ffmpeg `xfade` filter:
  `fadeblack`, `wiperight`, `wipeleft`. MCP `set_transition` updated to accept all
  four kinds.
- **J/K/L shuttle scrubbing**: Global keyboard shortcuts for shuttle control of the
  program monitor. `L` plays forward and each subsequent press doubles the speed
  (1×→2×→4×→8×); `J` plays in reverse (−1×→−2×→−4×→−8×); `K` pauses and resets
  speed. Shuttle rate is shown in the program monitor title bar ("▶▶ 2×" / "◀◀ 4×").
  Reverse playback uses GStreamer negative-rate seeks (graceful fallback on
  unsupported formats). Space/Stop always resets the JKL state. No focus needed —
  J/K/L work from anywhere in the main window (captured at window level, same
  pattern as the M-key marker shortcut).
- **Colour scopes panel**: A new collapsible panel below the program monitor provides
  four professional analysis tools — Waveform, Histogram, RGB Parade, and Vectorscope.
  Toggle with the "▾ Scopes" button. Frames are captured from a `tee`-based GStreamer
  sink bin (320×180 RGBA) added to the main pipeline; scope rendering is Cairo-drawn
  on the GTK main thread via the existing 33 ms poll timer.  No additional threads or
  blocking pipeline waits are introduced.
- **Cross-dissolve transitions — FCPXML persistence**: Transition metadata
  (`transition_after` kind and `transition_after_ns` duration) is now written
  as `us:transition-after` / `us:transition-after-ns` vendor attributes on
  `<asset-clip>` elements in the FCPXML writer, and parsed back on project
  load. Transitions set via the drag-and-drop transitions pane (or MCP
  `set_transition` tool) now survive save/load round-trips. Preview blending
  (opacity crossfade via dual GStreamer pipelines) and ffmpeg `xfade` export
  were already functional; this completes the end-to-end feature.

### Fixed
- **Media buttons (Append, Set In, Set Out) broken after first import**:
  - When a file is first imported, `duration_ns = 0` because the ffprobe runs in the background.
    `on_source_selected` set `source_marks.out_ns = 0`, which caused Append to create a
    zero-duration clip (silent no-op) and Set In to always clamp to zero regardless of
    scrubber position.
  - Clicking away and back on the item worked because by then the probe had completed and
    the correct duration was used.
  - Fix: the 100ms preview poll timer (which already queries `p.duration()` for the timecode
    label) now syncs `source_marks.duration_ns` and `source_marks.out_ns` from the player
    the first time a valid duration becomes available. The player pipeline prerolls in
    ~100–300ms, well before the user can click any buttons.
- **Proxy media not used on startup / LUT invisible in preview**:
  - `on_project_changed` called `cache.request()` (which synchronously adds disk-cached
    proxies to the in-memory map) but never pushed the result to the player.
    The player's `proxy_paths` map stayed empty so every clip fell back to its source
    file. Proxy paths are now forwarded to the player immediately after the request loop.
  - The 500 ms proxy poll timer only called `update_proxy_paths` when a background
    transcode completed (`resolved` non-empty). Synchronously-cached disk proxies were
    never reflected in `resolved`, so the player never learned about them. The timer now
    syncs proxy paths whenever the cache is non-empty and proxy mode is enabled.
  - Because the LUT is baked into the proxy file during ffmpeg transcode, the above fix
    also restores LUT visibility: once the correct LUT-baked proxy is used, the LUT is
    visible in the program monitor preview.
- **Playback stops on second (or later) cross-dissolve when clips have a 1-frame gap**:
  - `ns_to_fcpxml_time` uses integer frame-count division (floor), so clip positions
    can be off by 1 frame (≈41 ms at 24 fps) after an FCPXML save/load round-trip.
    `clip_at()` used exact `[start, end)` bounds, so a gap of even 1 ns between clip B
    and clip C caused the handoff to return `None` → the player treated it as
    end-of-timeline and stopped.
  - `activate_transition()` used `c.timeline_start_ns == clip_timeline_end_ns` (exact
    equality) to find the incoming clip — also broken by sub-frame gaps.
  - Fix: `clip_at()` now has a fallback that bridges gaps up to 100 ms ahead (≥2 frames
    at 24 fps) by finding the next-earliest clip starting in that window. This is safe
    for all existing call-sites (scrubbing, seeking, handoff detection).
  - `activate_transition()` incoming-clip search changed to a range check:
    `start_ns ∈ [clip_end_ns, clip_end_ns + 100 ms]`.
  - `transition_opacities()` now gates on `transition_active` so picture_b is never
    made partially visible when pipeline2 is not actually running.
- **Choppy playback around transitions**:
  - `activate_transition()` previously called `pipeline2.state(120ms)` — a blocking
    wait on the GTK UI thread. Because `poll()` runs from a 33ms GTK timeout, one
    120ms block dropped ~4 frames and caused a visible stutter at the start of every
    cross-dissolve.
  - Fix: removed the blocking wait. `activate_transition` now sets pipeline2 directly
    to `Playing` and records `pipeline2_pending_seek_ns`. The `seek_simple()` to
    `source_in_ns` is issued on the very next `poll()` tick (33ms later) by which
    point the pipeline is ready — zero UI thread blocking.
- **Cross-dissolve reverts to previous clip after transition**:
  - `load_clip_idx` applied `transition_alpha()` to the GStreamer `alpha` filter when
    the incoming clip loaded. At the clip boundary `timeline_pos = clip_B.timeline_start_ns`,
    `t = 0` → `alpha = 0.0`. GStreamer pipeline1 became fully transparent; GTK4 Picture
    retained clip A's last frame, appearing as if playback rewound.
  - Fix: `load_clip_idx` now always sets `alpha_filter.alpha = 1.0`. Cross-dissolve
    blending is handled entirely by `picture_a.set_opacity()` / `picture_b.set_opacity()`
    in the 33ms poll timer.

  - The seek flags used during scrubbing/paused seeks were `KEY_UNIT` (Smooth/Balanced
    priority), which snaps to the nearest keyframe before the playhead. For H.264 media
    with long GOP intervals this could display a frame seconds away from the actual
    playhead. Paused seeks now always use `ACCURATE` flags so the exact frame is decoded.
  - In **Smooth** playback priority mode, the preroll wait before seeking to a different
    source file was skipped. This caused the seek to be issued before the pipeline reached
    PAUSED, which GStreamer silently ignores, leaving the preview at frame 0 of the new
    clip. The preroll wait is now unconditional when not playing (150 ms cap).
  - The same `KEY_UNIT` bug also affected the Inspector's crop/rotation/flip controls
    and the title overlay position—adjusting these while paused would seek to the wrong
    frame. Fixed to use `ACCURATE` seeks in all non-playing seeks.
  - Inspector color/effects sliders (brightness, contrast, saturation, denoise,
    sharpness) did not visually update the preview when adjusted while paused. The
    GStreamer pipeline filter properties were updated correctly but the `KEY_UNIT` seek
    used to force a frame redraw could be a no-op when already at a keyframe boundary,
    leaving the preview stale. Fixed to use `ACCURATE` seeks so the current frame is
    always re-decoded with the new filter values applied.

### Added
- **True cross-dissolve preview in program monitor**:
  - The program monitor now uses a dual-pipeline architecture: a second lightweight
    `playbin` (pipeline2) feeds an independent `gtk4paintablesink`. Both pipelines are
    composited by a `GtkOverlay` with two `Picture` widgets whose `opacity` is updated
    every 33 ms to produce a genuine cross-dissolve (picture_a fades out, picture_b
    fades in) rather than the previous "dip to black" approximation.
  - During the transition window (final `d` ns of the outgoing clip), pipeline2 loads
    the incoming clip, seeks to its `source_in_ns`, and plays. After the window closes,
    pipeline2 is stopped and opacities reset to (1.0, 0.0).
- **Audio level meters (VU meter) in program monitor**:
  - A GStreamer `level` element is inserted in the audio filter chain of both the main
    video pipeline (`audiopanorama → audioconvert → level`) and the dedicated audio-only
    pipeline. The element posts peak/RMS values (dBFS) to the bus every 50 ms.
  - Peak values are read in the 33 ms poll timer alongside EOS detection (consolidated
    into `poll_bus()`), so no bus messages are discarded.
  - A Cairo `DrawingArea` VU meter is displayed in the program monitor title bar showing
    L/R channel peaks with three zones: green (< −18 dBFS), yellow (−18 to −6 dBFS),
    red (> −6 dBFS). The meter decays at ~3 dB per frame toward −60 dBFS when audio
    is silent.
  - VU meter updates when the playhead is seeked (paused preroll triggers a level message)
    and when the volume slider is adjusted while paused (force re-seek causes a new preroll).
- **Collapsible inspector sections**:
  - Each inspector section (Color & Denoise, Audio, Transform, Title Overlay, Speed, Color LUT) now has a `gtk4::Expander` disclosure triangle that collapses or expands its content.
  - Color, Audio, Transform, and Speed sections default to expanded; Title Overlay and Color LUT default to collapsed to reduce visual noise.
  - Metadata (clip name, source, timecodes) remains always visible.
- **Inspector: disable when empty, context-sensitive sections**:
  - The inspector panel is now grayed out (insensitive) when no clip is selected,
    providing a clear visual signal that there is nothing to edit.
  - Each inspector section is shown or hidden based on the selected clip's type:
    - **Audio clips**: only Audio (volume, pan) and Speed sections are visible.
    - **Image clips**: Color, Transform, Title Overlay, Speed, and LUT sections shown; no Audio section.
    - **Video clips**: all sections visible.
- **Color-coded audio waveforms on timeline**:
  - Timeline waveform bars are now colored per-amplitude: green (quiet, < −18 dBFS),
    yellow (moderate, −18 to −6 dBFS), red (loud, > −6 dBFS). Zones match the VU meter.
  - Applies to both audio-track waveforms and the new video-clip waveform overlay.
- **Waveform overlay on video clips (Preferences → Timeline)**:
  - New preference `Show audio waveforms on video clips` (Preferences → Timeline section).
  - When enabled, the audio waveform is drawn on the lower ~40% of each video clip tile
    with a semi-transparent dark backing so thumbnails remain visible above.
  - Color-coded using the same green/yellow/red amplitude zones.
  - Preference is persisted to disk (`show_waveform_on_video` in `ui-state.json`).
- **LUT import / apply (per clip)**:
  - Added `lut_path: Option<String>` field to the `Clip` model for storing the path to a `.cube` LUT file.
  - Inspector panel now has a **Color LUT** section with an **Import LUT…** file chooser (filtered to `.cube`) and a **Clear** button. The assigned LUT filename is displayed; a note clarifies "Applied on export (.cube)".
  - On export, FFmpeg's `lut3d` filter is applied to each clip's video filter chain when a LUT is assigned. Applies to both primary and secondary (overlay) video tracks.
  - A cyan **LUT** badge is rendered on timeline clips with an assigned LUT (analogous to the speed badge).
  - FCPXML round-trip: `us:lut-path` attribute is written on export and read on import.
  - MCP tool `set_clip_lut` added: accepts `clip_id` and optional `lut_path` (string or null to clear).
- **Program monitor playback priority**:
  - Added a persisted Playback preference for program-monitor priority: `Smooth`, `Balanced`, `Accurate`.
  - `Smooth` now prioritizes playback continuity (reduced blocking/preroll pressure during active playback).
  - Added MCP tool `set_playback_priority`; `get_preferences` now includes `playback_priority`.
  - Program monitor timeline redraws are now coalesced during playback to reduce UI pressure.
- **Proxy preview mode**:
  - Background proxy transcoding: generates lightweight half- or quarter-resolution H.264 proxy files via ffmpeg for smoother preview playback with heavy/4K media.
  - Added `ProxyMode` preference (`Off`, `Half Res`, `Quarter Res`) in Preferences → Playback.
  - Proxy files stored in `UltimateSlice.cache/` next to source files; export always uses originals.
  - Added MCP tool `set_proxy_mode`; `get_preferences` now includes `proxy_mode`.
  - Yellow progress bar status bar at bottom of window shows proxy generation progress.
  - **Bug fix**: Changing the proxy size (half ↔ quarter) in Preferences now invalidates existing proxies and re-generates them at the new resolution. Previously the old-resolution proxy was reused.
  - **Improvement**: Proxy filenames now encode the scale and LUT assignment (e.g. `clip.proxy_half_lut1a2b3c4d.mp4`) so clips with different scales or LUTs each get their own distinct proxy file.
  - **New**: When a LUT is assigned to or cleared from a clip, the proxy cache is invalidated and a new LUT-baked proxy is generated for that clip's source, allowing accurate preview of the color grade without waiting for export.
- **Reduced black flash on clip switches**:
  - During active playback, clip source changes no longer drop the pipeline to Ready state, avoiding the visible black frame flash between clips.
- **Background threading for media import**: `MediaProbeCache` (`src/media/probe_cache.rs`) moves
  GStreamer Discoverer duration and audio-only probing off the main thread. Media files are added
  to the library instantly; duration and type are filled in asynchronously via the existing 250 ms
  polling timer. Eliminates ~5 s UI freeze per imported file.
- **Background threading for project open**: Open and Recent-file handlers in `toolbar.rs` now
  read and parse FCPXML on a background `std::thread`, polling for the result on the main thread
  via a 50 ms `glib::timeout_add_local` timer. Eliminates UI freeze during project load.
- `MediaItem.is_audio_only` field — cached from background probe, used by `on_source_selected`
  to avoid a blocking `probe_is_audio_only` call when selecting a library item.

### Added
- **Advanced Editing Tools**:
  - **Ripple Edit Tool**:
    - Added `Ripple` tool to the toolbar (shortcut `R`).
    - **Ripple Trim Out**: Dragging the right edge of a clip shifts all subsequent clips on the track.
    - **Ripple Trim In**: Dragging the left edge of a clip shifts all subsequent clips on the track (preserving gapless sequence).
    - Undo/Redo fully supported for ripple operations.
  - **Roll Edit Tool**:
    - Added `Roll` tool to the toolbar (shortcut `E`).
    - Dragging the boundary between two adjacent clips adjusts the cut point.
    - Left clip's out-point and right clip's in-point are adjusted simultaneously.
    - Total duration of the sequence remains constant; subsequent clips do not move.
    - Undo/Redo fully supported.
- **Transitions pane (v1)**:
  - Added a right-sidebar **Transitions** pane below the Inspector with a hide/show toggle.
  - Added a draggable **Cross-dissolve** transition item so future transitions can be added to the same pane.
  - Dragging the transition onto a clip boundary in the timeline applies a transition marker (undoable).
  - Right-clicking a transition marker now removes that transition (undoable).
  - Export now applies cross-dissolve transitions on the primary video track using ffmpeg `xfade`.
  - Fixed transition export filter generation (resolved ffmpeg “Filter not found” parse errors).
  - Program preview reuses loaded source segments for same-file clip handoffs when possible.
  - Program preview now applies transition alpha ramps around cross-dissolve boundaries.
  - Added MCP tool `set_transition` to automate transition add/remove operations.
  - While dragging a transition into the timeline, the two target clips are now highlighted before drop.
  - Fixed transition-hover preview so clip-pair highlighting updates correctly during drag motion.
- **Undoable track add/remove**:
  - Adding and removing tracks now goes through the undo system (`Ctrl+Z` restores a deleted track with all its clips).
- **Media import improvements**:
  - Import dialog now supports selecting and importing multiple files in one action.
  - Media Library now accepts external file drops (drag files from file manager into the pane to import).
- **Playback control isolation**:
  - Timeline/program **Play** controls no longer start or pause Source Monitor playback.
- **Active track highlighting**:
  - Clicking anywhere in a track row (including empty space) selects it as the active track.
  - The active track's label area shows a blue left-edge accent bar and brighter background.
- **Smart Append**:
  - The Append button now detects whether the source media is audio-only (via GStreamer Discoverer).
  - Audio files append to the active audio track (or first audio track); video files to the active video track (or first video track).
- **Remove Track targets active track**:
  - The Remove Track button removes the currently highlighted active track instead of always the last one.
  - Selection is cleared after removal to prevent stale references.
- **Cross-track clip dragging**:
  - Clips can now be dragged vertically between tracks of the same kind (video → video, audio → audio).
  - Undo/redo fully supported for cross-track moves (including magnetic mode).
  - Snapping behaviour works across tracks.
- **Track reordering**:
  - Drag a track label vertically to reorder tracks in the timeline.
  - A blue drop indicator shows the target position during the drag.
  - Undoable via `Ctrl+Z`.
  - Added MCP tool `reorder_track` for automation.
- **Timeline filmstrip thumbnails now follow clip time**:
  - Video clip thumbnail strips now sample frames across the clip instead of repeating a single frame.
  - Each tile maps to its corresponding position between clip `source_in` and `source_out`, so trims/in-out changes are reflected in the strip.
  - Rendering keeps async `ThumbnailCache` extraction and uses a per-clip tile cap to avoid excessive request churn.
- **Preferences window + `Ctrl+,` shortcut**:
  - Added a categorized Preferences dialog (General, Playback) opened with **`Ctrl+,`**.
  - Added an initial **Hardware Acceleration** preference toggle in Preferences.
  - Preferences are persisted in `~/.config/ultimateslice/ui-state.json`.
  - Added MCP tools `get_preferences` and `set_hardware_acceleration` for automation.
  - Hardware acceleration now applies immediately to source preview playback sink selection (with fallback support); export behavior is unchanged.
- **Source preview close action**:
  - Added a close (`✕`) button in the Source Monitor header.
  - Clicking close now deselects the active media-library item, hides the source preview panel, stops source playback, and resets source marks/timecode state.
  - Added MCP tool `close_source_preview` for automating the same behavior.
- **Magnetic timeline mode (gap-free, track-local)**:
  - Added a **Magnetic** toggle in the toolbar to enable/disable gap-free timeline behavior.
  - When enabled, edits compact the affected track so clips remain contiguous after delete/move/trim and timeline insertions (append/drop/MCP clip edits).
  - Timeline overlay now shows a magnetic-mode indicator when active.
  - Added MCP tools `set_magnetic_mode` and `get_timeline_settings` for automation and verification.
- **Recent projects list**: Last 10 opened/saved projects are persisted in `~/.config/ultimateslice/recent.json`. A new **Recent ▾** button in the toolbar shows a popover with file names; clicking any entry opens that FCPXML immediately.
- **Per-clip speed change**:
  - New **Speed** section in the Inspector with a slider (0.25×–4.0×) and marks at ½×, 1×, 2×.
  - Changing speed updates `clip.speed` in the model immediately; the slider fires `on_project_changed` so the timeline clip width and program player both update.
  - `Clip::duration()` now returns timeline duration (`source_duration / speed`); `source_duration()` helper returns raw source material length.
  - GStreamer preview: `pipeline.seek(rate, ...)` with `rate = clip.speed` so the program monitor plays at the correct speed.
  - `poll()` converts GStreamer source position back to timeline position accounting for speed.
  - ffmpeg export: video filter gets `setpts=PTS/{speed}`, audio gets a chained `atempo` filter (handles full 0.25–4.0 range by splitting into ≤2.0 steps), input `-t` uses `source_duration` so the full source material is read.
  - Yellow speed badge (e.g. "2×") drawn on the clip in the timeline when speed ≠ 1.0.
  - FCPXML persistence via `us:speed` attribute.
- **Project Settings dialog** (`⚙ Settings` button in toolbar)
- **Advanced Export dialog** (replaces "Export MP4…")

### Fixed
- **Timeline scrubber position preservation**: `on_project_changed` now saves the current playhead position before rebuilding the program monitor clip list and restores it via a seek afterward, preventing the playhead from jumping to 0:00 on every project change (clip rename, color adjustment, etc.).
- **Inspector callbacks wired correctly**: `build_inspector` was previously called with an empty `|| {}` closure before `on_project_changed` was defined; it is now called after, and receives the real callback so clip name changes trigger proper UI updates.
- **Color sliders update preview live**: Color slider changes now call `prog_player.update_current_color()` directly (sets GStreamer `videobalance` properties + issues a flush seek to force frame redecode) rather than routing through the full `load_clips` pipeline reset, giving instant visual feedback without position loss.
- **Same-clip seek optimization in ProgramPlayer**: `load_clip_idx` now detects when the requested clip is already loaded and performs a lightweight seek instead of a full pipeline teardown, making scrubbing within a single clip fast and reliable.
- **`list_clips` MCP response now includes color fields**: `brightness`, `contrast`, `saturation` are included alongside other clip properties.

### Added
- **Titles / text overlay per clip**:
  - New **Title Overlay** section in the Inspector panel: text entry, X/Y position sliders (0.0–1.0 relative).
  - GStreamer `textoverlay` element injected at the end of the video filter bin; hidden (`silent=true`) when text is empty.
  - Default font `Sans Bold 36`, default color white, default position bottom-center (x=0.5, y=0.9).
  - Changes apply live to the program monitor without pipeline reload.
  - FCPXML persistence via `us:title-text`, `us:title-font`, `us:title-color`, `us:title-x`, `us:title-y` attributes.
- **Timeline markers / chapter points**:
  - Press `M` at any playhead position to drop a colored marker on the timeline ruler.
  - Markers are drawn as filled triangles with a vertical line and label; default color is orange (0xFF8C00FF).
  - Right-click anywhere on the ruler to remove the nearest marker within 8 px.
  - Markers persist across save/load via FCPXML (written as `<marker>` elements with custom `us:color` attribute).
  - Keyboard shortcut dialog updated to list `M` and right-click-ruler actions.
- **Snap-to-clip-edge when trimming**: `TrimIn` and `TrimOut` drag operations now snap to nearby clip edges (start/end of any other clip) within a 10 px threshold, matching the existing snap behavior for clip moves.
- **Volume and Pan per clip**:
  - Added `volume: f32` (0.0–2.0, default 1.0) and `pan: f32` (−1.0–1.0, default 0.0) fields to `Clip` model with `#[serde(default)]`.
  - Inspector: new **Audio** section with **Volume** and **Pan** sliders that update the program monitor live via `update_current_audio()` (sets `playbin` volume property and `audiopanorama` element).
  - GStreamer: `audiopanorama` element injected as `audio-filter` on `playbin`; per-clip pan applied in `load_clip_idx` alongside existing volume.
  - FCPXML persistence: `us:volume` and `us:pan` custom attributes written/read in writer/parser for lossless round-trip.
- **Auto-save**: 60-second timer saves the project to `/tmp/ultimateslice-autosave.fcpxml` when the project is dirty. Window title briefly shows "(Auto-saved)" for 3 seconds then restores the dirty indicator.
- **Denoise and Sharpness per clip**:
  - Added `denoise: f32` (0.0–1.0, default 0.0) and `sharpness: f32` (-1.0–1.0, default 0.0) fields to `Clip` model with `#[serde(default)]`.
  - GStreamer preview: upgraded video-filter from a single `videobalance` element to a bin `videobalance ! videoconvert ! gaussianblur`. Positive sigma = denoise/blur; negative sigma = sharpen. Combined sigma = `denoise * 4 − sharpness * 6`.
  - Inspector: two new sliders — **Denoise** (0.0–1.0) and **Sharpness** (−1.0–1.0) — in a new "Denoise / Sharpness" section below Color. Sliders update the preview live via `update_current_effects` without a pipeline reload.
  - Export (ffmpeg): `hqdn3d` filter added per-clip when `denoise > 0`; `unsharp` filter added when `sharpness ≠ 0`, chained after the existing `eq` color filter.
  - MCP `set_clip_color` tool extended with optional `denoise` and `sharpness` parameters; `list_clips` response includes both new fields.
  - Added `denoise: f32` (0.0–1.0, default 0.0) and `sharpness: f32` (-1.0–1.0, default 0.0) fields to `Clip` model with `#[serde(default)]`.
  - GStreamer preview: upgraded video-filter from a single `videobalance` element to a bin `videobalance ! videoconvert ! gaussianblur`. Positive sigma = denoise/blur; negative sigma = sharpen. Combined sigma = `denoise * 4 − sharpness * 6`.
  - Inspector: two new sliders — **Denoise** (0.0–1.0) and **Sharpness** (−1.0–1.0) — in a new "Denoise / Sharpness" section below Color. Sliders update the preview live via `update_current_effects` without a pipeline reload.
  - Export (ffmpeg): `hqdn3d` filter added per-clip when `denoise > 0`; `unsharp` filter added when `sharpness ≠ 0`, chained after the existing `eq` color filter.
  - MCP `set_clip_color` tool extended with optional `denoise` and `sharpness` parameters; `list_clips` response includes both new fields.
- Basic color correction per clip (brightness / contrast / saturation):
  - Added `brightness` (f32, default 0.0), `contrast` (f32, default 1.0), `saturation` (f32, default 1.0) fields to `Clip` model with `#[serde(default)]` so existing FCPXML/save files load without change.
  - Inspector panel: new "Color" section with three horizontal `Scale` sliders (brightness −1→1, contrast 0→2, saturation 0→2). Sliders update the clip live and trigger project-changed; feedback loop prevented by an `updating` flag during programmatic value set.
  - Playback: `Player::set_color()` applies a GStreamer `videobalance` element injected via `playbin`'s `video-filter` property. Program monitor (`ProgramPlayer`) applies per-clip color when loading each clip during timeline playback.
  - Export: ffmpeg `eq` filter inserted into the per-clip video filter chain (`scale/pad/setsar/fps/format,eq=…`) when color values differ from neutral; neutral clips skip the filter to avoid no-op overhead.
  - `SetClipColor` EditCommand added to `undo.rs` (reversible).
  - MCP tool `set_clip_color` added: accepts `clip_id`, `brightness`, `contrast`, `saturation`; updates clip in place and fires `on_project_changed`.

- Source Monitor — frame-accurate jog/shuttle control:
  - Frame step forward/backward buttons (◀▮ / ▮▶) in source monitor transport bar.
  - Left/Right arrow keyboard shortcuts for single-frame stepping.
  - J/K/L keyboard shortcuts for shuttle reverse/pause/forward at increasing speeds (1×, 2×, 4×).
  - Frame-accurate seeking via new `Player::seek_accurate()` (uses GStreamer `ACCURATE` flag).
  - `Player::step_forward()` / `step_backward()` methods for frame-level navigation.
  - Frame-accurate timecode display (`H:MM:SS:FF`) in position/duration label.
- Source Monitor — dedicated mark-in / mark-out timecode bar:
  - New styled `.marks-bar` showing In, Out, and Duration timecodes with frame accuracy.
  - In-point (green), Out-point (orange), and Duration labels with monospace font.
  - `SourceMarks.frame_ns` field for configurable frame duration (defaults to 24 fps).
- MCP `export_mp4` tool:
  - Added `McpCommand::ExportMp4` and MCP tool schema/dispatch (`export_mp4`).
  - Added main-thread handler in `window.rs` to run export in a background worker and return JSON results.
- Agent workflow rule:
  - Added instruction that new user-facing features should also be added to MCP (when automatable and not already exposed).

### Fixed
- MP4 export audio tracks:
  - Export previously produced silent video due to `-an` flag, `a=0` in concat filter, and audio tracks never being consulted.
  - Fixed: embedded audio from `ClipKind::Video` clips is extracted via `[i:a]adelay=DELAY:all=1` and mixed with `amix`.
  - Audio-only clips from dedicated audio tracks are also included and positioned at their timeline offsets.
  - `ClipKind::Image` clips and video clips without an audio stream (detected via `ffprobe`) are safely skipped.
  - Output is encoded as AAC 192 kbps stereo alongside the existing H.264 video stream.
  - Fixed missing `;` separator between the last `adelay` output label and the `amix` input list in the filter complex string (caused ffmpeg EINVAL / exit 234).
  - MCP export confirmed working end-to-end on `sample-project.fcpxml` (5 clips, ~60s AAC output).
- MP4 export ffmpeg discovery:
  - `Command::new("ffmpeg")` failed when the app's process PATH did not include `/usr/bin`.
  - Added `find_ffmpeg()` which tries the bare name first, then falls back to common absolute paths (`/usr/bin/ffmpeg`, `/usr/local/bin/ffmpeg`, `/opt/homebrew/bin/ffmpeg`).
  - Added `probe_has_audio()` using co-located `ffprobe` to check for audio streams before building the filter graph.
- MP4 export error visibility:
  - FFmpeg error output was silently discarded; exit failures only reported the exit code.
  - Non-progress stderr lines are now captured, logged via `eprintln!`, and included in the returned error message.
- MP4 export reliability:
  - Reworked export pipeline to use ffmpeg clip concat/transcode with per-clip in/out trimming.
  - Normalized sample aspect ratio (`setsar=1`) to prevent concat filter mismatch across mixed sources.
  - Confirmed MCP sample export success:
    - `Sample-Media/mcp-export-test.mp4` (~4.29s)
    - `Sample-Media/mcp-export-full.mp4` (~64.83s)
- Project load visibility:
  - Ensured timeline redraw/content-height update after project load.
  - Reset timeline view state (playhead/scroll/zoom/selection) on New/Open.
  - Synced project clip sources into media library and refreshed browser list when library changes externally.
- Media browser interaction:
  - Fixed click-to-select conflict introduced by drag source handling.
- Timeline scrubber interaction:
  - Fixed continuous click-and-drag scrubbing on the timeline ruler/playhead.
  - Scrubbing now works even when Razor tool is active.
  - Fixed timeline click/seek jumping back to 0 by syncing timeline playhead from program timeline position (not source monitor player position).

### Previous implemented milestones (recent)
- Program monitor playback panel and timeline-linked seeking.
- Audio waveform rendering in timeline audio tracks.
- Drag-and-drop from media browser to timeline.
- Keyboard shortcut overlay and export progress dialog.
- Comprehensive dark theme CSS coverage.
