# HDR Workflow — Full Buildout Plan

This document captures the technical details for completing the remaining HDR roadmap items that require libplacebo Vulkan integration. It's written as a reference for future implementation sessions.

---

## What's Already Shipped (v1)

| Feature | Implementation | Files |
|---------|---------------|-------|
| HDR source detection | GStreamer Discoverer colorimetry caps (PQ/HLG) | `probe_cache.rs` |
| Timeline badge | Orange "HDR" badge via `hdr_media_paths` in `TimelineState` | `widget.rs` |
| Preview tone mapping | `glcolorconvert` → `videoconvert` fallback in `build_effects_bin` | `program_player.rs` |
| Preview passthrough | `hdr_preview_passthrough` preference skips tone-map element | `ui_state.rs`, `preferences.rs` |
| Export tone mapping | `ffprobe` detection + `zscale`+`tonemap=hable` FFmpeg filters | `export.rs` |
| Export passthrough | `hdr_passthrough` on `ExportOptions` — 10-bit + BT.2020/PQ metadata | `export.rs`, `ui_state.rs` |
| Persistence | `hdr_colorimetry` on `MediaItem` / `SavedLibraryItem` / `ProbeResult` | `media_library.rs`, `probe_cache.rs` |

---

## Remaining Items

### 1. libplacebo Vulkan Integration (Foundation)

**What it is:** GPU-accelerated video rendering via libplacebo's Vulkan backend, replacing or augmenting the current CPU-based GStreamer preview pipeline.

**Why it matters:** libplacebo provides high-quality tone mapping algorithms (hable, bt2446a, st2094-40, spline), GPU-accelerated color space conversion, and advanced scaling — all unavailable through GStreamer's built-in elements.

**Technical approach:**

#### Option A: GStreamer `glplacebo` element (if available)
- GStreamer's `gst-plugins-rs` project has a `glplacebo` video filter element
- Runtime detection: `gst::ElementFactory::find("glplacebo")`
- Insert in `build_effects_bin` where the current `glcolorconvert`/`videoconvert` HDR tonemap sits
- Pros: No new Rust FFI, leverages GStreamer plugin system
- Cons: Requires user to have `gst-plugins-rs` with libplacebo support installed; limited parameter control

#### Option B: Direct libplacebo-rs bindings (full control)
- Create or adopt `libplacebo-sys` crate with `build.rs` using pkg-config
- Minimal FFI surface needed:
  ```
  pl_log_create / pl_log_destroy
  pl_vulkan_create / pl_vulkan_destroy
  pl_gpu_from_vulkan
  pl_renderer_create / pl_renderer_destroy
  pl_render_image (the main entry point)
  pl_color_space (BT.2020-PQ, BT.709, etc.)
  pl_tone_map_params / pl_tone_map_function
  pl_filter_config (for upscaling/downscaling)
  ```
- Integration via GStreamer pad probe on the compositor's src pad:
  1. Intercept raw video buffer
  2. Upload to Vulkan texture via `pl_tex_upload`
  3. Run `pl_render_image` with tone-map + scale config
  4. Download result back to CPU buffer (or pass Vulkan texture to display)
  5. Return modified buffer to the pipeline
- Pros: Full control over algorithms, parameters, and quality
- Cons: Maintain FFI bindings, Vulkan device lifecycle management, potential threading issues with GStreamer

#### Option C: FFmpeg libplacebo filter (hybrid)
- FFmpeg has `libplacebo` as a lavfi filter (`-vf libplacebo=...`)
- Could be used in the prerender cache pattern (like voice_enhance_cache):
  - Background FFmpeg job renders HDR→SDR sidecar with libplacebo tone mapping
  - Preview plays back the sidecar
- Pros: No Rust FFI needed, uses proven prerender pattern, high quality
- Cons: Not real-time, requires FFmpeg built with `--enable-libplacebo`

**Recommended approach:** Start with Option A (runtime `glplacebo` element detection) as a zero-dependency upgrade path. If unavailable, fall back to current `videoconvert`. Pursue Option B only if full parameter control is needed for professional HDR grading workflows.

#### Build system changes for Option B
```toml
# Cargo.toml (new dependency)
[dependencies]
libplacebo-sys = { version = "0.1", optional = true }

[features]
libplacebo = ["dep:libplacebo-sys"]
```

```rust
// build.rs (new file or addition)
fn main() {
    #[cfg(feature = "libplacebo")]
    {
        pkg_config::Config::new()
            .atleast_version("6.338")
            .probe("libplacebo")
            .expect("libplacebo >= 6.338 required");
    }
}
```

---

### 2. Inverse Tone Mapping (SDR → HDR)

**What it is:** Expand SDR source material to HDR for output on HDR displays or HDR export.

**When it's needed:** User has mixed SDR + HDR sources on the timeline and wants a unified HDR output, or wants to master SDR content for HDR delivery.

