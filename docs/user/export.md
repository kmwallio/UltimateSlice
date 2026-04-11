# Export

Use the toolbar linked split control **Export | ▼** (styled as one control):
- Click **Export** to open the advanced export dialog.
- Click **▼** for additional options such as **Export Frame…**.

**Export Frame…** saves the currently displayed Program Monitor frame as:
- **PNG** (`.png`)
- **JPEG** (`.jpg` / `.jpeg`)
- **PPM** (`.ppm`)
- Frame capture is written at the **project canvas resolution** (not reduced preview quality resolution).
- If playback is active, UltimateSlice pauses internally for capture and then resumes playback.

Animated SVG clips are rendered to cached silent video during export. Static image clips still use single-frame hold behavior, while animated SVG clips preserve authored motion and hold on the last frame if the clip was extended on the timeline.

Tracked clip transforms and first-mask attachments use the same motion data during export as they do in Program Monitor preview, including trackers with dense sample counts.

## Export Dialog Options

### Video Codec

| Option | ffmpeg encoder | Notes |
|---|---|---|
| **H.264** (default) | `libx264` | Best compatibility; works in all players/platforms |
| **H.265 / HEVC** | `libx265` | ~50% smaller files than H.264 at same quality; requires player support |
| **VP9** | `libvpx-vp9` | Open format; good for web (WebM container) |
| **ProRes** | `prores_ks` | High-quality mastering format; large files; use with MOV container |
| **AV1** | `libaom-av1` | Excellent compression; very slow to encode |

### Container

| Option | Extension | Best with |
|---|---|---|
| **MP4** (default) | `.mp4` | H.264, H.265, AV1 |
| **QuickTime** | `.mov` | ProRes, H.264 |
| **WebM** | `.webm` | VP9, AV1 |
| **Matroska** | `.mkv` | Any codec |
| **Animated GIF** | `.gif` | Animation, social media (no audio) |

MP4 and MOV containers get `-movflags +faststart` for web streaming compatibility.

> **Animated GIF**: Selecting this container hides the Video Codec and Audio settings (not applicable). A **GIF Frame Rate** spinner (1–30 fps, default 15) appears instead. GIF export uses FFmpeg's two-step `palettegen` → `paletteuse` pipeline with Bayer dithering for optimal color quality. The output loops infinitely (`-loop 0`). GIF files are significantly larger than video formats — use short clips or reduce the frame rate for smaller files.

### Output Resolution

| Option | Pixels | Use case |
|---|---|---|
| **Same as project** | Project width × height | No downscale |
| **4K UHD** | 3840 × 2160 | Archive / large screen |
| **1080p** | 1920 × 1080 | Standard HD delivery |
| **720p** | 1280 × 720 | Web / streaming |
| **480p** | 854 × 480 | Small file / mobile |

Clips are letterboxed/pillarboxed to fit the chosen resolution while preserving aspect ratio.

### Quality (CRF)

The **CRF** (Constant Rate Factor) slider controls quality vs. file size:

- **Lower = better quality, larger file**
- **Higher = lower quality, smaller file**
- Typical values: 18 (visually lossless) → 28 (acceptable web quality)
- Default: **23** (good balance)
- Not used for ProRes (lossless-ish by nature).

### Audio Codec

| Option | Notes |
|---|---|
| **AAC** (default) | Lossy; excellent compatibility |
| **Opus** | Lossy; excellent quality at low bitrates; best in WebM/MKV |
| **FLAC** | Lossless; large files |
| **PCM** | Uncompressed; very large; use for mastering |

### Audio Bitrate

Applies to AAC and Opus. Ignored for FLAC and PCM.

- Default: **192 kbps** (high quality)
- Acceptable web quality: 128 kbps
- High fidelity: 256–320 kbps
- Recommended for 5.1 surround AAC: **448 kbps**

### Per-clip audio chain order

For clips that have audio effects in the Inspector, the export filter graph
runs them in this order:

