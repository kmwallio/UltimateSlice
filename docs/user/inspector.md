# Inspector

The **Inspector** panel (right side) shows and edits the properties of the currently selected timeline clip.

Select a clip in the timeline to populate the Inspector. All changes apply immediately to the program monitor preview.

---

## Clip Info

| Field | Description |
|---|---|
| **Name** | Editable label for the clip; click **Apply Name** to commit |
| **Clip Color Label** | Semantic timeline color tag (`None`, `Red`, `Orange`, `Yellow`, `Green`, `Teal`, `Blue`, `Purple`, `Magenta`) |
| **Source** | Full source file path (selectable, with ellipsis when narrow) |
| **Media Status** | `Online` when source exists, `Offline` when source file is missing |
| **In** | Source In-point |
| **Out** | Source Out-point |
| **Duration** | Source duration |
| **Timeline Start** | Position of the clip on the timeline |

---

## Color Correction

Adjustments are applied live via GStreamer `videobalance` and rendered through ffmpeg on export.

| Slider | Range | Default | Effect |
|---|---|---|---|
| **Exposure** | −1.0 → 1.0 | 0.0 | Overall exposure compensation |
| **Brightness** | −1.0 → 1.0 | 0.0 | Additive luminance shift |
| **Contrast** | 0.0 → 2.0 | 1.0 | Contrast multiplier |
| **Saturation** | 0.0 → 2.0 | 1.0 | 0 = greyscale, 2 = vivid |
| **Temperature** | 2000 → 10000 K | 6500 | Color temperature: low = warm/amber, high = cool/blue |
| **Tint** | −1.0 → 1.0 | 0.0 | Green–magenta axis: negative = green, positive = magenta |
| **Black Point** | −1.0 → 1.0 | 0.0 | Lifts or crushes black levels |
| **Highlights Warmth** | −1.0 → 1.0 | 0.0 | Warm/cool shift in highlights (left = cooler, right = warmer) |
| **Highlights Tint** | −1.0 → 1.0 | 0.0 | Green/magenta shift in highlights |
| **Midtones Warmth** | −1.0 → 1.0 | 0.0 | Warm/cool shift in midtones (left = cooler, right = warmer) |
| **Midtones Tint** | −1.0 → 1.0 | 0.0 | Green/magenta shift in midtones |
| **Shadows Warmth** | −1.0 → 1.0 | 0.0 | Warm/cool shift in shadows (left = cooler, right = warmer) |
| **Shadows Tint** | −1.0 → 1.0 | 0.0 | Green/magenta shift in shadows |

> **Note:** Exposure uses preview-aligned brightness/contrast deltas in both Program Monitor and export (not gamma-only mapping). Per-tone Warmth/Tint uses a non-linear response with fine control near `0.0` and stronger effect at slider ends for creative grading (e.g., cooler shadows, warmer highlights). Preview applies GStreamer's frei0r `3-point-color-balance` plugin, which uses quadratic (parabola) interpolation internally. Export now emits matching FFmpeg `lutrgb` expressions with identical parabola coefficients for near-exact preview/export parity. Shadows warmth/tint endpoints are intentionally stronger for more pronounced creative looks at slider extremes.

---

## Denoise / Sharpness / Blur

Applied via GStreamer `gaussianblur` (preview) and ffmpeg `hqdn3d`/`unsharp`/`boxblur` (export).

| Slider | Range | Default | Effect |
|---|---|---|---|
| **Denoise** | 0.0 → 1.0 | 0.0 | Gaussian blur strength (noise reduction) |
| **Sharpness** | −1.0 → 1.0 | 0.0 | Negative = soften, positive = sharpen |
| **Blur** | 0.0 → 1.0 | 0.0 | Creative blur (censoring, depth-of-field, background defocus). Preview via gaussianblur, export via boxblur. Supports keyframe animation. |

## Stabilization

Video stabilization compensates camera shake using ffmpeg's libvidstab (two-pass workflow). When **proxy mode is enabled**, stabilization is baked into the proxy transcode so the effect is visible in the Program Monitor preview. Without proxies, stabilization is applied on export only.

