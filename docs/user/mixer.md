# UltimateSlice — Audio Track Mixer

The **Mixer** panel provides a traditional mixing-console view of your
timeline's audio, with per-track gain faders, stereo pan controls, VU meters,
and mute/solo buttons.

---

## Opening the Mixer

Click the **Show Mixer** toggle button on the track-management bar (bottom
left of the timeline area). The mixer appears as a bottom panel alongside
Keyframes, Transcript, and Markers — switch between them by clicking the
corresponding toggle button.

The mixer visibility is saved in workspace layouts, so you can include it in
your preferred arrangement.

---

## Channel Strip Anatomy

Each audio-producing track gets a vertical channel strip:

```
┌──────────┐
│  Label    │   Track name (ellipsized)
│  [D/E/M]  │   Audio role badge (Dialogue / Effects / Music)
│  ┌──┬──┐  │
│  │L │R │  │   Stereo VU meter (green → yellow → red)
│  │  │  │  │   with peak-hold indicators
│  └──┴──┘  │
│    ┃      │   Vertical gain fader (−∞ to +12 dB)
│  0.0 dB   │   Current gain readout
│  ◄━━━►    │   Horizontal pan slider (−1.0 … +1.0)
│  [M] [S]  │   Mute / Solo toggle buttons
└──────────┘
```

A **Master** strip on the far right shows the overall stereo VU meter and the
project's master gain control.

---

## Gain Fader

- **Range:** −∞ (silence) to +12 dB.
- **Default:** 0 dB (unity gain).
- **Behavior:** Track gain is a post-clip multiplier — it scales the combined
  output of all clips on the track, composing with per-clip volume keyframes
  and automatic ducking.
- **Double-click** the fader to reset to 0 dB.
- Fader changes are live during drag (you hear the change immediately) and are
  recorded as a single undo step when you release the mouse.

---

## Pan Control

- **Range:** −1.0 (full left) to +1.0 (full right).
- **Default:** 0.0 (center).
- **Behavior:** Track pan is added to each clip's pan value and clamped to
  [−1, 1]. This lets you offset an entire track's stereo image without
  touching individual clip pans.
- **Double-click** the slider to reset to center.

---

## VU Meters

The per-track stereo VU meters show real-time peak levels during playback:

| Zone | Range | Color |
|------|-------|-------|
| Normal | below −12 dB | Green |
| Caution | −12 dB to −3 dB | Yellow |
| Hot | above −3 dB | Red |

A **peak-hold** indicator (thin line) marks the recent peak for each channel.

The meters read from the same GStreamer level data as the timeline header
meters, just displayed larger for easier monitoring.

---

## Mute & Solo

- **Mute (M):** Silences the track. Syncs bidirectionally with the M button in
  the timeline track header.
- **Solo (S):** Solos the track (mutes all non-soloed tracks). Syncs
  bidirectionally with the S button in the timeline track header.

Both use undo commands, so you can Ctrl+Z to revert.

---

## MCP Automation

The mixer is fully controllable via MCP (Model Context Protocol):

| Tool | Parameters | Description |
|------|-----------|-------------|
| `set_track_gain` | `track_id`, `gain_db` | Set a track's gain in dB |
| `set_track_pan` | `track_id`, `pan` | Set a track's stereo pan (−1 to 1) |
| `get_mixer_state` | *(none)* | Returns all tracks' gain, pan, muted, soloed, role, and master gain |

See [python-mcp.md](python-mcp.md) for connection examples.

---

## Export

Track gain and pan are applied during export:

- **Gain** is applied as a separate volume filter after per-clip processing.
- **Pan** composes with per-clip pan values.

Both are also persisted in `.uspxml` (as `us:track-gain-db` / `us:track-pan`
attributes) and OTIO metadata, so they survive save/load round-trips.
