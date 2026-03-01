---
layout: doc
title: Inspector
permalink: /docs/inspector/
---

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
| **Volume** | 0.0 → 2.0 | 1.0 | Per-clip volume multiplier |
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
| **Rotate** | 0° / 90° / 180° / 270° | Rotation preset |
| **Flip H** | Toggle | Mirror horizontally |
| **Flip V** | Toggle | Mirror vertically |

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

Controls the playback speed of the clip. Changing speed adjusts the clip's width on the timeline proportionally.

| Slider | Range | Default | Effect |
|---|---|---|---|
| **Speed Multiplier** | 0.25× → 4.0× | 1.0× | 0.5× = slow-motion, 2.0× = fast-forward |

Marks at **½×**, **1×**, **2×** for quick snapping.

Preview uses GStreamer rate-seek. Export uses `setpts` (video) and chained `atempo` (audio).

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

## Notes

- All Inspector values are **persisted in the FCPXML** project file.
- There is no keyboard shortcut to focus the Inspector; use mouse/Tab navigation.
- The Inspector is empty when no clip is selected.