```
source audio
  → speed (atempo) / reverse
  → Enhance Voice (HPF + afftdn + EQ + compressor)   ← if "Enhance Voice" is on
  → Volume + Voice Isolation ducking                  ← if Voice Isolation > 0
  → channel routing
  → pitch shift
  → LADSPA effects
  → Match EQ (7-band, mic match)
  → User EQ (3-band parametric)
  → fades
  → pan
  → track delay
  → audiomixer pad → master volume → output
```

The important thing for voice work is that **Enhance Voice runs before Voice
Isolation**. The cleanup chain (high-pass, denoise, presence boost, compression)
happens first, so when voice isolation makes its ducking decision it sees a
cleaned-up signal — quieter background, more even speech levels — and the
ducked floor sounds natural instead of crunchy. If you have a clip with both
features on, this is the right order; nothing to configure.

The Realtime preview goes through the **same** filter chain via a background
prerender (cached under `$XDG_CACHE_HOME/ultimateslice/voice_enhance/`), so
what you hear in the Program Monitor is byte-identical to what the export
will produce. See [`docs/user/inspector.md` → Enhance Voice](inspector.md#enhance-voice).

### Audio Channels — Advanced Audio Mode (Surround)

The **Audio Channels** dropdown selects the output channel layout. Default is
**Stereo**, which preserves the existing pipeline byte-for-byte. Two surround
options opt into multichannel output:

| Option | Channels | ffmpeg layout |
|---|---|---|
| **Stereo** (default) | 2 | `stereo` |
| **5.1 Surround** | 6 | `5.1` (FL FR FC LFE BL BR) |
| **7.1 Surround** | 8 | `7.1` (FL FR FC LFE BL BR SL SR) |

Surround output is supported by **AAC**, **Opus**, **FLAC**, and **PCM**.
GIF has no audio so the dropdown is hidden when **Animated GIF** is selected.

#### Role-based auto-routing

Each track's existing **Audio Role** drives a sensible default destination in
the surround upmix:

| Audio Role | 5.1 Destination | 7.1 Destination |
|---|---|---|
| Dialogue | Front Center (FC) | Front Center (FC) |
| Music | Front L/R (FL+FR) | Front L/R (FL+FR) |
| Effects | Front L/R + Surround L/R (FL+FR+BL+BR) | Front L/R + back **and** side rears (FL+FR+BL+BR+SL+SR at lower gain) |
| None | Front L/R | Front L/R |

This means a typical project with one Dialogue track, one Music track, and one effects track will produce a usable 5.1 mix the moment you switch the dropdown to **5.1 Surround** — no per-track configuration required.

#### Per-track surround override (Inspector)

For more control, the **Inspector → Audio → Surround Position** dropdown lets
you pin a track to a specific destination, overriding the role-based default.
Options:

- **Auto (by role)** — default; uses the table above
- **Front L/R**, **Front Center**, **Front L/R + Surround L/R**, **Surround L/R**
- **LFE (bass only)** — pin a track to the subwoofer channel
- Single-channel pins: **Front Left**, **Front Right**, **Back Left**, **Back Right**, **Side Left**, **Side Right**

The override has no effect on stereo exports; it only kicks in when the export
channel layout is 5.1 or 7.1.

#### Automatic LFE bass tap

Surround exports automatically derive subwoofer (LFE) content from **Music**
and **Effects** tracks. A parallel filter chain runs each eligible stem through
two cascaded 120 Hz lowpass filters (~24 dB/oct slope) and routes the result
to LFE only. **Dialogue is excluded** so speech bleed never drives the
subwoofer. A track that's been explicitly assigned the **LFE** override is not
also tapped — it goes through the explicit pan path instead.

#### Limitations (phase 1)

- The upmix matrix is static for the duration of an export. Per-clip pan
  keyframes still control L/R balance within each stem, but pan keyframes
  cannot dynamically move a stem from FL → FC during playback. Dynamic
  surround panning is a future roadmap item.
- 7.1 has no canonical FCPXML X audio layout — when sidecar FCPXML is also
  written for a 7.1 export, the strict-DTD writer falls back to declaring
  `audioLayout="5.1"` and logs a warning so the file still imports cleanly
  into Final Cut.

#### Stereo regression safety

The stereo path is gated and produces a byte-identical filter graph to the
pre-surround code. Existing stereo exports, presets, and FCPXML round-trips
behave exactly as they did before — switching the dropdown is the only way
to opt into the new pipeline.

### Automatic Audio Crossfades

Export honors Timeline crossfade preferences (set in Preferences → Timeline, or via MCP `set_crossfade_settings`):

- `crossfade_enabled`
- `crossfade_curve` (Equal power or Linear)
- `crossfade_duration_ns`

When enabled, export applies automatic fades at adjacent same-track audio edit points for:

- clips on non-muted audio tracks
- embedded audio in eligible video clips (when embedded audio is not suppressed by linked audio peers, clip audio is present, and the clip is not a freeze-frame hold)

Fade lengths are clamped safely for very short clips and overlap boundaries so exports remain stable.

## Video Transitions

Export applies the supported primary-track transition set using the same duration and alignment timing shown in the Timeline and Program Monitor.

- The Inspector and **Transitions** pane expose the preview-supported transition set: `Cross-dissolve`, `Fade to black`, `Fade to white`, `Wipe left/right/up/down`, `Circle open/close`, `Cover left/right/up/down`, `Reveal left/right/up/down`, and `Slide left/right/up/down`.
- **End on cut**: the overlap finishes at the cut.
- **Center on cut**: the overlap is split across the cut.
- **Start on cut**: the overlap begins at the cut.
- For any post-cut overlap portion, export mirrors preview/background-prerender by holding the outgoing clip's last frame after the cut instead of reading source past the trimmed out-point.

### Frei0r effect export compatibility

When UltimateSlice cannot discover native frei0r plugin metadata on the local system, export falls back to built-in native parameter schemas for supported plugins instead of guessing from unordered numeric parameters.

This keeps FFmpeg frei0r exports more consistent across machines for effects such as **3-point color balance**, including correct bool formatting and grouped color values.

Title text export also resolves the selected Pango font into structured fontconfig selectors (family plus weight/slant/width), which keeps bold and italic title faces closer to the live Program Monitor preview.

## Export Presets

Use the **Preset** row in the Export dialog to save and reuse named export configurations:

- **Save As…** stores the current dialog settings as a named preset.
- **Update** overwrites the currently selected preset with current widget values.
- **Delete** removes the selected preset.
- Selecting a preset immediately applies its codec/container/resolution/CRF/audio settings.
- **(Custom)** means no saved preset is currently selected.
- New installs (and older UI-state files missing export preset config) start with bundled defaults: **Web H.264 1080p**, **High Quality H.264 4K**, **Archive ProRes 4K**, **WebM VP9 1080p**, and **Animated GIF**.

Preset data is stored in local UI state and persists across app restarts.

### MCP preset tools

For automation workflows, MCP also exposes preset operations:

- `list_export_presets`
- `save_export_preset`
- `delete_export_preset`
- `export_with_preset`

## Export Progress

After choosing the output file, an export progress dialog shows:
- A progress bar driven by ffmpeg progress output. It estimates final file size from bitrate × duration and tracks ffmpeg `total_size` against that estimate when possible, then automatically falls back to ffmpeg `out_time_*` progress while the muxed file size is still too small to be a useful signal.
- Progress is capped at **99%** while encoding/muxing is still running, then switches to **100%** only after export completes successfully.
- A status label showing the output path.
- A **Close** button (available once export completes or errors).

## Batch Export Queue

Queue multiple exports to run sequentially — useful for overnight renders, social media variants, or outputting the same project in multiple formats.

### Adding jobs to the queue

In the Export Settings dialog, configure your options as usual, then click **Add to Queue** instead of **Export Now**. A file chooser prompts for the output path. The job is added to the queue immediately (no export starts yet).

### Opening the queue

Click the **▼** dropdown next to the Export button and choose **Export Queue…**.

### Queue window

| Control | Description |
|---|---|
| Job list | Shows each job: file name, output path, and status badge |
| **✕** (per job) | Remove a Pending or Error job |
| **Run Queue** | Export all Pending jobs in order (background thread, live status updates) |
| **Clear Done/Error** | Remove all completed and failed jobs from the list |

Status badges: `Pending` → `Running…` → `Done ✓` or `Error ✗`

The queue persists across application restarts.

### MCP queue tools

| Tool | Description |
|---|---|
| `add_to_export_queue` | Add an export job; optionally specify `preset_name` |
| `list_export_queue` | List all jobs with status |
| `clear_export_queue` | Remove jobs; optional `status_filter`: `"all"`, `"done"`, `"error"` |
| `run_export_queue` | Run all pending jobs and block until complete |

## Speed-Changed Clips

Clips with a speed multiplier are exported correctly:
- Video: `setpts=PTS/speed` filter adjusts frame timestamps.
- Audio: chained `atempo` filters adjust playback rate while preserving pitch.
  - The `atempo` filter is limited to 0.5×–2.0× per instance; multiple are chained for 0.25× or 4×.

For reversed clips, export applies `reverse`/`areverse` before speed scaling so both video and audio are rendered backward.

### Variable speed ramps

Clips with speed keyframes use dynamic expressions for export:
- Video: `setpts=PTS/(speed_expr)` where `speed_expr` is a piecewise interpolation of the speed keyframes (supports linear and eased curves).
- Audio: uses the mean speed over the clip as a constant `atempo` chain (FFmpeg's `atempo` and `asetrate` filters do not support time-varying expressions). Pitch-preserving variable-speed audio (e.g. via Rubberband) is a future roadmap item.

The exported clip duration matches the timeline duration computed from the speed integral.

### Slow-motion interpolation

When **Slow-Motion Interpolation** is enabled in the Inspector for a clip with effective speed < 1.0, export inserts a smoothing pass appropriate to the chosen mode:

- **Frame Blending** (`mi_mode=blend`): fast temporal averaging between frames. Uses ffmpeg `minterpolate` appended after the speed filter at the project frame rate.
- **Optical Flow** (`mi_mode=mci`): classical motion-compensated interpolation. Uses ffmpeg `minterpolate` appended after the speed filter at the project frame rate (significantly slower to encode than Frame Blending).
- **AI Interpolation (RIFE)**: a higher-fps **sidecar** is precomputed by the background `FrameInterpCache` (see [inspector.md](inspector.md#slow-motion-interpolation)). Export then reads the sidecar instead of the original source for that clip — `minterpolate` is **not** applied because the sidecar already contains the interpolated frames. Both Program Monitor preview and export consume the same sidecar so the visible frames match exactly.

If a clip is set to AI Interpolation but the sidecar is not yet ready (still generating, model missing, or generation failed), export falls back to the original source and skips frame synthesis for that clip. Normal-speed and fast clips are unaffected by all three modes. Background prerender also applies the minterpolate path when enabled.

## Keyframed Properties

Export evaluates phase-1 clip keyframes with interpolation-aware curves:

- **Video:** `scale`, `position_x`, `position_y`, and `opacity`
- **Audio:** `volume`

Keyframes are evaluated in clip-local timeline time and rendered directly into ffmpeg filter chains so exported animation follows the same keyframe timing model used by Program Monitor preview. Dopesheet custom Bezier handle shapes are exported through a piecewise cubic-bezier approximation.

## Adjustment Layers

- Adjustment layers export as post-compositor effect passes over the assembled timeline image.
- The exported effect region uses the same **scale / position / crop / rotate / opacity** scope model as the Program Monitor overlay for adjustment clips.
- If an adjustment layer has an enabled shape mask, export intersects that mask alpha with the adjustment scope so the rendered effect region matches Program Monitor preview. Rectangle/ellipse masks stay inline in the FFmpeg graph; path masks rasterize to a temporary grayscale mask and are transformed with the adjustment clip before blending.
- For safe tracked/scoped adjustment cases, export now crops the work area down to a conservative bounded ROI before running the adjustment effect chain, which keeps exact output quality while avoiding needless full-frame processing for small moving masks.
- When that safe ROI path still has moving tracked geometry, UltimateSlice now pre-renders the resolved adjustment alpha as a temporary grayscale matte stream and `alphamerge`s it back into the cropped effect pass. This preserves preview/export parity while avoiding very large per-pixel tracked `geq` expressions in FFmpeg.
- Adjustment passes that still rely on the full-frame path (for example path masks or higher-risk effect combinations) fall back automatically.
- Each adjustment layer is trimmed to its own clip-local time before FFmpeg evaluates keyframed effect expressions, so adjustment-layer keyframes animate relative to the adjustment clip instead of the global timeline.
- Overlapping adjustment layers stack in track order, matching Program Monitor preview.

## Freeze-Frame Clips

- Freeze-frame clips export as video-only holds: ffmpeg samples the resolved freeze source frame and clones it for the resolved hold duration.
- Freeze-frame timing in export is aligned with Program Monitor preview so freeze durations and transition overlap timing match.
- Embedded video-track audio is intentionally omitted for freeze-frame clips (silent hold behavior).

## Chapter Markers

- Timeline markers (see [timeline.md](timeline.md#chapter-markers)) are automatically embedded as **chapter metadata** in exported MP4, MOV, and MKV files.
- Each marker creates a chapter starting at the marker's position; chapters end at the next marker or the project end.
- Chapters appear in media players that support them (VLC chapter nav, YouTube chapter timestamps, MKV chapter menus, etc.).
- Projects with no markers produce export output with no chapter metadata (no change in behavior).
- Verify chapters with: `ffprobe -show_chapters output.mp4`

## EDL Export (CMX 3600)

Export the timeline as a standard CMX 3600 Edit Decision List for handoff to color grading systems (DaVinci Resolve, Baselight) or broadcast.

**Export → Export EDL...** opens a file dialog to save the `.edl` file.

Features:
- Non-drop frame timecode for most frame rates; drop-frame (`;` separator) for 29.97fps
- Record timecodes start at 01:00:00:00 (broadcast standard)
- Source timecodes reflect clip in/out points
- Dissolve and wipe transitions preserved
- Speed effects noted via M2 comments
- Multi-track support (V, A, A2, A3...)
- Source file paths in comments for relinking
- Title and adjustment clips excluded (no source media)

Also available via MCP: `save_edl` tool with `path` parameter.

## OpenTimelineIO (OTIO) Export

Export the timeline as an OpenTimelineIO JSON file (`.otio`) for interchange with DaVinci Resolve, Premiere (via adapter), Nuke, RV, and other OTIO-compatible tools.

**Export → Export OTIO...** first lets you choose whether media references inside the OTIO file should be written as **absolute paths** or **paths relative to the exported `.otio` file**, then opens the save dialog.

Features:
- Clips with source media references (`file://` URLs)
- Absolute or relative media references inside the OTIO file (relative paths are resolved from the OTIO file's folder on import/open)
- Explicit gaps between clips
- Transitions (cross dissolve mapped to `SMPTE_Dissolve`)
- Speed effects stored as `LinearTimeWarp` OTIO effects
- Project markers attached to the first video track
- Track metadata (muted, locked, soloed, audio role, ducking)
- UltimateSlice OTIO metadata currently preserves the supported clip metadata set, including core clip settings (`speed`, `reverse`, `opacity`, `volume`, `pan`, `brightness`, `contrast`, `saturation`), transform/compositing settings (`scale`, `position_x`, `position_y`, `rotate`, `flip_h`, `flip_v`, `crop_left`, `crop_right`, `crop_top`, `crop_bottom`, `blend_mode`), and core animated lanes (`opacity_keyframes`, `scale_keyframes`, `position_x_keyframes`, `position_y_keyframes`, `rotate_keyframes`)
- Title clips exported with `MissingReference`, plus title styling metadata (text, font, colors, outline, shadow, box, template, secondary text, and clip background color)
- Subtitle-bearing clips preserve subtitle segments/word timing plus subtitle styling metadata (language, font/colors, outline, background box, highlight flags, highlight color, BG highlight color, base styles, word window, and vertical position)
- Adjustment clips also export as `MissingReference`

**Import:** Open `.otio` files via the main **Open…** action in the header bar or Welcome screen (or MCP `open_otio` tool). OTIO files from other tools are imported with default clip properties; UltimateSlice metadata is restored when present. Relative OTIO media references are resolved against the opened `.otio` file location. UltimateSlice also accepts older flat OTIO metadata from previous app builds and upgrades it to the current versioned OTIO metadata contract on save.

Current limitation: OTIO round-trip is still partial for some UltimateSlice-only features. Advanced effect stacks, mask payloads/animation, secondary keyframe lanes such as crop animation, and nested clip internals are not fully preserved yet, so `.uspxml` remains the highest-fidelity interchange/save format for complete UltimateSlice projects.

Also available via MCP: `save_otio` and `open_otio` tools. `save_otio` accepts `path` plus optional `path_mode` (`absolute` or `relative`), and `open_otio` resolves relative media references from the OTIO file location.

## Notes

- Export requires **ffmpeg** to be installed and on `$PATH`.
- All video tracks are processed in timeline order, with letterbox/pillarbox padding applied to each clip.
- Secondary-track overlays keep transparent padding when zoomed out and honor per-clip opacity, so layered composites export closer to Program Monitor preview.
- Title clips and other tracker-followed clips now use direct canvas translation for `Position X/Y`, so export keeps them moving at `Scale = 1.0` and stays aligned with the Program Monitor near frame edges. Normal still-image clips keep the existing non-tracked image path unless they are actually following a tracker.
- If lower video tracks are empty, export automatically promotes the first non-empty active video track to the base layer so upper-track PNG/title overlays still render instead of failing with “No video clips to export”.
- Overlay clips positioned near frame edges (where the PIP extends beyond the output boundary) are correctly clipped to match the preview — the export pre-crops overflow before padding so the visible portion and position match exactly.
- Primary static color controls (`brightness`, `contrast`, `saturation`, plus static `exposure`) are mapped through the same calibrated primary-color model used by Program Monitor preview, including contrast-dependent brightness compensation, to improve low/high-contrast parity.
- Extended grading sliders (`shadows`, `midtones`, `highlights`, `exposure`, `black point`, and per-tone warmth/tint) now prioritize preview/export parity. When FFmpeg frei0r modules are available, export uses a bridge path aligned with Program Monitor’s calibrated grading mapping; otherwise it falls back to native FFmpeg grading filters. Tonal warmth/tint controls use stronger non-linear endpoint response (with gentler center response) for more creative looks while staying parity-aligned.
- In the FFmpeg `coloradj_RGB` bridge path, export applies conservative tint-delta attenuation for cross-runtime drift reduction. Cool-side temperature parity gain remains available as a runtime-tunable hook and now supports piecewise cool-range shaping (`US_EXPORT_COOL_TEMP_GAIN_FAR`, `US_EXPORT_COOL_TEMP_GAIN_NEAR`, plus legacy `US_EXPORT_COOL_TEMP_GAIN` fallback). Defaults remain unity unless explicitly tuned for a parity campaign. Tonal (shadows/midtones/highlights) residuals remain under active calibration.
- Audio is mixed from all non-muted audio tracks plus eligible embedded audio in video clips; freeze-frame video clips do not contribute embedded audio.
- Export runs in a background thread; the UI remains responsive.
