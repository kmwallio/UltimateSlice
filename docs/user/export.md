# Export

Click **Export…** in the toolbar to open the advanced export dialog.

## Export Dialog Options

### Video Codec

| Option | ffmpeg encoder | Notes |
|---|---|---|
| **H.264** (default) | `libx264` | Best compatibility; works in all players/platforms |
| **H.265 / HEVC** | `libx265` | ~50% smaller files than H.264 at same quality; requires player support |
| **VP9** | `libvpx-vp9` | Open format; good for web (WebM container) |
| **ProRes** | `prores_ks` | High-quality mastering format; large files; use with MOV container |
| **AV1** | `libaom-av1` | Excellent compression; very slow to encode |

### Container

| Option | Extension | Best with |
|---|---|---|
| **MP4** (default) | `.mp4` | H.264, H.265, AV1 |
| **QuickTime** | `.mov` | ProRes, H.264 |
| **WebM** | `.webm` | VP9, AV1 |
| **Matroska** | `.mkv` | Any codec |

MP4 and MOV containers get `-movflags +faststart` for web streaming compatibility.

### Output Resolution

| Option | Pixels | Use case |
|---|---|---|
| **Same as project** | Project width × height | No downscale |
| **4K UHD** | 3840 × 2160 | Archive / large screen |
| **1080p** | 1920 × 1080 | Standard HD delivery |
| **720p** | 1280 × 720 | Web / streaming |
| **480p** | 854 × 480 | Small file / mobile |

Clips are letterboxed/pillarboxed to fit the chosen resolution while preserving aspect ratio.

### Quality (CRF)

The **CRF** (Constant Rate Factor) slider controls quality vs. file size:

- **Lower = better quality, larger file**
- **Higher = lower quality, smaller file**
- Typical values: 18 (visually lossless) → 28 (acceptable web quality)
- Default: **23** (good balance)
- Not used for ProRes (lossless-ish by nature).

### Audio Codec

| Option | Notes |
|---|---|
| **AAC** (default) | Lossy; excellent compatibility |
| **Opus** | Lossy; excellent quality at low bitrates; best in WebM/MKV |
| **FLAC** | Lossless; large files |
| **PCM** | Uncompressed; very large; use for mastering |

### Audio Bitrate

Applies to AAC and Opus. Ignored for FLAC and PCM.

- Default: **192 kbps** (high quality)
- Acceptable web quality: 128 kbps
- High fidelity: 256–320 kbps

## Export Progress

After choosing the output file, an export progress dialog shows:
- A progress bar (0%–100%) driven by ffmpeg's `out_time_us` reporting.
- A status label showing the output path.
- A **Close** button (available once export completes or errors).

## Speed-Changed Clips

Clips with a speed multiplier are exported correctly:
- Video: `setpts=PTS/speed` filter adjusts frame timestamps.
- Audio: chained `atempo` filters adjust playback rate while preserving pitch.
  - The `atempo` filter is limited to 0.5×–2.0× per instance; multiple are chained for 0.25× or 4×.

For reversed clips, export applies `reverse`/`areverse` before speed scaling so both video and audio are rendered backward.

## Notes

- Export requires **ffmpeg** to be installed and on `$PATH`.
- All video tracks are processed in timeline order, with letterbox/pillarbox padding applied to each clip.
- Secondary-track overlays keep transparent padding when zoomed out and honor per-clip opacity, so layered composites export closer to Program Monitor preview.
- Overlay clips positioned near frame edges (where the PIP extends beyond the output boundary) are correctly clipped to match the preview — the export pre-crops overflow before padding so the visible portion and position match exactly.
- Audio is mixed from all non-muted audio tracks plus embedded audio in video clips.
- Export runs in a background thread; the UI remains responsive.