| Control | Type | Default | Effect |
|---|---|---|---|
| **Enable** | Checkbox | Off | Toggle stabilization for this clip |
| **Smoothing** | 0.0 → 1.0 | 0.5 | Higher = smoother (less shake) but may crop edges. Maps to vidstab shakiness (1–10) for analysis and smoothing (1–30) for transform |

- Pass 1 (analysis): `vidstabdetect` runs during export to detect motion vectors
- Pass 2 (transform): `vidstabtransform` applies stabilizing corrections with post-sharpening (`unsharp`) to compensate for slight softening
- If ffmpeg lacks libvidstab, stabilization is silently skipped
- Persists in FCPXML via `us:vidstab-enabled` and `us:vidstab-smoothing` vendor attributes

---

## Audio

| Slider | Range | Default | Effect |
|---|---|---|---|
| **Volume** | −100 dB → +12 dB | 0 dB | Per-clip gain (`0 dB = 1.0x`, `-96 dB`/`-100 dB` ≈ mute) |
| **Pan** | −1.0 → 1.0 | 0.0 | Stereo position (−1 = full left, +1 = full right) |

### Normalize Audio

The **Normalize...** button (next to the volume slider) analyzes the clip's loudness using FFmpeg's EBU R128 measurement and adjusts the volume to hit **-14 LUFS** (YouTube/streaming standard). After analysis, the measured loudness is displayed (e.g., "−18.3 LUFS") and the volume slider updates to the normalized value. Fully undo-able.

MCP tool: `normalize_clip_audio` supports `mode` (`lufs` or `peak`) and `target_level` (e.g., `-14.0` for LUFS, `0.0` for peak dBFS).

### Audio keyframes (phase 1)

- Use **Set Volume Keyframe** / **Remove Volume Keyframe** for the volume lane.
- Use **Set Pan Keyframe** / **Remove Pan Keyframe** for the pan lane.
- Interpolation mode is selected in the transform section's **Interpolation** dropdown.

### Audio keyframe navigation

- **◀ Prev KF / Next KF ▶** buttons navigate between audio keyframes (volume and pan).
- **◆ Aud KF** indicator shows when the playhead is on an audio keyframe.
- **⏺ Record Keyframes** toggle in the audio section is synced with the transform section toggle — activating either enables animation mode for both sections. When active, volume and pan slider changes auto-create keyframes at the playhead position.

### Equalizer (3-band parametric)

Collapsible section inside Audio with three bands: **Low**, **Mid**, **High**.

| Parameter | Range | Defaults (Low / Mid / High) |
|---|---|---|
| **Freq (Hz)** | 20–1000 / 200–8000 / 1000–20000 | 200 / 1000 / 5000 |
| **Gain (dB)** | −24 → +24 | 0 (flat) |
| **Q** | 0.1 → 10.0 | 1.0 |

- Preview: GStreamer `equalizer-nbands` element (real-time parameter updates).
- Export: chained FFmpeg `equalizer` filters with per-band frequency, bandwidth, and gain.
- Gain per band supports keyframe animation via `eq_low_gain`, `eq_mid_gain`, `eq_high_gain` keyframe properties.
- MCP tool: `set_clip_eq` with 9 optional parameters.

---

## Video Transform

Applied via GStreamer `videocrop`, `videoflip`, `videoscale`, and `videobox` (preview) and ffmpeg filters (export).

| Control | Options | Description |
|---|---|---|
| **Blend Mode** | Dropdown | Compositing blend mode: Normal (default), Multiply, Screen, Overlay, Add, Difference, Soft Light. Preview blends against real lower layers via compositor probe; export uses ffmpeg `blend` filter |
| **Anamorphic Desqueeze** | Dropdown | Lens desqueeze factor: None (1.0x), 1.33x, 1.5x, 1.8x, 2.0x. Applies non-square pixel aspect ratio for anamorphic footage |
| **Scale** | 0.1 → 4.0 | Zoom factor. 1.0 = normal, 2.0 = 2× zoom in (crops), 0.5 = half size (letterbox/pillarbox) |
| **Opacity** | 0.0 → 1.0 | Layer blend amount. 1.0 = fully opaque, 0.0 = fully transparent |
| **Position X** | −1.0 → 1.0 | Horizontal offset within the frame. 0.0 = center, −1.0 = full left, 1.0 = full right |
| **Position Y** | −1.0 → 1.0 | Vertical offset within the frame. 0.0 = center, −1.0 = full top, 1.0 = full bottom |
| **Crop Left/Right/Top/Bottom** | 0 → 500 px | Crop pixels from each edge |
| **Rotate** | Dial/knob + numeric angle (−180° → 180°) | Arbitrary-angle rotation |
| **Flip H** | Toggle | Mirror horizontally |
| **Flip V** | Toggle | Mirror vertically |

