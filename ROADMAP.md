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
- [x] GApplication entry point with CSS loading
- [x] GNOME HIG-compliant app icon (`data/io.github.ultimateslice.svg`) — camera-cake slice concept
- [x] GitHub Actions workflows on push for native Cargo build/test and Flatpak manifest build

### Data Model
- [x] `Clip` — source path, source in/out (ns), timeline position, label, kind
- [x] `Track` — ordered list of clips, muted/locked flags, `TrackKind` (Video/Audio)
- [x] `Project` — title, frame rate, resolution, tracks, dirty flag
- [x] `MediaItem` — library entry (path, duration, label); separate from timeline clips
- [x] `SourceMarks` — shared in/out selection state for the source monitor
- [x] Unit tests for model, undo, and FCPXML parser (62 tests)

### Media Library Browser
- [x] Import media via file chooser (video/audio/image MIME filter)
- [x] GStreamer Discoverer probes duration on import (background thread via `MediaProbeCache`)
- [x] Library list with clip name + filename display
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
- [x] Adaptive source-proxy scale when proxy mode is Off (Quarter for small source monitor sizes, Half for larger)
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
- [x] Import source-time normalization: rebase `asset-clip@start` by `asset@start` for absolute timecode-domain assets so layered video/audio lane clips seek correctly in Program Monitor
- [x] Export transform overflow clipping: overlay clips with positions exceeding the frame boundary now crop overflow edges before padding, so exported PIP positions match the Program Monitor preview exactly
- [x] Background-threaded project open (file I/O + XML parsing off main thread)

### MCP Server (`--mcp` flag)
- [x] `--mcp` flag enables the MCP (Model Context Protocol) server at startup
- [x] JSON-RPC 2.0 over stdio (MCP 2024-11-05 protocol)
- [x] `--mcp` flag is stripped from argv before GLib sees it
- [x] Background thread reads stdin; main-thread polling via `glib::timeout_add_local`
- [x] Tools: `get_project`, `list_tracks`, `list_clips`, `add_clip`, `remove_clip`, `move_clip`, `trim_clip`, `set_project_title`, `save_fcpxml`, `export_mp4`, `list_library`, `import_media`
- [x] Unix domain socket transport (Preferences → Integration toggle) for connecting to a running instance
- [x] `--mcp-attach` stdio-to-socket proxy so standard MCP clients can use `.mcp.json` to attach
- [x] Python stdio-to-socket MCP bridge script (`tools/mcp_socket_client.py`) with `.mcp.json` server entry (`ultimate-slice-python-socket`)
- [x] `take_screenshot` tool — captures a PNG of the full application window via GTK snapshot + GSK CairoRenderer, written to the current working directory
- [x] `select_library_item`, `source_play`, `source_pause` tools — select media in the library and control Source Monitor playback via MCP

---

## 🔜 Planned

### Source Monitor Improvements
- [x] Clip name shown in source monitor header
- [x] Close button to hide source preview and clear current source selection
- [x] Frame-accurate jog/shuttle control
- [x] Mark-in / Mark-out visible as timecodes in a dedicated bar
- [x] Source preview auto-loads proxy files when available and requests proxy transcodes for high-resolution video

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

### Speed Ramps (per clip)
- [x] Constant speed change per clip (e.g. 0.5× slow-mo, 2× fast-forward) via GStreamer rate seek + ffmpeg `setpts`/`atempo` on export
- [x] Speed indicator badge on clip in timeline (yellow "2×" badge)
- [x] Persist speed data in FCPXML (`us:speed` attribute)
- [x] Reverse playback: per-clip "Reverse" toggle in Inspector applies to Program Monitor preview and export (`reverse`/`areverse`), timeline shows `◀` badge, and state persists via `us:reverse` FCPXML attribute
- [ ] Variable speed ramps: multiple keyframed speed segments within a single clip
- [ ] Optical flow / frame-blending for smooth slow-motion (ffmpeg `minterpolate` on export)

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
  - [ ] Add short frame cache around playhead (previous/current/next frames) to reduce stutter on scrubbing and pause/seek
    - [x] Frame-boundary seek deduplication: quantize paused scrub positions to frame boundaries and skip redundant pipeline work for same-frame seeks
   - [x] Introduce proxy preview mode (quarter/half resolution decode, full-res export) for large media
   - [x] Managed local proxy cache root (`$XDG_CACHE_HOME/ultimateslice/proxies`, fallback `/tmp/ultimateslice/proxies`) with fallback to alongside-media `UltimateSlice.cache` when local-cache transcodes fail
   - [x] Managed proxy cache lifecycle cleanup (startup stale prune for ownership-index entries older than 24h, plus project unload/app-close cleanup of managed cache files)
   - [x] Eager near-playhead proxy priming during project reload (capped, proximity-ordered source requests before first program-player rebuild)
   - [x] Preserve full-frame fit at reduced preview quality (`Half` / `Quarter`) so the monitor downscales the composed frame instead of cropping to the top-left region
    - [x] Add adaptive `Auto` preview quality mode that derives effective quality from current Program Monitor canvas size while preserving manual `Full/Half/Quarter`
    - [x] Auto-enable proxy preview during heavy overlap (3+ active video tracks) when manual proxy mode is Off, with automatic disable when overlap drops
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
          - [x] Prewarm incoming boundary clip decoder/effects resources ahead of handoff (lightweight Ready/Null warm-up)
          - [x] Adaptive rebuild wait budgets: scale preroll/arrival/link waits dynamically from a ring buffer of recent rebuild durations (tighter after fast rebuilds, conservative after slow ones)
          - [x] Audio pipeline continuity: skip audio_pipeline pause/resync at boundaries where only video tracks change
          - [x] Phase-level rebuild telemetry: per-phase timestamps in rebuild_pipeline_at
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

