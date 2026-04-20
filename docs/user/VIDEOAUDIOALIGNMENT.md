# Video & Audio Alignment

UltimateSlice provides two methods for synchronizing clips from multi-camera shoots or separate audio recorders: **timecode-based alignment** and **audio cross-correlation sync**. Both are available from the timeline right-click context menu and via MCP tools. Source timecode can come from imported metadata **or** from a one-time LTC audio conversion pass.

---

## Quick Start

1. Place your clips on separate tracks in the timeline.
2. Select all the clips you want to align (Ctrl+click or Shift+click).
3. Right-click the selection and choose:
   - **Align Grouped Clips by Timecode** — if clips already have source timecode metadata.
   - **Sync Selected Clips by Audio** — if clips share audible ambient sound.
4. The first selected clip is the **anchor** (it stays in place); other clips are repositioned to match.

---

## Method 1: Timecode Alignment

### How it works

When media is imported, UltimateSlice automatically extracts the creation date/time from each file using GStreamer's Discoverer (`GST_TAG_DATE_TIME`). This timestamp is stored as `source_timecode_base_ns` — time-of-day in nanoseconds — on the library item and on every clip placed from that source.

If the source instead carries **Linear Timecode (LTC)** on an audio channel, you can right-click a single timeline clip and choose **Convert LTC Audio to Timecode…**. UltimateSlice decodes the LTC start value into the same `source_timecode_base_ns` field, so the converted media behaves exactly like imported timecode metadata everywhere else in the app.

When you align by timecode, UltimateSlice computes the time-of-day offset between each clip's source start and the anchor clip's source start, then repositions the clips so those offsets are reflected in their timeline positions.

### When to use it

- Cameras were **jam-synced** (timecodes synchronized before recording).
- Cameras were **not** jam-synced but their internal clocks are reasonably close (within a few seconds). Small clock drift between consumer cameras is normal and acceptable.
- You want a fast, zero-computation alignment that works instantly.

### Requirements

- Clips must be **grouped** (`Ctrl+G`) before aligning by timecode.
- At least two clips in the group must have source timecode metadata.
- The metadata is automatically extracted on import for most camera files (MP4, MOV, MKV with creation-time tags), or can be generated from LTC audio with **Convert LTC Audio to Timecode…**. It's also preserved through FCPXML import/export.

### Converting LTC audio into source timecode

Use this when one audio side contains SMPTE LTC instead of a normal embedded timecode track.

1. Place the clip on the timeline and select it.
2. Right-click → **Convert LTC Audio to Timecode…**
3. Pick the LTC source channel:
   - **Auto Detect** — tries left, right, then mono mix.
   - **Left Channel** / **Right Channel** — use when you know which stereo side carries LTC.
   - **Mono Mix** — use for mono LTC or already mixed-down recordings.
4. Leave the frame rate on **Project / Source Default** unless the LTC was recorded at a different frame rate than the project.
5. Click **Convert**.

Result:

- UltimateSlice writes the decoded base to the library item, the selected source monitor entry, and all same-source clips already in the project.
- If LTC lives on the **left** side, the **right** side is routed to both speakers.
- If LTC lives on the **right** side, the **left** side is routed to both speakers.
- If the clip is effectively **mono LTC only**, clip audio is muted after conversion.

After conversion, group the clips and use **Align Grouped Clips by Timecode** as usual.

### Accuracy

Timecode alignment is as accurate as the cameras' internal clocks. Professional cameras with jam-sync are frame-accurate. Consumer cameras (GoPro, phones) are typically within 1-3 seconds.

### Source file

`src/media/probe_cache.rs` — `extract_timecode_ns()` reads `GST_TAG_DATE_TIME` from the GStreamer Discoverer result and converts hour/minute/second/microsecond to time-of-day nanoseconds.

`src/media/ltc.rs` — decodes LTC from audio samples when you run **Convert LTC Audio to Timecode…**.

---

## Method 2: Audio Cross-Correlation Sync

### How it works

Audio sync finds the time offset between two recordings by matching their audio content. If two cameras in the same room both captured a clap, a voice, or even background hum, the algorithm finds exactly where that sound appears in each recording and shifts the clips to align.

The implementation uses a signal processing technique called **GCC-PHAT** (Generalized Cross-Correlation with Phase Transform), which is the same approach used by professional multi-cam sync tools.

### Pipeline overview

