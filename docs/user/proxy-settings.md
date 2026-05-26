# Proxy Settings

Proxies are smaller, lighter video files that UltimateSlice generates from your source media so the timeline can scrub and play back smoothly without decoding the full-resolution original on every frame. Exports always use the original media — proxies are a preview optimization only.

This guide explains the three proxy preferences (resolution, codec, hardware encoder mode) and gives concrete recommendations for common PC setups. All settings live under **Preferences → Proxies / Performance**.

---

## When do I need proxies?

You need proxies when:

- Your source media is **4K, 6K, or 8K** and playback stutters
- Your source codec is **HEVC, AV1, or VP9** at high bitrates (these need ~10× more CPU per frame than H.264 at the same resolution)
- Your source media is **10-bit or 12-bit** (extra decode cost on top of the codec)
- You're working with **many concurrent video tracks** (3+ overlapping clips)
- You're on a **laptop on battery** and want to keep the fans quiet

You don't need proxies when:

- Your source is already 1080p H.264 8-bit (most action cameras, screen recordings, web video)
- You have a fast workstation and an SSD that can keep up
- You're doing very short edits (< 30 seconds) where transcode cost outweighs scrubbing benefit

---

## Quick decision tree

| Your source | Recommended proxy mode | Recommended codec |
|---|---|---|
| 1080p H.264 8-bit | Off | (n/a) |
| 4K H.264 8-bit | 1080p | H.264 |
| 4K HEVC 8-bit | 1080p | H.264 |
| 4K HEVC 10-bit | 1080p | HEVC |
| 6K / 8K HEVC 10-bit | 720p or 540p | HEVC |
| Mixed (4K + 1080p sources) | 1080p | H.264 |
| Cinema RAW / ProRes RAW | 1080p | HEVC |

For the **Hardware encoder** preference, leave it on **Auto** unless you've measured a regression. The app probes your hardware at startup and picks the right backend.

---

## Resolution

Proxy resolution is set as a *maximum height*. Source aspect ratio is preserved, and proxies never upscale a source that's already smaller.

| Setting | Output height | Disk per minute (~) | Speed multiplier vs Off |
|---|---|---|---|
| **Off** | full source | source bitrate | 1× (no proxy) |
| **1080p** | 1080 | 25 MB | 1× to 4× faster scrubbing |
| **720p** | 720 | 12 MB | 1.1× to 5× faster scrubbing |
| **640p** | 640 | 9 MB | 1.2× to 6× faster scrubbing |
| **540p** | 540 | 7 MB | 1.3× to 7× faster scrubbing |

**The honest tradeoff**: lower resolution = faster transcode + smaller proxy files + faster playback, but the Program Monitor preview is genuinely less crisp during editing. Final export quality is unaffected (the export always uses originals).

**Recommendation**: start at 1080p. Drop to 720p if 1080p proxies are still slow. Drop to 540p only if you're doing rough-cut work and you'll re-watch in the Program Monitor before exporting.

---

## Codec — H.264 vs HEVC

| | H.264 (`libx264` / `h264_vaapi` / `h264_nvenc`) | HEVC (`libx265` / `hevc_vaapi` / `hevc_nvenc`) |
|---|---|---|
| **Compatibility** | Universal (every player, every browser) | Most modern players + browsers |
| **File size at same quality** | Baseline | ~30–50% smaller |
| **Encode speed (software)** | Fast | ~2× slower |
| **Encode speed (Intel iGPU)** | Fast | Often faster than H.264 (less compression work per output pixel) |
| **Encode speed (NVENC)** | Very fast | Very fast (similar to H.264) |
| **Decode cost in Program Monitor** | Lower | Higher (modern CPUs handle it fine) |

**Recommendation**:

- **H.264** if you're not sure. It's the default and works everywhere.
- **HEVC** if your iGPU has a hardware HEVC encoder *and* you're transcoding very-high-resolution sources (6K+). On Intel Lunar Lake / Arrow Lake / Meteor Lake, HEVC encode often beats H.264 encode for these.
- **HEVC** if you're tight on disk space and your sources are large.

Switching codec invalidates the per-source HW failure caches but doesn't delete existing proxy files — those stay valid for replay until the source itself changes.

---

## Hardware encoder mode

Controls whether the proxy and background-prerender pipelines try a hardware H.264/HEVC encoder before falling back to libx264 / libx265.

