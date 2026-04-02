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

### Automatic Audio Crossfades

Export honors Timeline crossfade preferences (set in Preferences → Timeline, or via MCP `set_crossfade_settings`):

- `crossfade_enabled`
- `crossfade_curve` (Equal power or Linear)
- `crossfade_duration_ns`

When enabled, export applies automatic fades at adjacent same-track audio edit points for:

- clips on non-muted audio tracks
- embedded audio in eligible video clips (when embedded audio is not suppressed by linked audio peers, clip audio is present, and the clip is not a freeze-frame hold)

Fade lengths are clamped safely for very short clips and overlap boundaries so exports remain stable.

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
- A progress bar driven by ffmpeg progress output. It estimates final file size from bitrate × duration and then tracks ffmpeg `total_size` against that estimate.
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

When **Slow-Motion Interpolation** is enabled in the Inspector (Frame Blending or Optical Flow), export appends `minterpolate` after the speed filter for clips with effective speed < 1.0:

- **Frame Blending** (`mi_mode=blend`): fast temporal averaging between frames.
- **Optical Flow** (`mi_mode=mci`): motion-compensated interpolation for the smoothest result (significantly slower to encode).

The filter is set to the project frame rate (`fps=NUM/DEN`) so synthesized frames match the output timeline. Normal-speed and fast clips are unaffected. Background prerender also applies minterpolate when enabled.

## Keyframed Properties

Export evaluates phase-1 clip keyframes with interpolation-aware curves:

- **Video:** `scale`, `position_x`, `position_y`, and `opacity`
- **Audio:** `volume`

Keyframes are evaluated in clip-local timeline time and rendered directly into ffmpeg filter chains so exported animation follows the same keyframe timing model used by Program Monitor preview. Dopesheet custom Bezier handle shapes are exported through a piecewise cubic-bezier approximation.

## Adjustment Layers

- Adjustment layers export as post-compositor effect passes over the assembled timeline image.
- The exported effect region uses the same **scale / position / crop / rotate / opacity** scope model as the Program Monitor overlay for adjustment clips.
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

**Export → Export OTIO...** opens a file dialog to save the `.otio` file.

Features:
- Clips with source media references (`file://` URLs)
- Explicit gaps between clips
- Transitions (cross dissolve mapped to `SMPTE_Dissolve`)
- Speed effects stored as `LinearTimeWarp` OTIO effects
- Project markers attached to the first video track
- Track metadata (muted, locked, soloed, audio role, ducking)
- UltimateSlice OTIO metadata currently preserves the supported clip metadata set, including core clip settings (`speed`, `reverse`, `opacity`, `volume`, `pan`, `brightness`, `contrast`, `saturation`)
- Title clips exported with `MissingReference`, plus title styling metadata (text, font, colors, outline, shadow, box, template, secondary text, and clip background color)
- Subtitle-bearing clips preserve subtitle segments/word timing plus subtitle styling metadata (language, font/colors, outline, background box, highlight mode/color, word window, and vertical position)
- Adjustment clips also export as `MissingReference`

**Import:** Open `.otio` files via **File → Open** (or MCP `open_otio` tool). OTIO files from other tools are imported with default clip properties; UltimateSlice metadata is restored when present. UltimateSlice also accepts older flat OTIO metadata from previous app builds and upgrades it to the current versioned OTIO metadata contract on save.

Current limitation: OTIO round-trip is still partial for some UltimateSlice-only features. Some advanced transforms/effects/keyframes/masks and nested clip internals are not fully preserved yet, so `.uspxml` remains the highest-fidelity interchange/save format for complete UltimateSlice projects.

Also available via MCP: `save_otio` and `open_otio` tools with `path` parameter.

## Notes

- Export requires **ffmpeg** to be installed and on `$PATH`.
- All video tracks are processed in timeline order, with letterbox/pillarbox padding applied to each clip.
- Secondary-track overlays keep transparent padding when zoomed out and honor per-clip opacity, so layered composites export closer to Program Monitor preview.
- Overlay clips positioned near frame edges (where the PIP extends beyond the output boundary) are correctly clipped to match the preview — the export pre-crops overflow before padding so the visible portion and position match exactly.
- Primary static color controls (`brightness`, `contrast`, `saturation`, plus static `exposure`) are mapped through the same calibrated primary-color model used by Program Monitor preview, including contrast-dependent brightness compensation, to improve low/high-contrast parity.
- Extended grading sliders (`shadows`, `midtones`, `highlights`, `exposure`, `black point`, and per-tone warmth/tint) now prioritize preview/export parity. When FFmpeg frei0r modules are available, export uses a bridge path aligned with Program Monitor’s calibrated grading mapping; otherwise it falls back to native FFmpeg grading filters. Tonal warmth/tint controls use stronger non-linear endpoint response (with gentler center response) for more creative looks while staying parity-aligned.
- In the FFmpeg `coloradj_RGB` bridge path, export applies conservative tint-delta attenuation for cross-runtime drift reduction. Cool-side temperature parity gain remains available as a runtime-tunable hook and now supports piecewise cool-range shaping (`US_EXPORT_COOL_TEMP_GAIN_FAR`, `US_EXPORT_COOL_TEMP_GAIN_NEAR`, plus legacy `US_EXPORT_COOL_TEMP_GAIN` fallback). Defaults remain unity unless explicitly tuned for a parity campaign. Tonal (shadows/midtones/highlights) residuals remain under active calibration.
- Audio is mixed from all non-muted audio tracks plus eligible embedded audio in video clips; freeze-frame video clips do not contribute embedded audio.
- Export runs in a background thread; the UI remains responsive.
