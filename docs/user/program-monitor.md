# Program Monitor

The **Program Monitor** shows the assembled timeline played back in real time, clip by clip.

When no timeline clips are present, the monitor area shows a short first-use hint to import media and append/insert clips, and previous-project frames are cleared on project switch/new project.

## Canvas Aspect Ratio

The program monitor constrains its video display area to the **project canvas ratio**
(e.g. 16:9 for a 1920×1080 project). This means:

- If a source clip has a **different aspect ratio** than the canvas (e.g. a 21:9 wide-screen
  clip on a 16:9 canvas), the program monitor will show **black letterbox bars** above and
  below the clip — exactly matching what the exported video will look like.
- If the canvas is wider than the clip (e.g. a 4:3 clip on a 16:9 canvas), black **pillarbox
  bars** appear on the sides.
- The canvas ratio updates automatically when you change the project resolution in
  **Project Settings**.

This makes it much easier to judge clip placement, scale, and whether content is inside
or outside the export frame.

## Controls

| Element | Description |
|---|---|
| Video display | Renders the assembled sequence at the playhead position |
| Timecode label | Current timeline position |
| Go To button | Opens a timecode entry dialog (`HH:MM:SS:FF`) and jumps playhead |
| Play / Pause button | Toggle playback |
| Stop button | Stop and return to position 0 |
| Safe Areas toggle | Shows/hides action-safe (90%) and title-safe (80%) guides |
| Master VU meter | Stereo (L/R) output level meter in dBFS |

## Docked Resize

- When the Program Monitor is docked, you can drag the splitter between the preview and scopes area to resize how much space each gets.
- If scopes are hidden, the scopes pane is fully collapsed (the splitter/pane disappears).
- The docked splitter position is saved and restored on next launch.

## Keyboard Shortcuts

| Shortcut | Action |
|---|---|
| `Space` | Play / Pause (when timeline has focus) |
| `Ctrl+J` | Open Go To Timecode dialog |
| `←` / `→` / `↑` / `↓` | Nudge selected clip position in transform overlay (0.01) |
| `Shift + Arrow` | Coarse nudge selected clip position (0.1) |
| `+` / `-` | Increase / decrease selected clip scale in transform overlay |

## Transform Overlay Controls

When a visual timeline clip or adjustment layer is selected, the Program Monitor overlay provides direct transform editing:

- **Corner handles**: drag to scale; hold **Shift** for constrained scaling.
- **Center drag**: pan (Position X/Y).
- **Edge midpoint handles**: drag top/bottom/left/right handles to adjust crop directly in preview.
- Keyboard nudges work when the overlay has focus (click the monitor once).
- Starting an overlay drag pauses playback and keeps the current frame locked while editing; playback remains paused after you release.
- For **adjustment layers**, these controls edit the scoped effect region instead of moving source pixels. The overlay box is the exact region used for live scoped adjustment preview.

## Safe Area Guides

- Use **Safe Areas** in the Program Monitor header to toggle framing guides.
- When enabled, the monitor draws:
  - **Action-safe** at **90%** of the canvas.
  - **Title-safe** at **80%** of the canvas.
- The toggle state is persisted across launches.

## Playback Behaviour