### Color & Effects
- [x] Basic color correction (brightness / contrast / saturation) via GStreamer `videobalance`
- [x] Denoise filter per clip (GStreamer `gaussianblur` positive sigma; ffmpeg `hqdn3d` on export)
- [x] Sharpness / unsharp-mask per clip (GStreamer `gaussianblur` negative sigma; ffmpeg `unsharp` on export)
- [x] LUT import / apply
- [ ] Apply multiple LUTs to a clip
- [x] Color scopes (waveform, vectorscope, RGB parade, histogram)
- [ ] Shadows and Highlights
- [ ] Advanced color grading
- [ ] Color management pipeline (Rec.709 / Rec.2020 / ACES with display transform)
- [ ] HDR workflow (PQ/HLG preview + export metadata)
- [x] Titles / text overlay (`textoverlay`)
- [x] Transition effects (fade to black, wipe right, wipe left)

### Video Transform (per clip)
- [x] Scale / resize clip (zoom in/out within frame) via GStreamer `videoscale` + `videobox`
- [x] Crop clip (left / right / top / bottom margins) via GStreamer `videocrop`
- [x] Rotate clip (90° / 180° / 270° presets) via GStreamer `videoflip`
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
- [x] Shift-constrain while scaling — hold Shift during corner drag to lock aspect ratio
- [x] Keyboard nudge in transform overlay — arrow keys adjust position by 0.01 per press (0.1 with Shift); `+`/`-` adjust scale; activated when a clip is selected
- [x] Transform overlay drag now pauses playback at interaction start, so the Program Monitor frame stays fixed while editing (no background timeline advancement)

### Project Management
- [x] Project save / load as FCPXML (wired to New/Open/Save buttons in toolbar)
- [x] Recent projects list
- [x] Auto-save (60s timer, writes to /tmp/ultimateslice-autosave.fcpxml when project is dirty)
- [ ] Proxy media generation and management

### Canvas / Sequence Settings
- [x] Canvas size dialog (project resolution: 1080p, 4K, custom W×H)
- [x] Frame rate selector in project settings (23.976, 24, 25, 29.97, 30, 60 fps)
- [ ] Aspect ratio presets (16:9, 4:3, 9:16 vertical, 1:1 square)
- [x] Persist canvas settings in FCPXML `<format>` element

### Export
- [x] Advanced export dialog (replace current single-button export)
  - Codec selection: H.264, H.265/HEVC, VP9, ProRes, AV1
  - Container selection: MP4, MOV, WebM, MKV
  - Output resolution presets with downscale support (4K → 1080p → 720p → custom)
  - Bitrate control: CRF / target bitrate mode
  - Audio codec: AAC, Opus, FLAC, PCM
  - Audio sample rate and channel layout (stereo / mono)
- [ ] Export presets: save/load named configurations (e.g. "Twitter 720p", "Archive ProRes")
- [ ] ProRes / WebM / GIF export options
- [x] Export progress dialog with cancel (ProgressBar + status label)

### Polish
- [x] Keyboard shortcut reference overlay (? or / key opens a modal dialog)
- [x] Preferences dialog with categorized sections + hardware acceleration toggle wired to source preview playback
- [x] About dialog in Preferences (General page) with third-party crate/library credits and license notices
- [x] GTK renderer preference (Auto / Cairo / OpenGL / Vulkan) for low-memory devices
- [x] Launch-screen clarity polish (empty-state guidance, wider side panels, and cleaner toolbar/inspector hierarchy)
- [ ] Accessibility: keyboard navigation in all panels
- [ ] Welcome window for choosing recent project or new one
- [ ] Help documentation and tutorials
- [ ] Application icon and desktop integration (`.desktop` file)

### Professional Workflow (The "Pro" Edge)
- [ ] Multicam editing (sync by audio or timecode)
- [ ] Nested Timelines / Compound Clips
- [x] 3-Point and 4-Point editing (Insert/Overwrite from Source)
- [x] J/K/L scrubbing (shuttle control in program monitor; pitch-corrected audio is a future enhancement)
- [ ] Match Frame (shortcut to find timeline clip in media library)
- [ ] Proxy Workflow: One-click toggle between original and proxy media
- [ ] Keyword ranges + favorite/reject ratings in browser
- [ ] Auditions / clip versions (swap alternate takes nondestructively)
- [ ] Plugin architecture for third-party video effects (e.g. OFX/LV2 bridge)

### Advanced Audio
- [ ] Audio Roles (Dialogue, Effects, Music) with submixing
- [ ] Support for LV2 / LADSPA audio plugins
- [ ] Voiceover recording tool with countdown and punch-in
- [ ] Automatic Ducking (music volume lowers during dialogue)
- [ ] Audio normalization and peak-matching

### AI & Automation
- [ ] Speech-to-Text: Automatic subtitle generation and transcription
- [ ] AI Scene Cut Detection for long source files
- [ ] Smart Collections based on metadata (keywords, resolution, frame rate)
- [ ] Optical Flow slow-motion (AI frame interpolation)

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