| Mode | Behavior |
|---|---|
| **Auto** (default) | Picks the best available: NVENC > VA-API > software. Skips broken backends automatically. |
| **Off** | Always uses software (libx264 / libx265). Slowest but most predictable. |
| **VA-API** | Force Intel/AMD iGPU encoder. Falls back to software if VA-API isn't available. |
| **NVENC** | Force NVIDIA NVENC. Falls back to software if libcuda isn't installed. |

The app probes at startup and surfaces a one-line **WARN** in the log if a backend is advertised but unusable (missing `libcuda.so.1`, no DRM render-node access, libva driver missing). Common fixes:

- **NVENC unavailable** despite an NVIDIA GPU: install the proprietary NVIDIA driver (provides `libcuda.so.1`).
- **VA-API unavailable** on Linux: add your user to the `render` group: `sudo usermod -aG render $USER`, then log out and back in.
- **VA-API still failing after `render` group**: install the libva user-space driver: `sudo apt install intel-media-va-driver-non-free` (Intel) or `sudo apt install mesa-va-drivers` (AMD). Verify with `vainfo`.

The decode side has its own gating logic: VA-API decode is intentionally **never used** in the proxy/prerender filter chains regardless of encoder choice — empirical testing on Intel Lunar Lake (Xe2 iGPU) showed VA-API decode of HEVC 10-bit goes through a hybrid CPU+GPU path that's *slower* than libavcodec on a multi-core CPU once the LUT/color filter chain forces a roundtrip. CUDA decode (NVDEC) and QSV decode are still used when available — their drivers handle the roundtrip more efficiently.

---

## Per-PC-setup recommendations

### Intel Core Ultra (Meteor Lake / Lunar Lake / Arrow Lake) — modern integrated graphics

```
Proxy mode:        1080p   (or 720p for 6K+ sources)
Proxy codec:       H.264   (HEVC if you're transcoding 6K+ regularly)
HW encoder mode:   Auto
```

What happens: VA-API encoder runs on the iGPU; CPU does the HEVC/H.264 source decode (faster than the iGPU on this generation). On the Lunar Lake test machine a 6:13 5952×3968 10-bit HEVC source proxies in **6:39** (1.07× realtime) at 1080p H.264, vs **~7:15** software-only.

**Required system packages**: `intel-media-va-driver-non-free`, `libva-drm2`, `libva2`. Verify with `vainfo`. User must be in the `render` group.

### Intel 8th–13th gen — older integrated graphics

```
Proxy mode:        1080p
Proxy codec:       H.264
HW encoder mode:   Auto
```