- The program monitor uses a GStreamer **compositor** pipeline that layers all active video tracks simultaneously at the playhead position.
- Each active clip gets its own decoder branch with per-clip effects, connected to the compositor with correct z-ordering (higher tracks render on top).
- Audio from active video clips is mixed through an **audiomixer** element (except freeze-frame video holds, which are intentionally silent); audio-only tracks use a separate playbin.
- Before clip audio reaches the preview mixer, UltimateSlice normalizes it to a fixed 48 kHz stereo raw-audio format. This keeps lower-rate camera AAC sources (such as 16 kHz mono Ring clips) from tripping GStreamer `not-negotiated` playback errors in the Program Monitor.
- Animated SVG clips play through a cached silent rendered derivative so authored motion appears in preview, while timeline extensions beyond the authored duration hold on the last frame.
- Program Monitor shows a **master stereo meter** (L/R), updated from GStreamer `level` elements.
- During prerender playback, per-track timeline meters remain active by mapping prerender audio-level telemetry to the currently active prerender track set.
- Timeline position is tracked via wall-clock timing for reliable playhead movement — no seek-anchor heuristics needed.
- Audio boundaries are enforced via GStreamer seek stop positions, so audio stops precisely at the clip's source out-point.
- When clip boundaries are crossed during playback (a clip starts or ends), the pipeline is briefly rebuilt with the new set of active clips.
- During those boundary rebuilds, audio-only preview playback is paused/re-synced to the current timeline position before resume so audio does not run ahead and end earlier than video.
- All per-clip effects (color, denoise, sharpness, crop, rotate, flip, scale, position, title overlay, speed) are applied per-slot during playback.
- Adjustment layers are applied post-compositor. Supported scoped preview effects (including LUTs, primary color, temperature/tint, and three-point grading) are limited to the selected adjustment clip's transformed bounding box, and overlapping adjustment layers stack by track order.
- **Transitions** (cross-dissolve, fade-to-black, wipe-right, wipe-left) are previewed in real time during both playback and scrubbing, matching the FFmpeg `xfade` export output. Dissolve and fade transitions animate compositor pad alpha; wipe transitions use videocrop animation on the incoming clip to progressively reveal it.
- Freeze-frame clips are rendered as true video holds: Program Monitor samples the configured freeze source frame and holds that frame for the clip's resolved freeze duration during playback and scrubbing.
- Freeze-frame decoder seeks use accurate (non-key-unit) frame selection for the hold sample, preventing black-frame preview failures on long-GOP media.
- Scale/Position edits from the Inspector and transform overlay are applied to the active preview clip immediately in both paused and playing states.
- If optional denoise filters are unavailable in your GStreamer runtime, Program Monitor still applies crop/scale/position transforms.
- Program Monitor normalizes preview output to square pixels (`PAR 1:1`) so 21:9/ultra-wide sources don't keep aspect-ratio bars after zoom scaling.
- Playback priority can be set in **Preferences → Playback** (`Smooth`, `Balanced`, `Accurate`) to control smoothness vs seek precision.
- During playback boundary handoffs (when the active clip set changes because a clip starts/ends), UltimateSlice uses accurate decoder seeks so long-GOP proxy media does not jump to an earlier keyframe.
- During overlap boundary rebuilds, delayed video-pad linking no longer forces EOS on an already-linked slot audio branch, which reduces unintended video-track audio dropouts when heavier clips enter.
- During active overlap handoffs that start from an already-running slot set, UltimateSlice defers early pre-link EOS for newly added slots so late pad-added links can settle before post-seek arrival checks.
- During boundary rebuilds, both the compositor and audiomixer aggregators are flushed together so their output running-time stays in sync; this prevents the audiomixer from dropping audio buffers as "late" after a video-path flush reset.
- When a clip boundary only affects video tracks (e.g. a new video layer enters while the audio track continues), the audio pipeline is left running instead of being paused and resynced, eliminating the audible audio gap during video-only transitions.
- When a video-only boundary still requires an explicit audio resync, UltimateSlice now flushes and re-seeks the existing multi-audio decoder set in place if the same music/voiceover clips remain active, which keeps continuing audio tracks aligned without cutting or silencing shorter overlapping audio clips.
- Boundary rebuilds log per-phase timing (teardown, build, link wait, preroll, seek) to help diagnose and tune transition performance.
- Post-seek wait budgets are automatically tightened when the boundary was prewarmed by a sidecar pipeline, since warm file cache enables faster decoder settle.
- Occlusion audio-only decode substitution is currently disabled in preview rebuilds to prioritize reliable mixed audio from overlapping video tracks.
- Proxy preview mode can be enabled in **Preferences → Playback** to generate lightweight proxy files for smoother playback with large media. Export always uses original full-resolution media.
- **Background Removal**: Clips with "Remove Background" enabled in the Inspector use a pre-processed alpha-channel video (VP9 alpha WebM) for both preview and export. Processing runs offline using ONNX Runtime inference (MODNet segmentation model) and progress is shown in the status bar. The MODNet model can be downloaded from **Preferences → Models**.
- Preview quality (`Full` / `Half` / `Quarter`) downscales the composed monitor output while preserving full-frame fit/framing in the Program Monitor.
- Preview quality `Auto` dynamically adjusts effective monitor output quality from the current Program Monitor canvas size (including resize/zoom changes) to balance clarity and performance.
- While playback is active, Auto quality changes use a short minimum dwell to avoid rapid resolution flapping when overlap transitions briefly change load.
- In **Smooth** playback priority, the monitor enables an audio-master "drop-late" preview path whenever video playback is active, so late video frames are dropped rather than queued behind audio; when playback pauses/stops, normal non-dropping buffering is restored.
- During heavier overlap windows (especially 3+ active slots), per-clip compositor branch queues also switch to drop-late mode to reduce branch backpressure and boundary handoff stalls.
- During playback, the monitor also prewarms the next near-future boundary clip set (look-ahead probe/path warm-up), including lightweight incoming decoder/effects resource warm-up, to reduce transition-handoff stalls.
- In **Smooth** playback priority with background prerender enabled, UltimateSlice prewarms a slightly deeper upcoming-boundary horizon (and farther lookahead) for transition windows; when background prerender jobs are already heavily queued, it automatically falls back to the baseline depth to avoid overscheduling.
- Program Monitor logs now include periodic transition prerender hit/miss summaries by transition kind, which helps profiling runs identify where prerender is being generated but not consumed.
- Smooth-mode transition prewarm depth/lookahead is also auto-tuned from recent prerender hit/miss history: if hit rate stays low after enough samples, prewarm temporarily expands (bounded by queue-pressure guardrails) to improve prerender availability.
- Transition prerender windows include a small frame padding around overlap boundaries; incoming transition input is held through pre-overlap padding so source timing stays correct while reducing edge handoff misses.
- When Smooth-mode queue budget is tight, transition prewarm scheduling prioritizes boundaries with the worst observed prerender hit rates first, improving the odds that limited background prerender work helps the most problematic transitions.
- Transition prerender overlap padding now includes incoming audio timing parity: incoming transition audio is delayed until the overlap boundary, avoiding early incoming-audio bleed during the pre-padding window.
- Queue-constrained transition prewarm now also factors boundary proximity into prioritization, preventing far-future high-risk boundaries from starving near-term boundary preparation.
- Transition prerender hit/miss metrics are recency-weighted via periodic decay, so adaptive tuning and prioritization respond to current session behavior rather than stale long-ago outcomes.
- Background prerender queue admission is now priority-aware under load: queue depth is capped, and overflow is only allowed for substantially higher-priority requests, reducing low-value prerender churn.
- Ready prerender segments are now cache-pruned by playhead distance (while protecting any currently active prerender segment), keeping cache size bounded and focused on likely near-term reuse.
- Prerender cache lookups now track hit/miss telemetry (with hit-rate summaries), and `get_performance_snapshot` includes `prerender_cache_hits`, `prerender_cache_misses`, and `prerender_cache_hit_rate_percent`.
- For proxy-backed prerender inputs, LUT is not re-applied in the prerender FFmpeg graph, preventing double LUT grading when the proxy media is already LUT-baked.
- When a **scoped** adjustment layer is active, background prerender falls back to the live compositor-output path so the Program Monitor does not show stale full-frame adjustment renders.
- Background prerender now preserves animated **brightness / contrast / saturation / temperature / tint** keyframes, so overlap playback stays closer to the final export when those color controls are keyframed.
- Transparent title clips keep their alpha in background prerender, so prerendered title overlays show the lower video tracks behind the text instead of flattening to black.
- Title fonts in background prerender now reuse the selected family plus structured weight/slant/width selectors, so bold and italic title faces stay closer to the live Program Monitor preview instead of falling back to a regular face.
- Optional FFmpeg frei0r module probes used by background prerender now fail quietly when those modules are unavailable, so title-heavy compositions fall back cleanly without misleading `Could not find module` log spam.
- Background prerender now carries the remaining static clip-local visual effects that were missing from the FFmpeg path, including shape masks, clip blur, flip transforms, and anamorphic desqueeze.
- If an overlap window contains clip features the prerender FFmpeg path still cannot reproduce exactly (for example animated transform/mask properties, speed/reverse/freeze timing, or advanced clip-audio effects), Program Monitor falls back to the live compositor path instead of using an incorrect cached segment.