> **Adjustment layers:** the same transform controls define the adjustment clip's scoped effect region in preview and export. **Scale, Position, Crop, Rotate, and Opacity** stay active; **Blend Mode**, **Anamorphic Desqueeze**, and **Flip H/V** are shown but disabled because adjustment clips do not create their own image layer.

### Transform keyframes (phase 1)

- Transform controls include keyframe buttons for **Scale**, **Opacity**, **Position X**, and **Position Y**.
- **Set … Keyframe** writes the current slider value at the current playhead time using the selected interpolation mode.
- **Remove … Keyframe** removes a keyframe at that same playhead time.
- `rotate` and `crop_*` keyframe lanes are authorable via MCP (`set_clip_keyframe` / `remove_clip_keyframe`), respected by Program Monitor preview, applied on export, and persisted in project FCPXML.

### Interpolation modes

The **Interpolation** dropdown selects how values transition between adjacent keyframes:

| Mode | Behavior |
|---|---|
| **Linear** (default) | Constant rate of change |
| **Ease In** | Slow start, accelerates toward end |
| **Ease Out** | Fast start, decelerates toward end |
| **Ease In/Out** | Smooth acceleration and deceleration |

The selected mode applies to new keyframes created by "Set Keyframe" buttons, animation mode auto-keyframes, and MCP. When the playhead lands on an existing keyframe, the dropdown reflects that keyframe's interpolation. FCPXML round-trip preserves interpolation modes (`interp` attribute).

For strict native `.fcpxml` workflows, custom handle-authored segments are also exported with native keyframe `curve="smooth"` metadata, and imported `curve="smooth"` keyframes are mapped back into Bezier controls so non-linear segment intent survives beyond vendor-only attrs.

### Keyframe navigation

- **◀ Prev KF / Next KF ▶** buttons jump the playhead to the previous/next keyframe across all properties of the selected clip.
- **◆ Keyframe** indicator shows when the playhead is exactly on (within half a frame of) a keyframe.
- Keyboard shortcuts: **Alt+Left** / **Alt+Right** to navigate between keyframes.
- Click a keyframe tick on the timeline clip body to select the clip and seek the playhead to that keyframe.

### Keyframes panel (dopesheet, phase 1)

- A dedicated **Hide/Show Keyframes** button appears on the right side of the timeline track-management bar.
- The dopesheet appears as a resizable panel between the timeline tracks and the track-management bar. Drag the split handle to resize.
- The panel renders a per-lane dopesheet for the selected clip: **Scale, Opacity, Position X/Y, Volume, Pan, Speed, Rotate, Crop Left/Right/Top/Bottom**.
- Use lane checkboxes to show/hide lanes while editing.
- Lanes now include a value-curve overlay so you can see both **timing** and **value shape** per property.
- Selected keyframe segments expose Bezier handles in the dopesheet; dragging a handle updates the segment shape live and preserves exact tangent controls for preview/runtime (the interpolation preset UI reflects the nearest mode).
- Click a keyframe point to select it; drag it horizontally to retime it within the clip duration.
- **Ctrl/Cmd+Click** toggles a keyframe in the current selection.
- **Shift+Click** on a keyframe selects a same-lane range between the current anchor keyframe and the clicked keyframe.
- **Add @ Playhead** inserts/updates a keyframe on the selected lane at the current playhead position, using the panel's interpolation mode.
- **Remove** deletes the current keyframe selection.
- **Apply Interp** applies the chosen interpolation mode to the current keyframe selection.
- Keyboard edits (when the keyframes panel has focus): **Delete/Backspace** removes selected keyframe(s); **Left/Right** nudges selected keyframe(s) by 1 frame; **Shift+Left/Right** nudges selected keyframe(s) by 10 frames.
- Time-scale controls: **− / + / 100%** buttons adjust or reset dopesheet zoom. **Ctrl+Scroll** also zooms the dopesheet, centered around the playhead position.
- Dopesheet panning: use the mouse wheel/trackpad scroll gesture over the panel to pan across time when zoomed in.
- Keyframe panel edits participate in global undo/redo history.

