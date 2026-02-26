# Colour Scopes

UltimateSlice includes a professional colour scope panel to help you evaluate your
grade objectively. The panel lives below the **Program Monitor** and is toggled with
the **"▾ Scopes"** button.

---

## Showing / Hiding the Scopes

Click the **▾ Scopes** button just below the Program Monitor to reveal or hide the
panel. The panel slides in with an animation and disappears completely when hidden
(freeing vertical space).

Frames are only captured and analysed while the panel is visible — no CPU is wasted
when it is hidden.

---

## Scope Modes

Switch between modes using the tab strip at the top of the scopes panel.

### Waveform

Plots the **luma (brightness)** of every pixel at its horizontal position in the
frame.

- **Bright pixels** appear near the top (high IRE).
- **Dark pixels** appear near the bottom (low IRE).
- Graticule lines are drawn at 0 %, 25 %, 50 %, 75 %, and 100 %.

Use the waveform to set exposure, check for crushed blacks or clipped whites, and
match shots.

### Histogram

Shows the **distribution of luma values** from 0 (black) to 255 (white) as a bar
chart. A healthy exposure typically produces a well-spread, roughly bell-shaped
histogram that does not clip hard at either end.

### RGB Parade

Three side-by-side waveform monitors — one each for the **Red**, **Green**, and
**Blue** channels. Use the parade to detect colour casts: if one channel's waveform
sits higher or lower than the others in the same region of the frame, that channel
is biased.

### Vectorscope

Plots **Cb (U) vs Cr (V)** chrominance for every pixel in a circular diagram.

- The **centre** represents neutral (no saturation).
- **Distance from centre** indicates saturation.
- **Angle** indicates hue.

Use the vectorscope to match skin tones across shots and verify colour fidelity.

---

## Underlying Architecture

Colour scope frames are captured by a small `appsink` (320 × 180 RGBA) connected
via a GStreamer `tee` inside the program player's video-sink bin. The tee branches
to both the display (`gtk4paintablesink`) and the scope appsink simultaneously.
The appsink uses `drop=true, max-buffers=1` so it never stalls playback.

Frame analysis and Cairo drawing happen on the GTK main thread inside the existing
33 ms poll timer.