## Seeking

- Click on the **ruler** in the timeline to seek the program monitor to that position.
- Use **Go To** in the Program Monitor header (or **Ctrl+J**) to jump directly to a timecode in `HH:MM:SS:FF` format.
- The program monitor seeks to the correct source position within the appropriate clip, accounting for clip speed.
- When scrubbing within the same clip, the existing decoder is seeked in-place (no pipeline rebuild) so the monitor shows the frame at the exact playhead position without a black-screen or first-frame flash.
- When the playhead crosses a clip boundary (different clips become active), the pipeline is briefly rebuilt for the new set of active clips.
- Opening a project and seeking immediately now follows the same safe paused rebuild/seek flow, avoiding intermittent monitor freezes during initial interaction.
- Opening/creating a project does not auto-start playback; Program Monitor remains paused until you explicitly press Play.
- Project reload + first seek now run as short staged callbacks (load first, then seek), and stale pending seek/reload requests are coalesced so rapid edits/scrubs don't queue long back-to-back main-thread work.
- Proxy mode is now strict: when set to `Off`, Program Monitor does not auto-enable proxy playback during overlap boundaries.
- During paused scrubbing, UltimateSlice waits for a fresh post-seek preroll frame so the Program Monitor and transform overlay update to the new playhead frame instead of showing black.
- During paused scrubbing, active clip decoder branches are created before preroll/seek settle so the monitor does not remain stuck on a black frame after moving the playhead.
- With 3+ active video tracks, paused settle waits are budget-capped to keep the UI responsive; if the full second-pass settle would exceed the budget it is skipped in favor of immediate interactivity.
- During paused scrubbing, Program Monitor keeps a short previous/current/next frame cache around the playhead (keyed by frame position and current render state) and uses cache hits to tighten in-place seek settle waits, reducing repeated scrub stutter around nearby frames.
- Manual timeline seeks use the paused accurate-seek path and then resume playback if it was active, so the frame shown at the playhead is updated before playback continues.
- While paused, the monitor is repainted continuously so delayed post-seek frame updates still appear without requiring playback to resume.
- Subtitle text is drawn in the Program Monitor overlay layer above the video pictures, so subtitles remain visible even when the underlying video frame is coming from background-prerendered playback.
- Subtitle preview and export still use different renderers (GTK/Cairo overlay in preview, libass/ASS in export), but the monitor now scales subtitle outline, box padding, underline, and stroke metrics from the preview height/font size using the same 1080-based sizing model as export, and it maps `subtitle_position_y` to the same anchored top/center/bottom subtitle region instead of using it as a raw baseline, which keeps common subtitle styles visually closer.