**Technical details:**
- libplacebo supports inverse tone mapping via `pl_tone_map_inverse` with algorithms like `bt2446a` (ITU-R BT.2446 Method A)
- The inverse map expands the SDR luminance range (0–100 nits) to HDR (0–1000+ nits) using perceptual models
- Requires knowing the target display peak luminance (configurable, e.g. 1000 nits for PQ mastering)

**Implementation sketch:**
1. Add `hdr_output_mode` to `ExportOptions`: `Sdr` (default) / `Hdr10` / `Hlg`
2. When `Hdr10` or `Hlg`, skip SDR tone mapping and instead:
   - For SDR sources: apply inverse tone map (expand to HDR)
   - For HDR sources: pass through (or re-grade to target peak)
3. Set FFmpeg output flags: `-color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc`
4. For preview: libplacebo renderer configured with target HDR display colorimetry

**FFmpeg approach (without libplacebo):**
```
-vf "zscale=t=linear,tonemap=linear:param=1.5,zscale=t=smpte2084:p=bt2020:m=bt2020nc,format=yuv420p10le"
```
This is a rough linear expansion — libplacebo's `bt2446a` produces perceptually better results.

---

### 3. High-Quality Upscaling/Downscaling

**What it is:** Replace GStreamer's basic `videoscale` (bilinear/Lanczos) with libplacebo's GPU-accelerated polar/orthogonal scalers (ewa_lanczos, ewa_lanczossharp, spline36, etc.).

**When it matters:**
- Downscaling 4K/8K to 1080p preview (current `videoscale` produces aliasing)
- Upscaling 720p/1080p sources to 4K export
- Preview quality at reduced resolution (`Half`/`Quarter` preview modes)

**Implementation sketch:**
1. In `build_effects_bin`, replace `videoconvertscale` with a libplacebo scaler element when available
2. Configure scaler via `pl_filter_config`:
   - Downscale: `ewa_lanczos` (sharp, minimal ringing)
   - Upscale: `ewa_lanczossharp` or `spline36`
3. The scaler runs on GPU, so it's faster than CPU scaling for high-res content
4. Fallback: existing `videoconvertscale` (no regression for users without libplacebo)

**Export path:** FFmpeg's `libplacebo` filter supports scaling:
```
-vf "libplacebo=w=3840:h=2160:upscaler=ewa_lanczos:downscaler=ewa_lanczos"
```

---

## Pipeline Architecture with libplacebo

### Current preview pipeline (per slot)
```
uridecodebin → effects_bin [convert → capsfilter(RGBA)] → compositor → display
```

### Target pipeline with libplacebo
```
uridecodebin → effects_bin [
    glcolorconvert (HDR tone map, if glplacebo unavailable)
    OR
    libplacebo_filter (tone map + scale + color space, GPU)
    → capsfilter(RGBA/P010)
] → compositor → libplacebo_display (optional) → gtk4paintablesink
```

### Key decisions
- **Where to insert:** Per-slot (before compositor) for source-aware tone mapping, or post-compositor for unified output processing
- **Buffer format:** RGBA 8-bit (current) vs P010/RGB30 10-bit (HDR passthrough to display)
- **GPU memory management:** Vulkan textures stay on GPU through the chain, or CPU round-trip per frame
- **Threading:** libplacebo Vulkan device must be created on the GStreamer streaming thread, not the GTK main thread

---

## Testing Requirements

1. **HDR test media:** At least one PQ (HDR10) and one HLG clip for testing
   - Freely available: Tears of Steel HDR, Cosmos Laundromat HDR grading
   - Can generate with: `ffmpeg -f lavfi -i testsrc2=s=1920x1080:r=24 -t 10 -c:v libx265 -x265-params "hdr-opt=1:repeat-headers=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc" -pix_fmt yuv420p10le test_hdr.mp4`

2. **HDR display:** For passthrough testing, need a monitor that supports HDR10 via Wayland or X11 HDR protocol

3. **Tone mapping quality:** Visual comparison of:
   - Current `videoconvert` tone mapping (basic)
   - libplacebo hable/bt2446a tone mapping (target)
   - FFmpeg `zscale+tonemap` export path

4. **Performance:** GPU tone mapping should be faster than CPU; measure frame delivery latency with and without libplacebo

---

## Dependencies and Availability

| Component | Package | Minimum Version | Notes |
|-----------|---------|----------------|-------|
| libplacebo | `libplacebo-dev` | 6.338+ | Vulkan backend required |
| Vulkan SDK | `libvulkan-dev` | 1.3+ | Runtime loader |
| GStreamer glplacebo | `gst-plugins-rs` | varies | Optional element |
| FFmpeg libplacebo | `--enable-libplacebo` | FFmpeg 6.0+ | For export path |

**Flatpak:** Would need `libplacebo` added to the Flatpak manifest (`io.github.kmwallio.ultimateslice.yml`) as a build module, similar to how ONNX Runtime is handled.
