# WebGPU — cross-vendor GPU acceleration

**Recommended default for most users.** The `ai-webgpu` Cargo feature
pulls ONNX Runtime's WebGPU execution provider as a prebuilt binary,
which runs on Intel Arc, AMD, and NVIDIA through a single Vulkan-
based path (Dawn → Vulkan / D3D12 / Metal). No vendor SDK install is
required — just up-to-date GPU drivers.

This is the lowest-friction way to get GPU acceleration for the four
AI caches that use ONNX Runtime: SAM segmentation, MODNet background
removal, RIFE frame interpolation, and MusicGen.

## When to pick WebGPU vs. a native EP

| You have… | Pick WebGPU if… | Pick native EP if… |
|---|---|---|
| NVIDIA GPU | …you want zero setup | …you need max throughput (→ [CUDA](nvidia-cuda.md)) |
| Intel Arc / iGPU | …you want zero setup | …you need INT8 quant / OpenVINO-specific graph optimizations (→ [OpenVINO](intel-arc-openvino.md)) |
| AMD GPU | …you want zero setup or your GPU isn't on AMD's ROCm compatibility list | …your GPU is supported by ROCm and you want MIOpen-tuned kernels (→ [ROCm](amd-rocm.md)) |
| None / shared VM | Skip GPU features entirely; the default build runs everything on CPU | — |

WebGPU gives good-but-not-optimal throughput — typically 50-80% of
what the native EP delivers on the same GPU, with none of the install
pain.

## Prerequisites

- A GPU with a working Vulkan driver:
  - **Intel Arc / iGPU (Linux)**: `intel-opencl-icd` + the Intel
    Vulkan driver (`mesa-vulkan-drivers` on Debian/Ubuntu,
    `vulkan-intel` on Arch, `mesa-vulkan-drivers` on Fedora). Kernel
    ≥ 6.2 for Arc A-series recommended.
  - **AMD (Linux)**: Mesa RADV is the default; it ships with
    `mesa-vulkan-drivers` / `vulkan-radeon`. You do **not** need the
    proprietary AMDGPU-PRO driver. Kernel ≥ 6.1 recommended.
  - **NVIDIA (Linux)**: proprietary driver ≥ 535 with the
    `libnvidia-vulkan` package, or nouveau + Mesa NVK (experimental).
- Linux. Windows support is possible via D3D12 but untested against
  this project.
- `vulkaninfo` should report your GPU. If `vulkaninfo | grep -i
  "deviceName"` lists your card, WebGPU should work.

## Building

```bash
cargo build --features ai-webgpu
```

First run downloads the `wgpu`-flavored prebuilt ONNX Runtime binary
(~300 MB) from `cdn.pyke.io/0/pyke:ort-rs/ms@1.24.2/` — the exact
archive is `x86_64-unknown-linux-gnu+wgpu.tar.lzma2`. This archive
includes Dawn and all its runtime dependencies, so no further system
libraries are needed at build time.

If you're behind a firewall that blocks `cdn.pyke.io`, see the
[troubleshooting page](troubleshooting.md#prebuilt-download-fails)
for how to pre-fetch the tarball and point `ORT_LIB_LOCATION` at it.

## Selecting WebGPU at runtime

Launch the app and open **Preferences → Models**. In the **AI
Acceleration** section, pick `WebGPU (cross-vendor)` from the Backend
dropdown. The change takes effect on the next inference job — no
restart required.

Alternatively, set it in the MCP preferences blob or directly via
the `ai_backend` field in `PreferencesState` on disk.

## Verification

Run the SAM smoke test with verbose ONNX Runtime logging:

```bash
RUST_LOG=ort=debug cargo test --features ai-webgpu \
    media::sam_cache::tests::segment_with_box_smoke -- \
    --ignored --nocapture 2>&1 | grep -i "provider\|execution"
```

You should see a line like `Execution provider: WebGpuExecutionProvider`
or `WebGPU: registered successfully`. Inference time should drop to
a few seconds (vs ~13 s on CPU).

## Known issues

- **Test-harness segfault on process exit with `ai-webgpu`**: the
  `configure_session_builder_auto_succeeds` unit test is marked
  `#[ignore]` when `ai-webgpu` is enabled. Dawn's C++ destructor
  races with the ort environment teardown when a GPU device is
  initialized but never actually run, causing the test binary to
  segfault **after** tests have already passed. This is a test-
  harness-only issue — long-lived GTK sessions do not hit it. See
  `src/media/ai_providers.rs` for the `#[cfg_attr]` marker and
  explanation.
- **First run is slow**: Dawn does initial shader compilation on the
  first inference and caches the results under
  `$XDG_CACHE_HOME/dawn/`. Subsequent runs hit the cache and are
  much faster.

## Linking with native EPs

You can build with **multiple** GPU features enabled at once:

```bash
cargo build --features ai-webgpu,ai-cuda
```

In that case the `Auto` backend in Preferences registers both EPs
and ORT picks at runtime. The in-tree ordering is `CUDA → ROCm →
OpenVINO → WebGPU → CPU` — native EPs win when available, WebGPU is
the cross-vendor fallback above CPU.

Mixing `ai-webgpu` with `ai-rocm` or `ai-openvino` is allowed in
principle but requires a matching source-built ONNX Runtime; the
prebuilt `wgpu` variant does not include the ROCm or OpenVINO EPs.
If you need both WebGPU and a source-built EP, build ORT from
source with `--use_webgpu` alongside the vendor-specific flag.
