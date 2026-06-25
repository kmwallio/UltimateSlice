# HEVC (H.265) hardware acceleration — VA-API / QSV / NVENC

This page covers hardware HEVC **encode** for export and the current state of
HEVC **decode**, with the vendor differences that matter for **10-bit 4:2:2**.

## Encode (export)

The export pipeline picks an HEVC encoder per the user's hardware-encoder mode
(`Off` / `Auto` / `Vaapi` / `Nvenc`) and the requested pixel profile:

| Profile | libx265 (software) | `hevc_qsv` (Intel QSV) | `hevc_vaapi` | `hevc_nvenc` |
|---|---|---|---|---|
| 8-bit 4:2:0 (`main`) | ✅ `yuv420p` | ✅ `nv12` | ✅ | ✅ |
| 10-bit 4:2:0 (`main10`) | ✅ `yuv420p10le` | ✅ `p010le` | ✅ (driver) | ✅ `p010le` |
| **10-bit 4:2:2 (`main422-10`)** | ✅ `yuv422p10le` | ✅ `y210le` | ❌ | ❌ |

Key points:

- **libx265** can encode every profile and is the universal fallback. The
  built-in **"HEVC 10-bit 4:2:2 (Mastering)"** preset uses it by default.
- **4:2:2 is hardware-accelerated only on Intel via QSV** (`hevc_qsv`,
  `y210le`). It's gated by `hwaccel::hevc_qsv_422_10_usable()`, which requires
  (a) ffmpeg to list `hevc_qsv`, (b) the QSV device to actually initialise
  (`-init_hw_device qsv` probe), and (c) `y210le` in `ffmpeg -h encoder=hevc_qsv`.
  When any of those fail — common on very new Arc drivers where the QSV device
  can't init, or where the build lacks oneVPL — export silently falls back to
  libx265.
- **`hevc_vaapi` and `hevc_nvenc` are 4:2:0 only.** A 4:2:2 request bypasses
  them (and the VAAPI `hwupload` plumbing) and uses QSV or libx265.
- **AMD**: the VCN video engine generally has **no 4:2:2** support, so 4:2:2
  uses libx265 on AMD. 4:2:0 (8/10-bit) works via `hevc_vaapi`.

## Decode

Hardware HEVC **decode** (`vah265dec` via VA-API, or QSV) is available on
Intel/AMD and decodes 10-bit 4:2:2 fine.

### Source Monitor (media browser preview)

When **Preferences → General → Hardware acceleration** is on and `vapostproc`
is available, the Source Monitor uses a GPU-resident decode path: it restores
the VA-API decoder ranks so `vah265dec` is auto-plugged, and scales the decoded
VA frame **on the GPU** (`vapostproc`, VA→VA) down to the preview size before
the only download. The expensive full-resolution scale never touches the CPU —
measured ~3.3× faster decode+scale on a 6K HEVC source (Intel Arc), which is
what lets a big source play back smoothly instead of stuttering. `vapostproc`
emits exact even dimensions, avoiding the "buffer not valid for dmabuf" errors
that made a raw-DMABuf attempt unreliable.

Any decode/display error in the hardware path falls back automatically to the
software pipeline for that clip (debounced to avoid thrash), and the whole path
is gated on the preference, VA-API availability, and `vapostproc` — so machines
without them keep the software path. The preview compositor is still 8-bit RGBA,
so this is a **performance** win, not a precision one.

### Program Monitor / export

The Program Monitor and **export** still decode HEVC in **software** today;
extending the hardware path to them is a follow-up. True 10-bit/HDR preview
requires the separate libplacebo pipeline (`docs/HDR-Buildout.md`).

## Checking your hardware

```sh
vainfo | grep -i hevc                 # VA-API profiles (look for HEVCMain422_10)
ffmpeg -hide_banner -h encoder=hevc_qsv | grep "Supported pixel formats"
ffmpeg -hide_banner -encoders | grep -iE "hevc|265"
```