### Animation mode (Record Keyframes)

- The **⏺ Record Keyframes** toggle button (or **Shift+K**) activates animation mode.
- When active, transform overlay drags automatically create/update **Scale**, **Position X**, and **Position Y** keyframes at the current playhead position when the drag ends.
- Inspector slider changes (Scale, Opacity, Position X/Y, Volume, Pan) also auto-create keyframes.
- When inactive (default), drags and slider changes modify the clip's static values without creating keyframes.

Program Monitor overlay integration:
- Drag **corner handles** to scale (hold **Shift** for constrained scaling).
- Drag the **orange rotation handle** above the clip box to set rotation angle.
- Drag **edge midpoint handles** to adjust crop left/right/top/bottom directly in preview.
- Use keyboard nudges in the Program Monitor overlay for fine position/scale adjustments.

---

## Title / Text Overlay

Rendered via GStreamer `textoverlay` (preview) and FFmpeg `drawtext` (export).

| Field | Description |
|---|---|
| **Text** | The overlay text (leave empty to hide) |
| **Font** | Click to choose a font (Pango font description) |
| **Text Color** | Color picker with alpha support |
| **Position X** | 0.0 (left) → 1.0 (right) |
| **Position Y** | 0.0 (top) → 1.0 (bottom) |
| **Outline Width** | Stroke width in pts (0 = none) |
| **Outline Color** | Stroke color with alpha |
| **Drop Shadow** | Enable/disable drop shadow |
| **Shadow Color** | Shadow color with alpha |
| **Shadow Offset X/Y** | Shadow offset in pts |
| **Background Box** | Enable/disable background box behind text |
| **Box Color** | Background box color with alpha |
| **Box Padding** | Padding around text in pts |

Default: white `Sans Bold 36`, bottom-centre (`x=0.5, y=0.9`).

> **Resolution-independent sizing** — Font size is specified in Pango points at a 1080p reference height. Both GStreamer preview and FFmpeg export scale the effective font size proportionally to the actual rendering height, so title text appears at the same relative size regardless of output resolution (720p, 1080p, 4K, etc.).

> **Live preview** — All title styling controls update the GStreamer preview immediately. Property changes are non-blocking; the compositor flush is fire-and-forget with 32ms debouncing, so rapid typing and slider drags feel smooth even on multi-clip timelines.

> **Resolution-independent sizing** — Title font size is calculated to match the export output regardless of preview quality or window size. The font size specified in the inspector (e.g. "Sans Bold 36") defines the visual size at 1080p; the preview and export both scale proportionally to the actual rendering height.

> **Preview limitations** — GStreamer `textoverlay` has fixed outline width (~1px), fixed shadow offset, and a single dark shaded-background style. The FFmpeg export renders all styling options at full fidelity.

---

## Speed

Controls the playback speed and direction of the clip. Changing speed adjusts the clip's width on the timeline proportionally.

| Control | Range / Values | Default | Effect |
|---|---|---|---|
| **Speed Multiplier** | 0.25× → 4.0× | 1.0× | 0.5× = slow-motion, 2.0× = fast-forward |
| **Reverse** | Checkbox | Off | Play the clip backwards (reversed frame order) |
| **Slow-Motion Interpolation** | Off / Frame Blending / Optical Flow | Off | Synthesizes intermediate frames on export for smooth slow-motion (speed < 1.0 only) |

Marks at **½×**, **1×**, **2×** for quick snapping.

### Slow-motion interpolation

When a clip's speed is below 1.0, the default behavior repeats frames to fill the longer duration, which can look stuttery. The **Slow-Motion Interpolation** dropdown offers two alternatives:

- **Frame Blending** — fast temporal averaging (`minterpolate mi_mode=blend`). Produces a slight motion-blur effect between frames.
- **Optical Flow** — motion-compensated interpolation (`minterpolate mi_mode=mci`). Slower to encode but produces the smoothest result by synthesizing true intermediate frames.

This is an **export-only** feature — `minterpolate` is too CPU-intensive for real-time preview. Background prerender includes it when enabled. The setting has no effect when speed is 1.0 or higher. Persists via FCPXML.

### Variable speed ramps

You can create speed ramps (variable speed within a single clip) by adding **Speed** keyframes in the Keyframes panel dopesheet. For example, ramping from 1× to 2× creates a smooth acceleration effect.

- Open the **Keyframes** panel and enable the **Speed** lane.
- Position the playhead and add keyframes at the desired times with different speed values.
- The clip's timeline duration automatically adjusts to account for the varying speed — a ramp from 1× to 0.5× will make the clip longer, while 1× to 2× will make it shorter.
- Playback in the Program Monitor reflects the speed curve in real time.
- Speed keyframes support all interpolation modes (Linear, Ease In, Ease Out, Ease In/Out) and custom Bezier curves for fine-tuned ramp shaping.

A yellow **⏲ Ramp** badge appears on clips with speed keyframes. Reversed speed-ramped clips show **⏲ ◀ Ramp**.

### Constant speed

Program Monitor preview uses GStreamer rate-seek (including reverse direction). Export uses `reverse`/`areverse` (when reversed) and `setpts`/chained `atempo` (audio) for speed.

A yellow **◀** badge appears on reversed clips in the timeline (e.g. **◀ 2×** for a reversed 2× speed clip). Reverse can be combined with any speed multiplier.

---

## Color LUT

Assigns a 3D Look-Up Table (LUT) file for professional color grading. LUTs remap colors globally for cinematic looks, log-to-Rec.709 conversions, and other grade transformations.

| Control | Description |
|---|---|
| **LUT filename** | Shows the assigned `.cube` filename, or "None" if no LUT is set. |
| **Import LUT…** | Opens a file chooser filtered to `.cube` files. Select a LUT to assign it to the selected clip. |
| **Clear** | Removes the currently assigned LUT from the clip. |

> **Real-time preview & export** — The LUT is applied in the Program Monitor via CPU-based trilinear interpolation at preview resolution, providing immediate visual feedback. On export, it is applied via FFmpeg's `lut3d` filter at full resolution. When proxy mode or Preview LUTs is enabled and the proxy is ready, the LUT is already baked into the proxy media — the real-time probe is automatically skipped to prevent double-application. A cyan **LUT** badge appears on the clip in the timeline when a LUT is assigned.

---

## Copy & Paste Color Grade

You can copy all color grading values from one clip and paste them onto another. This copies only color-related properties — not audio, transforms, or other attributes.

| Shortcut | Action |
|---|---|
| **Ctrl+Alt+C** | Copy color grade from the selected clip |
| **Ctrl+Alt+V** | Paste color grade onto the selected clip |

**Copied properties:** Brightness, Contrast, Saturation, Temperature, Tint, Exposure, Black Point, Shadows, Midtones, Highlights, per-tone Warmth/Tint, Denoise, Sharpness, Blur, and LUT path. Static values only — keyframe animations are not included.

> **Tip:** Use **Ctrl+Shift+V** (Paste Attributes) to copy *all* clip attributes including audio, transforms, and effects. Use **Ctrl+Alt+V** (Paste Color Grade) when you only want to match the color look between clips.

> **MCP tools:** `copy_clip_color_grade` and `paste_clip_color_grade` provide the same functionality for automation.

Only `.cube` format (3D LUT) is supported. One LUT per clip; multiple-LUT stacking is a future feature.

---

## Match Clip Colors

Automatically adjusts the selected clip's color parameters so it visually matches a reference clip. Uses Reinhard-style statistical color transfer in CIE L\*a\*b\* space.

### How It Works

1. Samples representative frames from both the selected clip (source) and the chosen reference clip.
2. Computes per-channel mean and standard deviation in L\*a\*b\* color space.
3. Maps the statistical differences onto existing clip color sliders (brightness, contrast, saturation, temperature, tint).
4. Optionally generates a 17³ 3D `.cube` LUT for fine-grained matching that sliders alone cannot express.

