# Inspector

The **Inspector** panel (right side) shows and edits the properties of the currently selected timeline clip.

Select a clip in the timeline to populate the Inspector. All changes apply immediately to the program monitor preview. Clips inside compound clips are fully supported -- double-click a compound clip to enter it, then select an internal clip to inspect and edit its properties.

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

## Audition (alternate takes)

Visible only when an **audition clip** is selected. Audition clips wrap multiple alternate takes around a single timeline slot — see [auditions.md](auditions.md) for the full guide.

| Control | Description |
|---|---|
| **Takes list** | Every alternate take with its label, source filename, and duration. The currently active take is highlighted with an **Active** badge. Click any other row to switch — the Program Monitor and timeline update immediately, and the swap is undoable. |
| **Add Take from Source** | Append a new take to the audition. Seeded from the currently active take's source path and in/out as a starting point; tweak afterwards. |
| **Remove Take** | Remove the selected non-active take. Disabled when the active take is selected — switch active first if you want to delete what's currently playing. |
| **Finalize Audition** | Collapse the audition to a normal clip referencing only the active take. Discards alternates. Undoable. |

Color grade, transforms, transitions, masks, keyframes, and frei0r effects all live on the audition slot, not on individual takes. Switching the active take preserves your look — that's the point.

---

## Transition

The Inspector can edit the selected clip's **outgoing** transition when another clip follows it on the same track.

- In the Inspector layout, the **Transition** section appears below **Transform** and starts collapsed by default.

| Control | Description |
|---|---|
| **Type** | `None`, `Cross-dissolve`, `Fade to black`, `Fade to white`, `Wipe left/right/up/down`, `Circle open/close`, `Cover left/right/up/down`, `Reveal left/right/up/down`, or `Slide left/right/up/down` |
| **Duration (ms)** | Transition length in milliseconds. Durations are clamped automatically to the boundary capacity of the two adjacent clips |
| **Alignment** | Controls where the overlap sits relative to the edit (`End on cut`, `Center on cut`, `Start on cut`) |
| **Remove Transition** | Clears the outgoing transition from the selected clip |

- Dragging from the **Transitions** browser still creates a new transition with the default **500 ms** duration; use the Inspector to refine it after drop.
- If the selected clip has no following clip on the same track, transition editing is disabled and the Inspector explains why.
- Duration and alignment changes affect preview, export, and background prerender together.
- Program Monitor keeps exact live previews for all supported transition types.
- `Start on cut` and the after-cut half of `Center on cut` keep the outgoing clip visible by holding its last frame after the cut. UltimateSlice does this because trimmed clips do not currently expose hidden source handles past their out-point.

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

## HSL Qualifier (Secondary Color Correction)

Isolates pixels by **Hue**, **Saturation**, and **Luminance** range, then
applies a follow-up brightness / contrast / saturation grade **only inside
the matched region**. This is the DaVinci-style workflow for grading a
specific color — punch the sky cyan without tinting skin, desaturate green
foliage, boost magenta rim light, etc.

Open the **HSL Qualifier** expander below Color & Denoise.

| Control | Range | Default | Notes |
|---|---|---|---|
| **Enable** | toggle | off | Turns the qualifier on. |
| **Invert** | toggle | off | Flip the matte — grade the pixels *outside* the range. |
| **View Mask** | toggle | off | Debug overlay — Program Monitor shows the computed matte as grayscale so you can dial ranges. Not persisted; never applied at export. |
| **Hue Min / Hue Max** | 0–360° | 0 / 360 | Range of matched hues. When `Min > Max` the range **wraps around 360°**, which is how you select reds that straddle 0° (e.g. `Min=340`, `Max=20`). |
| **Hue Softness** | 0–60° | 0 | Smoothstep feather band on both edges of the hue range. |
| **Sat Min / Sat Max** | 0.0–1.0 | 0.0 / 1.0 | Saturation range. Narrow to isolate pastels vs. vivid tones. |
| **Sat Softness** | 0.0–0.5 | 0.0 | Feather band on saturation edges. |
| **Lum Min / Lum Max** | 0.0–1.0 | 0.0 / 1.0 | Luminance range. Narrow to isolate shadows / midtones / highlights. |
| **Lum Softness** | 0.0–0.5 | 0.0 | Feather band on luminance edges. |
| **Secondary Brightness** | −1.0 → 1.0 | 0.0 | Brightness delta added to matched pixels. |
| **Secondary Contrast** | 0.0 → 2.0 | 1.0 | Contrast multiplier (around 0.5 mid) on matched pixels. |
| **Secondary Saturation** | 0.0 → 2.0 | 1.0 | Saturation multiplier on matched pixels — 0 = desaturate the range, 2 = pump it. |

