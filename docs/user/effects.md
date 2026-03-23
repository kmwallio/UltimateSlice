# Effects & Color Correction

UltimateSlice provides a comprehensive suite of video effects, color correction tools, and a plugin system for third-party frei0r filters. All effects update the program monitor preview in real-time and render at full fidelity on export.

---

## Built-in Effects Summary

| Category | Effects | Preview Engine | Export Engine |
|----------|---------|----------------|---------------|
| Color Correction | Brightness, Contrast, Saturation, Temperature, Tint, Exposure, Black Point | GStreamer `videobalance` + frei0r `coloradj_RGB` | FFmpeg `lutrgb` |
| Per-Tone Grading | Shadows/Midtones/Highlights Warmth & Tint | frei0r `3-point-color-balance` | FFmpeg `lutrgb` (matched parabola coefficients) |
| Denoise & Sharpen | Denoise, Sharpness | GStreamer `gaussianblur` | FFmpeg `hqdn3d` / `unsharp` |
| Chroma Key | Green/Blue/Custom screen removal | GStreamer `alpha` | FFmpeg `colorkey` |
| AI Background Removal | Neural-net foreground segmentation | ONNX Runtime inference | (preview only) |
| Transform | Scale, Opacity, Position, Crop, Rotate, Flip, Blend Mode | GStreamer compositor + `videocrop` | FFmpeg `scale` / `crop` / `overlay` / `blend` |
| Color LUT | 3D lookup table (`.cube` files) | CPU trilinear interpolation | FFmpeg `lut3d` |
| Title Overlay | Text, font, color, outline, shadow, background box | GStreamer `textoverlay` | FFmpeg `drawtext` |
| Speed | Constant speed, reverse, variable speed ramps | GStreamer rate-seek | FFmpeg `setpts` / `atempo` / `reverse` |
| Frei0r Plugins | 130+ third-party filter effects | GStreamer `frei0r-filter-*` | FFmpeg `frei0r` |

---

## Color Correction

Select a clip and expand the **Color & Denoise** section in the Inspector.

| Slider | Range | Default | Description |
|--------|-------|---------|-------------|
| **Exposure** | -1.0 to 1.0 | 0.0 | Overall exposure compensation |
| **Brightness** | -1.0 to 1.0 | 0.0 | Additive luminance shift |
| **Contrast** | 0.0 to 2.0 | 1.0 | Contrast multiplier (1.0 = normal) |
| **Saturation** | 0.0 to 2.0 | 1.0 | 0 = greyscale, 1 = normal, 2 = vivid |
| **Temperature** | 2000 to 10000 K | 6500 | Color temperature (low = warm/amber, high = cool/blue) |
| **Tint** | -1.0 to 1.0 | 0.0 | Green-magenta axis (negative = green, positive = magenta) |
| **Black Point** | -1.0 to 1.0 | 0.0 | Lifts or crushes black levels |

### Per-Tone Grading

Fine-grained color control separated by luminance zone. Uses a non-linear response curve: fine control near the center, stronger creative effect at slider extremes.

| Slider | Range | Default | Description |
|--------|-------|---------|-------------|
| **Shadows** | -1.0 to 1.0 | 0.0 | Shadow zone brightness |
| **Midtones** | -1.0 to 1.0 | 0.0 | Midtone zone brightness |
| **Highlights** | -1.0 to 1.0 | 0.0 | Highlight zone brightness |
| **Shadows Warmth** | -1.0 to 1.0 | 0.0 | Warm/cool shift in shadows |
| **Shadows Tint** | -1.0 to 1.0 | 0.0 | Green/magenta shift in shadows |
| **Midtones Warmth** | -1.0 to 1.0 | 0.0 | Warm/cool shift in midtones |
| **Midtones Tint** | -1.0 to 1.0 | 0.0 | Green/magenta shift in midtones |
| **Highlights Warmth** | -1.0 to 1.0 | 0.0 | Warm/cool shift in highlights |
| **Highlights Tint** | -1.0 to 1.0 | 0.0 | Green/magenta shift in highlights |

### Color Grade Copy/Paste