```
Media file
    │
    ▼
GStreamer decode (uridecodebin → audioconvert → audioresample)
    │
    ▼
Raw mono F32 samples at 22,050 Hz (capped at 15 seconds)
    │
    ▼
Bandpass filter: 300 Hz – 3,000 Hz (biquad IIR)
    │
    ▼
FFT → cross-power spectrum → PHAT normalization → IFFT
    │
    ▼
Peak detection → sample offset → nanosecond offset
    │
    ▼
Confidence check → apply or reject
```

### Step-by-step detail

#### 1. Audio extraction

Each clip's audio is decoded using a GStreamer pipeline identical to the waveform cache:

```
uridecodebin → audioconvert → audioresample → capsfilter [F32LE, mono, 22050 Hz] → appsink
```

The clip is decoded from `source_in` to `source_out`, but extraction is capped at **15 seconds** (`MAX_EXTRACT_SECONDS`). This cap serves two purposes:
- Keeps FFT sizes manageable (15s at 22,050 Hz = ~330K samples, FFT of ~1M points).
- Prevents a short 5-second clip from being drowned out in the correlation output when correlated against a 60-minute clip.

Memory budget: two 15-second clips + FFT buffers ≈ 40 MB.

#### 2. Bandpass prefilter (300 – 3,000 Hz)

Before correlation, each audio signal is filtered through a cascaded pair of second-order Butterworth biquad IIR filters:
- **High-pass at 300 Hz** — removes low-frequency rumble, HVAC hum, wind noise, and handling noise that varies between microphones.
- **Low-pass at 3,000 Hz** — removes high-frequency hiss, self-noise, and sibilance that differs dramatically between microphone capsules.

The 300–3,000 Hz range is where ambient sound, voices, claps, and room tone live. This frequency range is captured by virtually every microphone regardless of quality — from a phone's built-in mic to a GoPro to a professional shotgun. By discarding the frequencies where microphones disagree, the correlation focuses on what they have in common.

The filter coefficients are computed using the standard Audio EQ Cookbook biquad formulas with Q = 1/√2 (Butterworth).

#### 3. GCC-PHAT correlation

Standard cross-correlation computes `R = IFFT(FFT(a) × conj(FFT(b)))`. This works but produces a broad peak that can be ambiguous in reverberant environments or when recording levels differ.

GCC-PHAT normalizes the cross-power spectrum by its magnitude:

```
R = IFFT( FFT(a) × conj(FFT(b)) / |FFT(a) × conj(FFT(b))|^β )
```

The normalization discards amplitude information (which differs between mics) and keeps only phase information (which encodes the time delay). The result is a much sharper, more distinct peak.

**Smoothing parameter β = 0.73:** Pure PHAT (β = 1.0) normalizes completely, which can amplify noise in frequency bins with little signal energy. The smoothed variant with β = 0.73 retains enough magnitude weighting to keep the peak above noise while still providing the sharpening benefits. This is a well-established compromise used in practical implementations.

The FFT is computed at the next power-of-two length ≥ `len_a + len_b - 1` for efficient FFT computation (using the `rustfft` crate, pure Rust, MIT-licensed).

#### 4. Peak detection and offset

The IFFT output is a circular correlation array. The index of the maximum magnitude maps to the sample offset:
- Index `k` in `[0, fft_len/2]` → positive lag (clip B should be placed later).
- Index `k` in `(fft_len/2, fft_len)` → negative lag `k - fft_len` (clip B should be placed earlier).

The sample offset is converted to nanoseconds: `offset_ns = sample_offset × (10⁹ / 22050)` ≈ 45,351 ns per sample.

#### 5. Confidence scoring

Confidence = peak magnitude / mean magnitude of the correlation output (excluding the peak itself). This ratio measures how "distinct" the best match is compared to the noise floor:

| Confidence | Meaning |
|---|---|
| > 10 | Excellent match (loud shared event like a clap) |
| 5 – 10 | Good match (ambient sound, speech) |
| 3 – 5 | Marginal match (quiet room, some shared content) |
| < 3 | **Rejected** — no reliable audio match found |

When confidence is below 3.0, UltimateSlice rejects the result and shows a status message instead of applying incorrect offsets.

### Multi-clip sync (3+ clips)

When more than two clips are selected, the first clip is the **anchor**. Each subsequent clip is independently correlated against the anchor. This is O(K) correlations for K clips, not O(K²).

### When to use it

- Cameras were **not** timecode-synced (no jam-sync, different brands, phone + camera, etc.).
- Audio recorders were used separately from cameras.
- Timecode alignment is unavailable or insufficient (clock drift > a few seconds).
- Any scenario where all recording devices captured audible ambient sound in the same environment.

### When it won't work