### Workflow

1. Enable the qualifier.
2. Toggle **View Mask** so the Program Monitor shows the matte in grayscale.
3. Drag the **Hue Min / Max** sliders until only the region you want is white.
4. Pinch **Sat Min / Max** and **Lum Min / Max** to drop any false hits.
5. Nudge the **Softness** sliders to feather the edges.
6. Toggle View Mask off and adjust **Secondary Brightness / Contrast /
   Saturation** — you will see the secondary grade applied only to the
   matched pixels.

### Under the hood

Program Monitor and FFmpeg export share the exact same HSL math. Preview
runs a CPU pad probe on a dedicated `us-hsl-identity` element placed after
the primary color chain (so the qualifier sees your primary grade, not the
raw source). Export emits a single inline `geq` filter wrapped in
`format=gbrp` / `format=yuva420p` bridges that encodes the RGB→HSL
conversion, range membership, secondary grade, and alpha blend in one pass.
Neutral or disabled qualifiers are skipped entirely so existing clips stay
byte-identical.

Automation: the **`set_clip_hsl_qualifier`** MCP tool accepts every field
as an optional argument and supports `clear: true` to remove the qualifier.
Persistence: `.uspxml` projects round-trip the qualifier via a
`us:hsl-qualifier` attribute; OTIO exports round-trip via
`metadata.ultimateslice.hsl_qualifier`.

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
| **Voice Isolation** | 0% → 100% | Off | Ducks volume between spoken words. See **Voice Isolation Source** below. |
| **Pan** | −1.0 → 1.0 | 0.0 | Stereo position (−1 = full left, +1 = full right) |

#### Voice Isolation Source

Voice isolation needs to know *when* the speech is happening so it can duck audio in the gaps. UltimateSlice supports two ways to provide that timing:

- **Subtitles** (default) — uses Whisper-generated word timings. Requires running **Generate Subtitles** on the clip first. Best precision when the audio transcribes well.
- **Silence Detect** — uses ffmpeg's `silencedetect` filter to find speech regions automatically, **without subtitles**. Use this for clips that don't transcribe well (music beds, ambient/B-roll, foreign-language clips) or when running Whisper is overkill.

When **Silence Detect** is selected, three additional controls appear:

| Control | Range | Default | Effect |
|---|---|---|---|
| **Silence threshold** | −60 dB → −10 dB | −30 dB | Audio below this level is treated as silence. Lower = stricter (only true near-silence counts as a gap). |
| **Min gap** | 50 ms → 2000 ms | 200 ms | Minimum silence duration to count as a gap. Higher = ignore brief pauses between words. |
| **Suggest** button | — | — | Analyzes the clip's noise floor with `astats` and auto-picks a threshold (5th-percentile RMS + 6 dB headroom). |
| **Analyze Audio** button | — | — | Runs `silencedetect` with the current threshold + min-gap settings, stores the resulting speech intervals on the clip. **Required** before silence-mode voice isolation takes effect. |

The detected speech intervals are cached on the clip but **not** persisted in `.fcpxml` — the source path, threshold, min-gap, and source mode round-trip, but the intervals themselves are re-analyzed on demand after reload (click **Analyze Audio** again). Threshold/min-gap edits and trim edits both invalidate the cache.

MCP tools:
- `set_voice_isolation_source` — switch between `"subtitles"` and `"silence"`
- `set_voice_isolation_silence_params` — set `threshold_db` and/or `min_ms`
- `suggest_voice_isolation_threshold` — returns a suggested threshold without mutating the clip
- `analyze_voice_isolation_silence` — runs the analysis and stores intervals

### Normalize Audio

The **Normalize...** button (next to the volume slider) analyzes the clip's loudness using FFmpeg's EBU R128 measurement and adjusts the volume to hit **-14 LUFS** (YouTube/streaming standard). After analysis, the measured loudness is displayed (e.g., "−18.3 LUFS") and the volume slider updates to the normalized value. Fully undo-able.

MCP tool: `normalize_clip_audio` supports `mode` (`lufs` or `peak`) and `target_level` (e.g., `-14.0` for LUFS, `0.0` for peak dBFS).

### Match Audio

The **Match Audio...** button analyzes the selected clip against another audio-capable clip and applies a conservative reference match using integrated loudness plus the built-in **Low / Mid / High** EQ bands. It now derives the three bands' **frequency**, **gain**, and **Q** from a finer speech-focused spectrum analysis instead of only pushing fixed band gains. If the clips already have subtitle/STT timing, Match Audio prioritizes those dialogue regions; otherwise it falls back to voice-active frame weighting to reduce the influence of silence, room tone, and non-speech noise. This works best when both clips contain the same speaker or similar dialogue material, such as nudging a lav mic recording closer to a shotgun mic recording.