| Shortcut | Action |
|----------|--------|
| `Ctrl+Alt+C` | Copy color grade from selected clip |
| `Ctrl+Alt+V` | Paste color grade to selected clip |
| `Ctrl+Alt+M` | Match color to a reference clip |

Copies all color sliders + LUT path as static values between clips. Use `Ctrl+Shift+V` (Paste Attributes) to copy everything including audio, transforms, and effects.

### Match Clip Colors

Automatically grade one clip to match the color appearance of a reference clip using Reinhard-style statistical color transfer in CIE L\*a\*b\* space. Available via Inspector **"Match Color..."** button or `Ctrl+Alt+M`.

Optionally generates a 17^3 3D `.cube` LUT for fine-grained non-linear residual matching beyond what sliders can express. Fully undoable in one step.

---

## Denoise / Sharpness / Blur

| Slider | Range | Default | Description |
|--------|-------|---------|-------------|
| **Denoise** | 0.0 to 1.0 | 0.0 | Gaussian blur strength for noise reduction |
| **Sharpness** | -1.0 to 1.0 | 0.0 | Negative = soften, positive = sharpen |
| **Blur** | 0.0 to 1.0 | 0.0 | Creative blur for censoring, depth-of-field, background defocus. Preview via gaussianblur, export via boxblur. Supports keyframe animation. |

---

## Color LUT (3D Lookup Table)

Assign a `.cube` LUT file to any clip for professional color grading.

| Control | Description |
|---------|-------------|
| **Import LUT...** | Opens file dialog filtered to `.cube` files |
| **Clear** | Removes LUT from clip |

- One LUT per clip; applied after built-in color corrections
- When proxy mode is enabled, LUT is baked into proxy media for efficient preview
- Timeline badge: cyan **LUT** indicator on clips with an assigned LUT

---

## Chroma Key (Green/Blue Screen)

Expand the **Chroma Key** section in the Inspector.

| Control | Type | Default | Description |
|---------|------|---------|-------------|
| **Enable** | Checkbox | Off | Activate chroma keying |
| **Key Color** | Radio: Green / Blue / Custom | Green | Target color to remove |
| **Tolerance** | Slider (0.0-1.0) | 0.3 | How far from target color to key out |
| **Edge Softness** | Slider (0.0-1.0) | 0.1 | Softens key edge for smoother blending |

Transparent regions allow lower video tracks to show through the compositor.

---

## AI Background Removal

Expand the **Background Removal** section in the Inspector. Requires an ONNX model file.

| Control | Type | Default | Description |
|---------|------|---------|-------------|
| **Enable** | Checkbox | Off | Activate neural-net background removal |
| **Threshold** | Slider (0.0-1.0) | 0.5 | Model confidence threshold |

---

## Transform

Expand the **Transform** section in the Inspector.

### Compositing

| Control | Type | Range / Options | Default | Description |
|---------|------|-----------------|---------|-------------|
| **Blend Mode** | Dropdown | Normal, Multiply, Screen, Overlay, Add, Difference, Soft Light | Normal | Compositing blend mode for overlay clips |
| **Scale** | Slider | 0.1x to 4.0x | 1.0 | Zoom factor (>1 = zoom in/crop, <1 = shrink with letterbox) |
| **Opacity** | Slider | 0.0 to 1.0 | 1.0 | Layer transparency (0 = invisible, 1 = opaque) |

### Position

| Control | Range | Default | Description |
|---------|-------|---------|-------------|
| **Position X** | -1.0 to 1.0 | 0.0 | Horizontal offset (-1 = full left, 0 = center, 1 = full right) |
| **Position Y** | -1.0 to 1.0 | 0.0 | Vertical offset (-1 = full top, 0 = center, 1 = full bottom) |

### Crop

| Control | Range | Default |
|---------|-------|---------|
| **Crop Left/Right/Top/Bottom** | 0 to 500 px | 0 |

### Rotation & Flip

| Control | Type | Description |
|---------|------|-------------|
| **Rotate** | Dial + numeric (-180 to 180) | Arbitrary-angle rotation |
| **Flip H** | Toggle | Mirror horizontally |
| **Flip V** | Toggle | Mirror vertically |

### Blend Modes Reference