## Playhead Accuracy

- When you seek and then press Play, UltimateSlice rebuilds the compositor pipeline for the active clips at the playhead position and waits for post-seek preroll (up to ~2 seconds in paused accurate mode for long-GOP media) before transitioning back to Playing. This ensures playback starts from the correct frame rather than jumping to position 0.
- During active playback boundary handoffs, preroll waits are tuned for responsiveness (shorter than paused scrubbing waits) to reduce visible stutter while preserving accurate clip positioning.
- Wait budgets for boundary rebuilds adapt automatically based on recent rebuild performance: when recent transitions completed quickly, subsequent waits are tightened to reduce blocking; when they were slow, budgets widen for reliability.

## Speed Change Preview

When a clip has a speed multiplier set (see [inspector.md](inspector.md)), the program monitor plays it at that rate using GStreamer's rate-seek mechanism. Audio pitch is **not** corrected in the preview (it sounds higher/lower pitched). The exported file uses `atempo` for proper pitch correction.

When **Reverse** is enabled on a clip, Program Monitor preview plays that clip backward (video and audio direction) while keeping other timeline layers audible.

## MCP Automation

- `seek_playhead` seeks the timeline/program-monitor playhead to an absolute nanosecond position.
- `get_performance_snapshot` returns compact Program Monitor performance metrics for automation (prerender queue/segment state, recent rebuild timings, and transition prerender hit/miss rates).
- `export_displayed_frame` exports the current displayed frame to a binary PPM (`P6`) image file.
- `take_screenshot` captures a PNG screenshot of the full application window using the GTK snapshot API and GSK `CairoRenderer`. The PNG is written to the current working directory as `ultimateslice-screenshot-<unix_epoch>.png`.
