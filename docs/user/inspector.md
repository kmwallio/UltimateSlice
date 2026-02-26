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

Applied via GStreamer `videocrop` and `videoflip` (preview) and ffmpeg filters (export).

| Control | Options | Description |
|---|---|---|
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

## Notes

- All Inspector values are **persisted in the FCPXML** project file.
- There is no keyboard shortcut to focus the Inspector; use mouse/Tab navigation.
- The Inspector is empty when no clip is selected.