| Mode | Effect |
|------|--------|
| **Normal** | Default compositing, no blending |
| **Multiply** | Darkens by multiplying pixel values |
| **Screen** | Lightens by inverting, multiplying, and inverting |
| **Overlay** | Combination of multiply and screen based on base layer |
| **Add** | Additive blending, sums pixel values (brightens) |
| **Difference** | Absolute difference between layers |
| **Soft Light** | Subtle dodge/burn effect |

### Transform Keyframes

Scale, Opacity, Position X/Y, Rotate, and Crop are all keyframable.

| Shortcut | Action |
|----------|--------|
| `Shift+K` | Toggle Record Keyframes mode |
| `Alt+Left` / `Alt+Right` | Jump to previous/next keyframe |
| Arrow keys | Nudge position by 0.01 |
| `Shift+Arrow` | Nudge position by 0.1 |
| `+` / `-` | Increase / decrease scale |

Interpolation modes: Linear, Ease In, Ease Out, Ease In/Out.

---

## Speed

Expand the **Speed** section in the Inspector.

| Control | Range | Default | Description |
|---------|-------|---------|-------------|
| **Speed** | 0.25x to 4.0x | 1.0 | Playback speed (0.5 = slow-motion, 2.0 = fast-forward) |
| **Reverse** | Checkbox | Off | Play clip backwards |

### Variable Speed Ramps

Speed supports keyframes for smooth ramp effects (e.g., normal speed ramping into slow-motion). Edit via the dopesheet **Speed** lane in the Keyframes panel.

Timeline badges: **Ramp** (speed keyframes), **< 2x** (constant speed), **< Ramp** (reversed + ramped).

---

## Frei0r Plugin Effects

UltimateSlice discovers and supports **130+ frei0r video filter plugins** from the system GStreamer registry.

### Browsing Effects

1. Click the **Effects** tab in the left panel
2. Browse by category or use the search bar (matches name, description, plugin ID)
3. Double-click or click **"Apply to Clip"** to add an effect to the selected clip

### Applied Effects (Inspector)

The **Applied Effects** section in the Inspector shows the per-clip effect chain:

| Control | Description |
|---------|-------------|
| Enable checkbox | Bypass effect without removing |
| Up/Down arrows | Reorder in chain |
| Delete button | Remove effect |
| Parameter sliders | Numeric parameters (0.0-1.0 normalized) |
| Dropdowns | Enum string parameters |
| Text entries | Free-form string parameters |

Effects process in order from top to bottom, after built-in color corrections and before chroma key.

### Curves Editor

The **curves** frei0r plugin renders as a graphical curve editor instead of raw sliders:

- **Channel selector**: Red, Green, Blue, RGB, or Luma
- **240×240 canvas**: Dark grid with diagonal identity baseline and smooth Catmull-Rom spline
- **2–5 control points**: Click to select, drag to move (auto-sorted by input value, clamped to 0–1)
- **Double-click**: On empty area to add a new point (up to 5), on existing point to remove it (minimum 2)
- Changes update the preview in real-time

### Levels Editor

The **levels** frei0r plugin renders as a graphical levels editor:

- **Channel selector**: Red, Green, Blue, or Luma
- **Transfer function visualization**: 240×80 canvas showing the input→output mapping curve with vertical markers for input black/white levels
- **Input Black / Input White**: Set the input range (0–1)
- **Gamma**: Midtone adjustment (0.1–4.0, mark at 1.0 = neutral; mapped from frei0r 0–1 range)
- **Output Black / Output White**: Set the output range (0–1)
- The transfer function curve updates in real-time as sliders move

### Copy/Paste Effects

Copy and paste buttons in the **Applied Effects** header let you transfer all frei0r effects between clips. Also included in Paste Attributes (`Ctrl+Shift+V`).

### Common Frei0r Effects

Here is a selection of commonly used frei0r effects. The full list depends on your system's installed `frei0r-plugins` package.

#### Color & Grading

