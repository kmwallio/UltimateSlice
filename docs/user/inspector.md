# Inspector

The **Inspector** panel (right side) shows and edits the properties of the currently selected timeline clip.

Select a clip in the timeline to populate the Inspector. All changes apply immediately to the program monitor preview.

---

## Clip Info

| Field | Description |
|---|---|
| **Name** | Editable label for the clip; click **Apply Name** to commit |
| **Clip Color Label** | Semantic timeline color tag (`None`, `Red`, `Orange`, `Yellow`, `Green`, `Teal`, `Blue`, `Purple`, `Magenta`) |
| **File** | Source filename |
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

## Denoise & Sharpness

Applied via GStreamer `gaussianblur` (preview) and ffmpeg `hqdn3d`/`unsharp` (export).

| Slider | Range | Default | Effect |
|---|---|---|---|
| **Denoise** | 0.0 → 1.0 | 0.0 | Gaussian blur strength (noise reduction) |
| **Sharpness** | −1.0 → 1.0 | 0.0 | Negative = soften, positive = sharpen |

---

## Audio

| Slider | Range | Default | Effect |
|---|---|---|---|
| **Volume** | −100 dB → +12 dB | 0 dB | Per-clip gain (`0 dB = 1.0x`, `-96 dB`/`-100 dB` ≈ mute) |
| **Pan** | −1.0 → 1.0 | 0.0 | Stereo position (−1 = full left, +1 = full right) |

### Audio keyframes (phase 1)

- Use **Set Volume Keyframe** / **Remove Volume Keyframe** for the volume lane.
- Use **Set Pan Keyframe** / **Remove Pan Keyframe** for the pan lane.
- Interpolation mode is selected in the transform section's **Interpolation** dropdown.

### Audio keyframe navigation

- **◀ Prev KF / Next KF ▶** buttons navigate between audio keyframes (volume and pan).
- **◆ Aud KF** indicator shows when the playhead is on an audio keyframe.
- **⏺ Record Keyframes** toggle in the audio section is synced with the transform section toggle — activating either enables animation mode for both sections. When active, volume and pan slider changes auto-create keyframes at the playhead position.

---

## Video Transform

Applied via GStreamer `videocrop`, `videoflip`, `videoscale`, and `videobox` (preview) and ffmpeg filters (export).

| Control | Options | Description |
|---|---|---|
| **Scale** | 0.1 → 4.0 | Zoom factor. 1.0 = normal, 2.0 = 2× zoom in (crops), 0.5 = half size (letterbox/pillarbox) |
| **Opacity** | 0.0 → 1.0 | Layer blend amount. 1.0 = fully opaque, 0.0 = fully transparent |
| **Position X** | −1.0 → 1.0 | Horizontal offset within the frame. 0.0 = center, −1.0 = full left, 1.0 = full right |
| **Position Y** | −1.0 → 1.0 | Vertical offset within the frame. 0.0 = center, −1.0 = full top, 1.0 = full bottom |
| **Crop Left/Right/Top/Bottom** | 0 → 500 px | Crop pixels from each edge |
| **Rotate** | Dial/knob + numeric angle (−180° → 180°) | Arbitrary-angle rotation |
| **Flip H** | Toggle | Mirror horizontally |
| **Flip V** | Toggle | Mirror vertically |

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

Rendered via GStreamer `textoverlay` (preview) and composited on export.

| Field | Description |
|---|---|
| **Text** | The overlay text (leave empty to hide) |
| **Position X** | 0.0 (left) → 1.0 (right) |
| **Position Y** | 0.0 (top) → 1.0 (bottom) |

Default: white `Sans Bold 36`, bottom-centre (`x=0.5, y=0.9`).

---

## Speed

Controls the playback speed and direction of the clip. Changing speed adjusts the clip's width on the timeline proportionally.

| Control | Range / Values | Default | Effect |
|---|---|---|---|
| **Speed Multiplier** | 0.25× → 4.0× | 1.0× | 0.5× = slow-motion, 2.0× = fast-forward |
| **Reverse** | Checkbox | Off | Play the clip backwards (reversed frame order) |

Marks at **½×**, **1×**, **2×** for quick snapping.

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

Only `.cube` format (3D LUT) is supported. One LUT per clip; multiple-LUT stacking is a future feature.

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

## Notes

- All Inspector values are **persisted in the FCPXML** project file.
- Phase-1 keyframes are persisted via UltimateSlice vendor attributes (`us:scale-keyframes`, `us:opacity-keyframes`, `us:position-x-keyframes`, `us:position-y-keyframes`, `us:volume-keyframes`, `us:pan-keyframes`, `us:rotate-keyframes`, `us:crop-left-keyframes`, `us:crop-right-keyframes`, `us:crop-top-keyframes`, `us:crop-bottom-keyframes`).
- Transform fields (scale/position/rotation), opacity, and crop are now also mapped to standard FCPXML adjustment elements (`adjust-transform`, `adjust-compositing`, `adjust-crop`/`crop-rect`) for improved interoperability.
- Clip volume now also imports from standard FCPXML `adjust-volume@amount` (including dB values such as `-6dB` and `-96dB`) and is converted to Inspector linear volume.
- There is no keyboard shortcut to focus the Inspector; use mouse/Tab navigation.
- When no clip is selected, the Inspector shows an instructional message and hides edit controls.
- To reduce first-use visual density, **Audio**, **Transform**, and **Speed** sections start collapsed by default.