| Scenario | Why | Alternative |
|---|---|---|
| Clip has no audio stream | Nothing to correlate | Use timecode alignment |
| Completely different audio content | No shared signal to match | Manual alignment |
| Very short clip (< 0.5s) | Too few samples for reliable correlation | Extend clip or use manual alignment |
| Cameras in different rooms | No shared ambient sound | Use timecode alignment |

---

## UI Access

### Timeline context menu

1. Select 2+ clips on the timeline.
2. Right-click → choose **Sync Selected Clips by Audio** or **Align Grouped Clips by Timecode**.
3. The sync button is only sensitive when 2+ clips are selected.
4. The timecode button is only sensitive when selected clips are grouped and have timecode metadata.
5. For LTC-backed sources, right-click a **single** clip first and run **Convert LTC Audio to Timecode…**, then group and align.

### Status feedback

- During audio sync, the bottom status bar shows ongoing progress while sync runs on a background thread.
- During LTC conversion, a short in-app toast confirms that decoding has started while the background work runs.
- On completion: "Audio sync complete" or "Audio sync failed — no reliable audio match found".
- On LTC completion: "Converted LTC to HH:MM:SS:FF..." with the applied audio-routing outcome.
- Toast messages auto-dismiss after a short delay.

### Undo

Both operations are fully undoable with `Ctrl+Z`. Audio sync uses `SetTrackClipsCommand` internally, so the undo history shows "Sync clips by audio" as the operation name.

---

## MCP Tools

### `sync_clips_by_audio`

```json
{
  "name": "sync_clips_by_audio",
  "arguments": {
    "clip_ids": ["clip-uuid-1", "clip-uuid-2", "clip-uuid-3"]
  }
}
```

### `convert_ltc_audio_to_timecode`

```json
{
  "name": "convert_ltc_audio_to_timecode",
  "arguments": {
    "clip_id": "clip-uuid-1",
    "ltc_channel": "auto",
    "frame_rate": "29.97"
  }
}
```

Returns:

```json
{
  "success": true,
  "clip_id": "clip-uuid-1",
  "source_timecode_base_ns": 3600000000000,
  "timecode": "01:00:00:00",
  "resolved_ltc_channel": "left",
  "applied_audio_channel_mode": "right",
  "muted": false,
  "updated_clip_count": 3
}
```

First clip ID is the anchor. Returns:

```json
{
  "success": true,
  "results": [
    {
      "clip_id": "clip-uuid-2",
      "offset_ns": 1500000000,
      "confidence": 12.5,
      "new_timeline_start_ns": 1500000000
    },
    {
      "clip_id": "clip-uuid-3",
      "offset_ns": -500000000,
      "confidence": 8.3,
      "new_timeline_start_ns": 0
    }
  ]
}
```

If any clip's confidence is below 3.0, `success` is `false` and no timeline changes are applied.

The MCP handler runs synchronously (blocks the MCP thread), which is acceptable since audio sync typically completes in 1-5 seconds.

### `align_grouped_clips_by_timecode`

```json
{
  "name": "align_grouped_clips_by_timecode",
  "arguments": {
    "clip_ids": ["clip-uuid-1", "clip-uuid-2"]
  }
}
```

Clips must be grouped. Returns aligned group/clip counts or an error if timecode metadata is missing.

---

## Source Code Reference

| File | Role |
|---|---|
| `src/media/audio_sync.rs` | Core sync engine: audio extraction, bandpass filter, GCC-PHAT |
| `src/media/probe_cache.rs` | Timecode extraction from GStreamer Discoverer tags |
| `src/ui/timeline/widget.rs` | Context menu buttons, `can_sync` / `sync` methods |
| `src/ui/window.rs` | Background thread dispatch, result polling, offset application |
| `src/mcp/mod.rs` | `SyncClipsByAudio` command variant |
| `src/mcp/server.rs` | `sync_clips_by_audio` tool definition and dispatch |

---

## Technical Constants

| Constant | Value | Rationale |
|---|---|---|
| Sample rate | 22,050 Hz | Adequate for 300–3000 Hz band; 1 sample ≈ 45 μs resolution |
| Max extraction | 15 seconds | Balances sync reliability vs. FFT cost |
| Bandpass low | 300 Hz | Excludes rumble/hum that varies between mics |
| Bandpass high | 3,000 Hz | Excludes hiss/sibilance that varies between mics |
| PHAT β | 0.73 | Smoothed phase normalization; sharper than standard xcorr, more robust than pure PHAT |
| Confidence threshold | 3.0 | Below this, results are rejected as unreliable |