- Pick the source clip in the timeline, click **Match Audio...**, then choose the reference clip.
- **Match voice** is the default and recommended mode: it analyzes the full trimmed source/reference clips while still prioritizing dialogue or voice regions automatically.
- **Channel handling** defaults to **Auto (Recommended)**, which respects each clip's current channel routing and automatically switches to a single side when the other stereo channel is effectively silent. You can also force **Mono Mix**, **Left Only**, or **Right Only** analysis.
- Switch **Match mode** to **Choose region...** to reveal **Source In/Out** and **Reference In/Out** timecode fields when you want to match only a selected phrase. Those fields default to each clip's full trimmed duration and use the project frame rate for timecode entry.
- UltimateSlice measures both clips in the background and applies one undoable update to the source clip's volume, measured loudness, **3-band EQ**, and a separate **7-band match EQ** (`match_eq_bands`) for finer mic matching.
- The 7-band match EQ centers are at ~100 Hz, 200 Hz, 400 Hz, 800 Hz, 2 kHz, 5 kHz, and 9 kHz — covering body/clothing resonance, chest/proximity effect, low-mid muddiness, fundamental speech, presence, and air. Both EQs are applied in series during export AND in live preview (the program player wires a dedicated 7-band equalizer element into each slot's audio chain when match EQ is present). The 3-band EQ remains available for manual tweaks on top.
- When match EQ is active, the inspector shows a small **frequency-response curve** below the **Match Audio…** button (log-frequency X axis, ±12 dB Y axis, with band markers). A **Clear Match EQ** button next to it resets just the 7-band match correction without touching your manual 3-band EQ.
- While the analysis is running, the bottom status bar shows **Matching audio...** and the shared progress bar pulses until the result is applied or fails.
- The result is a tonal nudge, not full microphone cloning: room reflections, de-reverb, compression, and noise reduction are out of scope.

MCP tools:
- `match_clip_audio` with `source_clip_id`, `reference_clip_id`, optional `source_start_ns`, `source_end_ns`, `reference_start_ns`, `reference_end_ns`, plus optional `source_channel_mode` / `reference_channel_mode`. The response includes both `eq_bands` (3-band) and `match_eq_bands` (7-band).
- `clear_match_eq` with `clip_id` resets just the 7-band match EQ on a clip, leaving the user 3-band EQ untouched.

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

### Track audio: Audio Role + Surround Position + Ducking

The collapsible **Track Audio** sub-section inside Audio shows controls that
apply to the *track* the selected clip lives on (not just the clip):

| Control | Effect |
|---|---|
| **Audio Role** | Tags the track as `Dialogue` / `Effects` / `Music` / `None`. Drives the per-role submix bus during stereo export and the auto-routing destination during surround export. |
| **Surround Position** | Per-track override for **Advanced Audio Mode** surround exports (5.1 / 7.1). `Auto (by role)` (default) uses the role-based mapping; explicit values pin the track to a specific channel. Has no effect on stereo exports. |
| **Duck this track…** | When enabled, automatically lowers this track's volume whenever audio is present on any non-ducked track at the same timeline position (typical use: duck music under dialogue). |
| **Duck Amount (dB)** | Negative dB applied while ducking is active. Default −6 dB. |

**Surround Position** options:

- **Auto (by role)** — recommended; uses Dialogue → Front Center, Music → Front L/R, Effects → Front L/R + Surround L/R
- **Front L/R**, **Front Center**, **Front L/R + Surround L/R**, **Surround L/R**
- **LFE (bass only)** — pin this track to the subwoofer channel
- Single-channel pins: **Front Left**, **Front Right**, **Back Left**, **Back Right**, **Side Left**, **Side Right** (Side Left/Right alias to Back Left/Right in 5.1 since 5.1 has no side speakers)

The override is stored on the track (not the clip) and round-trips through
project save/load (`.uspxml`) as well as OTIO export. Switching the Inspector
selection between clips on the same track shows the same value.

For the full surround export pipeline (auto-routing table, automatic LFE bass
tap, codec compatibility), see [`docs/user/export.md`](export.md#audio-channels--advanced-audio-mode-surround).

---

## Video Transform

Applied via GStreamer `videocrop`, `videoflip`, `videoscale`, and `videobox` (preview) and ffmpeg filters (export).

| Control | Options | Description |
|---|---|---|
| **Blend Mode** | Dropdown | Compositing blend mode: Normal (default), Multiply, Screen, Overlay, Add, Difference, Soft Light. Preview blends against real lower layers via compositor probe; export uses ffmpeg `blend` filter |
| **Anamorphic Desqueeze** | Dropdown | Lens desqueeze factor: None (1.0x), 1.33x, 1.5x, 1.8x, 2.0x. Applies non-square pixel aspect ratio for anamorphic footage |
| **Scale** | 0.1 → 4.0 | Zoom factor. 1.0 = normal, 2.0 = 2× zoom in (crops), 0.5 = half size (letterbox/pillarbox) |
| **Opacity** | 0.0 → 1.0 | Layer blend amount. 1.0 = fully opaque, 0.0 = fully transparent |
| **Position X** | −3.0 → 3.0 | Horizontal offset. 0.0 = center, ±1.0 = clip edge touches the canvas edge, values past ±1.0 push the clip off-canvas (the preview compositor and export ffmpeg graph crop/pad past the frame edges) |
| **Position Y** | −3.0 → 3.0 | Vertical offset. 0.0 = center, ±1.0 = clip edge touches the canvas edge, values past ±1.0 push the clip off-canvas |
| **Crop Left/Right/Top/Bottom** | 0 → 4000 px | Crop pixels from each edge (in project pixels). The transform overlay also clamps each edge against the actual project resolution at runtime so opposing crops can never exceed the frame width/height |
| **Rotate** | Dial/knob + numeric angle (−180° → 180°) | Arbitrary-angle rotation |
| **Flip H** | Toggle | Mirror horizontally |
| **Flip V** | Toggle | Mirror vertically |

> **Adjustment layers:** the same transform controls define the adjustment clip's scoped effect region in preview and export. **Scale, Position, Crop, Rotate, and Opacity** stay active; **Blend Mode**, **Anamorphic Desqueeze**, and **Flip H/V** are shown but disabled because adjustment clips do not create their own image layer. Adjustment clips now also expose the normal **Shape Mask** section, and that mask is intersected with the transform/crop scope instead of replacing it. On adjustment clips, **Position X/Y** move the scoped region itself, so tracked/full-frame masked adjustments still translate visibly at `Scale = 1.0`.

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

## Motion Tracking

The **Motion Tracking** section is available on visual clips and covers both tracker authoring and tracker attachments.

| Control | Description |
|---|---|
| **Tracker** | Select which tracker on the current clip to edit. Use **+** to add a tracker to this clip and **Delete** to remove the selected tracker |
| **Label** | Rename the selected tracker |
| **Edit Region in Monitor** | Shows the tracker region in the Program Monitor so you can drag it into place visually |
| **Region Center X / Y** | Normalized tracker region center |
| **Region Width / Height** | Normalized half-size of the tracked region |
| **Region Rotation** | Rotation of the analysis rectangle |
| **Track Region / Re-run Tracking** | Run motion analysis for the selected tracker on the current clip, or regenerate samples after changing the tracker region |
| **Cancel** | Stop an in-progress tracking analysis job |
| **Attach To** | Choose whether a tracker drives the **Clip Transform** or the clip's **First Mask** |
| **Follow Tracker** | Attach this clip or mask to a tracker created on another clip in the project |
| **Clear Attachment** | Remove the current tracker attachment from the clip or mask |

- Trackers are stored on the source clip that was analyzed.
- Attachments are stored on the follower clip transform or its first mask.
- If you move or resize the tracker region after analysis, UltimateSlice keeps the attachment but clears the old samples; run **Re-run Tracking** on the source clip again before expecting preview/export motion.
- The **Follow Tracker** picker labels trackers that have no samples yet, and attached clips/masks warn when their source tracker is empty or disabled.
- The built-in tracker currently analyzes **translation** motion, so tracked overlays and masks follow position but do not yet infer scale or rotation automatically.
- Tracker attachments are resolved in both Program Monitor preview and export, and they persist through UltimateSlice project save/load (`.uspxml` vendor-attribute workflow).
- Title clips and other tracker-followed clips now translate directly across the canvas in preview and export, so **Follow Tracker** still moves them at `Scale = 1.0` instead of appearing to stop at the frame edge. Normal still-image clips stay on the existing still-image preview path unless they are actually following a tracker.
- Mask attachments currently target the **first rectangle or ellipse mask** on the clip. **Path masks** still need to be animated manually with their own controls/keyframes.

---

## Subtitles / Captions

Clips with subtitle segments show subtitle style controls in the Inspector.

| Field | Description |
|---|---|
| **Font** | Subtitle font description (Pango-style, e.g. `Sans Bold 24`) |
| **Bold** | Always-on bold for all subtitle text (independent of font description) |
| **Italic** | Always-on italic for all subtitle text |
| **Underline** | Always-on underline for all subtitle text |
| **Shadow** | Draw a drop shadow behind all subtitle text |
| **Shadow Color** | Shadow color (default: semi-transparent black) |
| **Shadow Offset X/Y** | Shadow displacement in points (default: 1.5) |
| **Text Color** | Main subtitle text color |
| **Word Highlight Flags** | Multi-select checkboxes: combine Bold, Color, Underline, Stroke, Italic, Background, and Shadow effects on the active word (replaces old single-mode dropdown) |
| **Highlight Color** | Active-word color for Color / Stroke highlight effects |
| **BG Highlight Color** | Background highlight color behind the active word (default: semi-transparent yellow) |
| **Word Window** | Number of nearby words grouped on screen around the active word |
| **Vertical Position** | Subtitle placement from top (`0.0`) to bottom (`1.0`) |
| **Outline Color** | Subtitle outline/stroke color |
| **Background Box** | Toggle the subtitle background box |
| **Background Color** | Background box color |
| **Copy Style / Paste Style** | Reuse subtitle styling across clips |

- Program Monitor preview renders subtitles in the monitor overlay.
- Export burns subtitle styling above the composited video.
- Subtitle font selection now uses the same normalized family/style/size parsing in preview and export, so descriptions like `Bold`, `Italic`, `Oblique`, and narrowed families stay closer between the Program Monitor and exported burn-in.
- Preview and export still use different subtitle renderers, so very fine styling details can still vary slightly.
- **See also: [Transcript panel](transcript.md)** — once you've generated subtitles, the bottom-of-window Transcript panel lets you click words to seek and select word ranges to ripple-delete the underlying clip in one undo entry.

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
| **Slow-Motion Interpolation** | Off / Frame Blending / Optical Flow / AI Interpolation (RIFE) | Off | Synthesizes intermediate frames for smooth slow-motion (speed < 1.0 only) |

Marks at **½×**, **1×**, **2×** for quick snapping.

### Slow-motion interpolation

When a clip's speed is below 1.0, the default behavior repeats frames to fill the longer duration, which can look stuttery. The **Slow-Motion Interpolation** dropdown offers three alternatives:

- **Frame Blending** — fast temporal averaging (`minterpolate mi_mode=blend`). Produces a slight motion-blur effect between frames. Export-only.
- **Optical Flow** — classical motion-compensated interpolation (`minterpolate mi_mode=mci`). Slower to encode but produces a smoother result than Frame Blending. Export-only.
- **AI Interpolation (RIFE)** — learned frame interpolation via a RIFE ONNX model. Best quality on rapid motion / occlusion / non-rigid subjects, where classical motion compensation tends to warp or ghost. Unlike the other two modes, the AI sidecar is **shared between Program Monitor preview and export**, so what you scrub through in the monitor is exactly what you get in the rendered MP4.

#### How AI Interpolation works

When you switch a slowed clip to **AI Interpolation**, a background worker decodes the source, runs RIFE pairwise to produce intermediate frames (multiplier `M = ceil(1 / min_speed)`, clamped to 2× / 4× / 8×), and writes a higher-fps H.264 sidecar to `~/.cache/ultimateslice/frame_interp/`. A status row beneath the dropdown shows **Generating…**, **Ready**, **Error**, or **Model not installed**. While generation is in progress the clip plays through the original source (so you keep working); once the sidecar is ready, the next preview rebuild and any export pick it up automatically.

The AI mode requires a **RIFE ONNX model** in `~/.local/share/ultimateslice/models/rife.onnx` — see [Preferences → Models](preferences.md#models) for the install location and a download link. If the model is missing, the dropdown still accepts the AI option but the status row will say **Model not installed** and preview/export fall back to the original source. AI mode is also a no-op for clips with no slow-motion segment — it only does work when the speed curve dips below 1.0.

The other two modes (Frame Blending / Optical Flow) remain export-only — they use ffmpeg `minterpolate`, which is too CPU-intensive for real-time preview, and are also picked up by background prerender when enabled. All four modes persist via the FCPXML `us:slow-motion-interp` vendor attribute.

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

> **Motion tracking** — The first rectangle/ellipse mask can be attached to a tracker from the **Motion Tracking** section so it follows tracked translation in preview and export. Path masks are still manual/keyframed only.

> **Adjustment layers** — On adjustment clips, the mask limits where the grading/effect pass is applied. The final affected area is the intersection of the adjustment layer's transform/crop scope and the authored mask alpha, so existing scoped-adjustment projects keep the same overall region semantics.

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