| Plugin | Description |
|--------|-------------|
| **coloradj_RGB** | Per-channel RGB level adjustment |
| **3-point-color-balance** | Shadows/midtones/highlights color balance (renders as color wheels in Inspector) |
| **curves** | Tone curve adjustment (renders as graphical curve editor in Inspector) |
| **levels** | Input/output level control with gamma (renders as graphical levels editor in Inspector) |
| **brightness** | Simple brightness control |
| **gamma** | Gamma correction |
| **saturat0r** | Saturation adjustment |
| **equaliz0r** | Histogram equalization (auto-contrast) |
| **colgate** | Color grading with lift/gamma/gain |
| **tint0r** | Map grayscale to color gradient |

#### Blur & Sharpen

| Plugin | Description |
|--------|-------------|
| **IIRblur** | Fast IIR Gaussian blur |
| **squareblur** | Box blur |
| **medians** | Median filter (noise reduction preserving edges) |

#### Distortion & Stylize

| Plugin | Description |
|--------|-------------|
| **distort0r** | Lens-like distortion |
| **cartoon** | Cartoon/posterize effect |
| **edgeglow** | Glowing edge detection |
| **emboss** | Emboss/relief effect |
| **pixeliz0r** | Pixelation/mosaic |
| **vertigo** | Vertigo zoom-rotate effect |
| **water** | Water ripple distortion |
| **nervous** | Random frame shaking |

#### Generators & Overlay

| Plugin | Description |
|--------|-------------|
| **lissajous0r** | Lissajous pattern generator |
| **cairogradient** | Gradient overlay (linear, radial, patterns) |
| **light_graffiti** | Light painting / long exposure simulation |
| **vignette** | Darkened/lightened corners |

#### Compositing & Alpha

| Plugin | Description |
|--------|-------------|
| **alpha0ps** | Alpha channel operations (shrink, grow, display, threshold) |
| **alphagrad** | Alpha gradient (linear fade) |
| **alphaspot** | Circular alpha spot |
| **bluescreen0r** | Blue-screen keying |
| **bgsubtract0r** | Background subtraction from static reference |
| **select0r** | HSL-based color selection to alpha |
| **transparency** | Simple opacity adjustment |

#### Time & Motion

| Plugin | Description |
|--------|-------------|
| **delay0r** | Frame delay effect |
| **baltan** | Temporal averaging (ghosting) |
| **nervous** | Random frame displacement |

### Installing Frei0r Plugins

```bash
# Debian / Ubuntu
sudo apt install frei0r-plugins

# Fedora
sudo dnf install frei0r-plugins

# Arch Linux
sudo pacman -S frei0r-plugins
```

After installation, restart UltimateSlice. Plugins are discovered automatically from standard system directories.

---

## Processing Order

Effects are applied in this order within each clip's pipeline:

1. **Color Correction** (brightness, contrast, saturation, temperature, tint, exposure, per-tone grading)
2. **Color LUT** (3D lookup table)
3. **Denoise / Sharpness** (Gaussian blur)
4. **Frei0r Plugin Effects** (user-applied, in chain order)
5. **Chroma Key** (alpha keying)
6. **Transform** (crop, scale, position, rotation, flip)
7. **Title Overlay** (text rendering)
8. **Compositor** (layer compositing with blend mode and opacity)

---

## MCP Tools

| Tool | Description |
|------|-------------|
| `set_clip_color` | Set brightness/contrast/saturation |
| `set_clip_opacity` | Set opacity (0.0-1.0) |
| `set_clip_keyframe` | Set/update a keyframe at a timeline position |
| `remove_clip_keyframe` | Remove a keyframe |
| `set_clip_chroma_key` | Set chroma key parameters |
| `set_clip_blend_mode` | Set compositing blend mode |
| `set_clip_title_style` | Set title text/font/color/styling |
| `add_clip_frei0r_effect` | Add a frei0r effect to a clip |
| `remove_clip_frei0r_effect` | Remove a frei0r effect |
| `set_clip_frei0r_effect_params` | Set effect parameters |
| `reorder_clip_frei0r_effects` | Change effect chain order |
| `list_clip_frei0r_effects` | List effects on a clip |
| `list_frei0r_plugins` | List all available frei0r plugins |
| `match_clip_colors` | Auto-match color grading to reference clip |
| `copy_clip_color_grade` | Copy color grade to clipboard |
| `paste_clip_color_grade` | Paste color grade from clipboard |