### Using Match Color

| Control | Description |
|---|---|
| **Match Color…** button | In the Inspector Color Correction section. Opens a dialog to select a reference clip and run matching. |
| **Reference clip** | Dropdown of all other video/image clips in the project. |
| **Generate LUT** | Optional checkbox. When enabled, a 3D LUT is generated and assigned to the clip in addition to slider adjustments. |
| **Ctrl+Alt+M** | Keyboard shortcut — opens the same dialog for the selected timeline clip. |

Match Color adjusts **all** color parameters: global controls (brightness, contrast, saturation, temperature, tint, exposure) **and** per-zone grading (shadows, midtones, highlights brightness, per-zone warmth/tint, and black point). Zone grading is estimated by classifying pixels into shadow/midtone/highlight luminance bands and computing the residual difference not covered by global adjustments.

If the reference clip has existing color grading (sliders or a LUT), the match targets the **graded appearance** — not the raw source footage. When "Generate LUT" is enabled, the LUT captures only the non-linear residual that sliders cannot express, avoiding double-application of color corrections.

> **Undo support:** The entire operation (all slider changes + optional LUT assignment) is undoable in a single `Ctrl+Z` step.

> **MCP tool:** `match_clip_colors` provides the same functionality for automation. Parameters: `source_clip_id`, `reference_clip_id`, `generate_lut` (optional boolean).

---

## Chroma Key

Removes a target color (green screen / blue screen) from the clip, making those pixels transparent so lower video tracks show through the compositor pipeline.

| Control | Range | Default | Description |
|---|---|---|---|
| **Enable Chroma Key** | on/off | off | Activates/deactivates chroma keying for this clip |
| **Key Color** | Green / Blue / Custom | Green | Target color to make transparent; Custom shows a hex entry |
| **Tolerance** | 0.0 → 1.0 | 0.3 | How far from the target color to key out (higher = wider range) |
| **Edge Softness** | 0.0 → 1.0 | 0.1 | Softens the key edge for smoother blending (higher = softer) |

> **Pipeline placement** — Chroma key is applied after color correction (so you can white-balance a green screen clip before keying) but before crop/rotation. Preview uses GStreamer's `alpha` element; export uses FFmpeg's `colorkey` filter.

Place the chroma-keyed clip on an upper video track with the background on a lower track. The compositor automatically composites transparent regions.

---

## Shape Mask

Restricts the visible area of a clip using a geometric shape. Pixels outside the mask become transparent, revealing lower video tracks through the compositor.

| Control | Range | Default | Description |
|---|---|---|---|
| **Enable Mask** | on/off | off | Activates/deactivates shape masking |
| **Shape** | Rectangle / Ellipse / Path | Rectangle | Shape type for the mask region |
| **Center X** | 0.0 → 1.0 | 0.5 | Horizontal mask center (0 = left, 1 = right) |
| **Center Y** | 0.0 → 1.0 | 0.5 | Vertical mask center (0 = top, 1 = bottom) |
| **Width** | 0.01 → 0.5 | 0.25 | Half-width of the mask (normalized) |
| **Height** | 0.01 → 0.5 | 0.25 | Half-height of the mask (normalized) |
| **Rotation** | −180° → 180° | 0° | Rotate the mask shape |
| **Feather** | 0.0 → 0.5 | 0.0 | Edge softness (SDF-based smoothstep falloff) |
| **Expansion** | −0.5 → 0.5 | 0.0 | Grow or shrink the mask boundary |
| **Invert Mask** | on/off | off | Show area outside the mask instead of inside |

> **Pipeline placement** — The mask is applied after crop and LUT but before color effects and chroma key. It operates in pre-transform clip space, so the mask moves with the clip's scale/position/rotation. Preview uses a GStreamer RGBA pad probe with SDF alpha computation; export uses FFmpeg `geq` expressions (rect/ellipse) or rasterized grayscale PGM with `movie`/`alphamerge` (path).

All numeric mask properties (rect/ellipse) support keyframe animation via the Phase 1 keyframe system.

