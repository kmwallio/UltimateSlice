# Inspector

The **Inspector** panel (right side) shows and edits the properties of the currently selected timeline clip.

Select a clip in the timeline to populate the Inspector. All changes apply immediately to the program monitor preview.

---

## Clip Info

| Field | Description |
|---|---|
| **Name** | Editable label for the clip; click **Apply Name** to commit |
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
| **Brightness** | −1.0 → 1.0 | 0.0 | Additive luminance shift |
| **Contrast** | 0.0 → 2.0 | 1.0 | Contrast multiplier |
| **Saturation** | 0.0 → 2.0 | 1.0 | 0 = greyscale, 2 = vivid |
| **Temperature** | 2000 → 10000 K | 6500 | Color temperature: low = warm/amber, high = cool/blue |
| **Tint** | −1.0 → 1.0 | 0.0 | Green–magenta axis: negative = green, positive = magenta |

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

> **Applied on export** — The LUT is applied via FFmpeg's `lut3d` filter during export (full quality). Preview playback does not apply the LUT. A cyan **LUT** badge appears on the clip in the timeline when a LUT is assigned.

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
- Transform fields (scale/position/rotation), opacity, and crop are now also mapped to standard FCPXML adjustment elements (`adjust-transform`, `adjust-compositing`, `adjust-crop`/`crop-rect`) for improved interoperability.
- Clip volume now also imports from standard FCPXML `adjust-volume@amount` (including dB values such as `-6dB` and `-96dB`) and is converted to Inspector linear volume.
- There is no keyboard shortcut to focus the Inspector; use mouse/Tab navigation.
- When no clip is selected, the Inspector shows an instructional message and hides edit controls.
- To reduce first-use visual density, **Audio**, **Transform**, and **Speed** sections start collapsed by default.