What happens: VA-API encode runs on the iGPU. On older Intel iGPUs, VA-API decode of 8-bit H.264/HEVC is also genuinely faster than the CPU path — but our code currently skips VA-API decode universally (calibrated for Lunar Lake). If you have a 12th-gen or older Intel chip and observe slower-than-expected proxy times, [open an issue](https://github.com/kmwallio/UltimateSlice/issues) and we'll add a generation-detection heuristic.

### NVIDIA discrete GPU (RTX series) on a desktop

```
Proxy mode:        1080p
Proxy codec:       H.264   (or HEVC if you also have a strong CPU)
HW encoder mode:   Auto
```

What happens: NVENC + CUDA decode handle both ends on the GPU, with frames downloaded to CPU between them for the LUT step. Expect proxies in the **30–90 second range** for a 6-minute 4K HEVC source. **Required**: working NVIDIA proprietary driver (provides `libcuda.so.1`).

### NVIDIA laptop GPU (mobile RTX / GTX) with Intel iGPU

```
Proxy mode:        1080p
Proxy codec:       H.264
HW encoder mode:   Auto       (NVENC will win)
```

If the dGPU is unpowered (laptop on battery, MUX switched to iGPU) the system falls back to VA-API on the Intel side automatically. Auto mode handles this transparently.

### AMD discrete GPU or AMD APU

```
Proxy mode:        1080p
Proxy codec:       H.264
HW encoder mode:   Auto       (VA-API will be picked)
```

What happens: AMD VCE/VCN encodes via VA-API. Performance varies by generation — RDNA 3 (RX 7000 series) and newer is competitive with NVENC. Older GCN cards can be slower than libx264 in some configurations.

**Required system packages**: `mesa-va-drivers`. Verify with `vainfo`.

### macOS (any Apple Silicon or Intel Mac)

```
Proxy mode:        1080p
Proxy codec:       H.264   (Apple's H.264 + HEVC encoders are both excellent)
HW encoder mode:   Off     (VideoToolbox isn't wired in yet — see below)
```

VideoToolbox HW encoding for Apple Silicon is on the roadmap but not yet exposed through the same picker. For now macOS uses software encoders; an Apple Silicon CPU is fast enough that this isn't usually a blocker.

### Linux without any working HW encoder (cloud / VM / minimal install)

```
Proxy mode:        720p     (smaller proxies = faster software encode)
Proxy codec:       H.264
HW encoder mode:   Off      (skips startup probes, faster app launch)
```

Set Off explicitly to skip the runtime HW probes at app startup. With software-only encoding, lower the proxy resolution to 720p or 540p to keep transcode times manageable.

### Low-end / low-RAM laptop (Celeron, older AMD APU, Chromebook converted)

```
Proxy mode:        540p     (smallest proxies)
Proxy codec:       H.264
HW encoder mode:   Auto     (use any HW you have)
```

Also consider:
- **Background Render: Off** in Preferences (saves CPU during edit sessions)
- **Preview quality: Half** or **Quarter** in Preferences
- **Realtime preview: Off** in Preferences

---

## Troubleshooting

### "My proxies are still slow"

Run with `RUST_LOG=info` and look for the `ProxyCache` log lines. Each transcode logs the encoder + decoder it actually used:

```
ProxyCache: spawning ffmpeg for ... (decode=sw, encode=h264_vaapi)
ProxyCache: ffmpeg ok for ... (decode=sw, encode=h264_vaapi) in 399.3s
```

If you see `decode=sw` and you have an HW decoder available (cuda / qsv), the picker is correctly skipping VA-API decode (which is the right call on Intel iGPUs). If `decode=sw` and `encode=libx264` *together*, no HW backend was selected — see the **HW encoder mode** section above for the install hints.

### "VA-API failed to initialise / unknown libva error"

Your system has the kernel render node (`/dev/dri/renderD128`) but the libva user-space driver isn't installed for your GPU. Install it:

- Intel: `sudo apt install intel-media-va-driver-non-free`
- AMD: `sudo apt install mesa-va-drivers`

Verify with `vainfo` — it should print a list of supported codec profiles. If `vainfo` itself errors, the VA-API stack isn't usable yet.

### "CUDA / NVENC unavailable" but I have an NVIDIA GPU

You're missing `libcuda.so.1`. Install the proprietary NVIDIA driver:

- Ubuntu: `sudo ubuntu-drivers autoinstall` then reboot
- Fedora: enable RPM Fusion, then `sudo dnf install akmod-nvidia xorg-x11-drv-nvidia-cuda`

The open-source `nouveau` driver does **not** support NVENC.

### "My CPU pegs at 100% during proxy generation"

This is correct behavior when you're using a software encoder (no HW encoder available, or you've chosen Off). FFmpeg uses every CPU thread to keep transcode time down. The 4-worker parallelism in UltimateSlice is intentionally throttled to **2 concurrent jobs** for sources at or above 4K to avoid thrashing — sub-4K jobs run unrestricted on all 4 workers.

### "Proxy generation freezes on a specific source"

Check the log for a `ProxyCache: ffmpeg FAILED for <path> ... (decode=..., encode=...): <stderr tail>` line. The stderr tail tells you what FFmpeg refused to do — common cases:

- 10-bit source + H.264 NVENC: NVENC's H.264 encoder doesn't accept 10-bit input. FFmpeg should auto-convert via `format=nv12` insertion; if it doesn't, switch to **HEVC** codec (NVENC's HEVC encoder supports 10-bit natively).
- Variable-frame-rate source: try a different proxy mode or report the source — VFR handling has known edge cases.
- Corrupt source file: `ffprobe` will tell you; not something the editor can recover from.

### "I switched HW encoder mode but nothing changed"

Open and close Preferences once after restarting the app — the saved preference is applied at startup but in-flight transcodes finish under their original settings. The next proxy job will pick up the new mode. The per-process HW failure caches also reset whenever you change `HwEncoderMode` or `Proxy codec`, so previously-failed sources get a fresh attempt.

---

## See also

- [preferences.md](preferences.md) — full Preferences reference including non-proxy settings
- [program-monitor.md](program-monitor.md) — playback quality controls
- [project-health.md](project-health.md) — view + clean up proxy cache disk usage
- [export.md](export.md) — exports always use original media; settings here don't affect output quality