The Program Monitor transform overlay shows a cyan dashed outline of the active mask shape with a center crosshair.

### Path (Bezier) Masks

When **Path** is selected, the Center/Width/Height/Rotation sliders are hidden and replaced by a **path point editor**:

- Each point has X/Y coordinates (normalized 0.0–1.0) defining the anchor position
- Each point has incoming and outgoing bezier tangent handles (relative offsets)
- The path is always closed (last point connects back to first)
- Minimum 3 points required for a valid path
- **Add Point** button appends a new anchor at (0.5, 0.5)
- **×** button on each point removes it (if more than 3 points remain)

A default 4-point diamond shape is created when Path is first selected. Zero tangent handles produce straight line segments; drag handles in the overlay to add curvature.

Feather, expansion, and invert controls apply to path masks the same as rect/ellipse masks.

---

## Applied Effects (Frei0r Plugins)

The **Applied Effects** section appears below Color Correction when a clip has frei0r effects applied. Effects are discovered from the system's GStreamer frei0r plugin registry (~130+ filters on a typical Linux system).

### Adding effects

Open the **Effects Browser** tab in the left panel (tab peer of Media Browser). Effects are organized by category with a search filter. Double-click a plugin or click **Apply** to add it to the selected clip. Effects stack in order — first applied is processed first.

### Per-effect controls

Each applied effect row shows:

| Control | Description |
|---------|-------------|
| **Enable toggle** | Bypass the effect without removing it |
| **Effect name** | Human-friendly plugin name |
| **↑ / ↓** buttons | Reorder within the effect chain |
| **×** button | Remove the effect |
| **Parameters** (collapsible) | Click "Parameters" expander to reveal controls |
| **Numeric sliders** | Frei0r double parameters (0.0–1.0 normalized range) |
| **Boolean toggles** | CheckButton for on/off parameters |
| **String dropdowns** | DropDown for enum string parameters (e.g. blend-mode with values like "normal", "multiply", "screen") |
| **String entries** | Text entry for free-form string parameters |

### Preview & Export

- **Preview**: GStreamer `frei0r-filter-*` elements inserted after the built-in color pipeline (brightness/contrast/saturation/LUT/temperature/tint/grading/denoise/sharpness/blur) and before chroma key. Parameter changes update live without pipeline rebuild.
- **Export**: FFmpeg `frei0r=filter_name={name}:filter_params={p1}|{p2}|...` filter chain. Parameters are passed in registry-defined order.

### MCP tools

- `list_frei0r_plugins` — enumerate available plugins with parameter metadata
- `add_clip_frei0r_effect` — apply a plugin to a clip
- `remove_clip_frei0r_effect` — remove an effect instance
- `set_clip_frei0r_effect_params` — update effect parameters
- `reorder_clip_frei0r_effects` — reorder the effect chain
- `list_clip_frei0r_effects` — list effects applied to a clip

All effect operations are **undoable** (add, remove, reorder, set params, toggle).

---

## Notes

- All Inspector values are **persisted in the FCPXML** project file.
- Phase-1 keyframes are persisted via UltimateSlice vendor attributes (`us:scale-keyframes`, `us:opacity-keyframes`, `us:position-x-keyframes`, `us:position-y-keyframes`, `us:volume-keyframes`, `us:pan-keyframes`, `us:rotate-keyframes`, `us:crop-left-keyframes`, `us:crop-right-keyframes`, `us:crop-top-keyframes`, `us:crop-bottom-keyframes`).
- Transform fields (scale/position/rotation), opacity, and crop are now also mapped to standard FCPXML adjustment elements (`adjust-transform`, `adjust-compositing`, `adjust-crop`/`crop-rect`) for improved interoperability.
- Clip volume now also imports from standard FCPXML `adjust-volume@amount` (including dB values such as `-6dB` and `-96dB`) and is converted to Inspector linear volume.
- There is no keyboard shortcut to focus the Inspector; use mouse/Tab navigation.
- When no clip is selected, the Inspector shows an instructional message and hides edit controls.
- To reduce first-use visual density, **Audio**, **Transform**, and **Speed** sections start collapsed by default.
