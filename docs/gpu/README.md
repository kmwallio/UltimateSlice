# GPU acceleration

UltimateSlice's four ONNX-backed AI caches (SAM segmentation, MODNet
background removal, RIFE frame interpolation, MusicGen) run on CPU
in the default build, and switch to GPU execution when built with
any of the `ai-webgpu` / `ai-cuda` / `ai-rocm` / `ai-openvino`
Cargo features. The `ai-cuda` and `ai-rocm` features additionally
bridge Whisper subtitle generation through `whisper-rs/cuda` and
`whisper-rs/hipblas`.

All scaffolding — `AiBackend` enum, process-wide atomic, detection,
per-cache session-builder routing, live Preferences UI, persistence
— exists in `src/media/ai_providers.rs` and is already wired into
every cache. The four feature flags control which execution
providers are **compiled in**; the Preferences UI controls which
one is **selected** at runtime. Selection changes take effect on
the next inference job without restart.

## Which backend should I pick?

Use this decision tree:

```
                       ┌──────────────────────┐
                       │  Need GPU quickly?   │
                       │  (zero vendor SDK)   │
                       └─────────┬────────────┘
                                 │
                    ┌────────────┴────────────┐
                   yes                        no
                    │                         │
                    ▼                         ▼
              ┌──────────┐            ┌──────────────┐
              │ ai-webgpu│            │ Which vendor?│
              └──────────┘            └──────┬───────┘
                                             │
                    ┌────────────────────────┼────────────────────────┐
                    │                        │                        │
                  NVIDIA                   Intel                     AMD
                    │                        │                        │
                    ▼                        ▼                        ▼
              ┌──────────┐             ┌──────────────┐        ┌──────────┐
              │  ai-cuda │             │ ai-openvino  │        │ ai-rocm  │
              │ prebuilts│             │ source-build │        │ source-  │
              │  1 step  │             │   ~1 hour    │        │ build    │
              └──────────┘             └──────────────┘        └──────────┘
```

Ranked by setup cost and peak performance:

| Backend | Setup cost | Peak perf | Vendor | STT GPU? | Doc |
|---|---|---|---|---|---|
| `ai-webgpu` | **low** — just prebuilts | good | Intel Arc / AMD / NVIDIA | no | [webgpu.md](webgpu.md) |
| `ai-cuda` | **low** — prebuilts + CUDA toolkit | **best (NVIDIA)** | NVIDIA | yes | [nvidia-cuda.md](nvidia-cuda.md) |
| `ai-openvino` | **high** — source-build ORT + OpenVINO SDK | **best (Intel)** | Intel Arc / iGPU / CPU | no | [intel-arc-openvino.md](intel-arc-openvino.md) |
| `ai-rocm` | **high** — source-build ORT + ROCm | **best (AMD)** | AMD (RDNA2+) | yes | [amd-rocm.md](amd-rocm.md) |

**Recommendation**: start with `ai-webgpu`. It's one `cargo build`
command away, works on any GPU with a Vulkan driver, and is the
same binary on all three vendors. Upgrade to a native EP only if
you've measured the WebGPU performance and need more throughput.

## Building

```bash
# Cross-vendor (WebGPU via Vulkan, prebuilts, recommended first step)
cargo build --features ai-webgpu

# NVIDIA (prebuilts, needs CUDA toolkit)
cargo build --features ai-cuda

# Intel (source-build required, ~1 hour)
./scripts/build_onnxruntime.sh --vendor openvino
export ORT_LIB_LOCATION=$HOME/.cache/ultimateslice/onnxruntime-openvino/build/Linux/Release
export ORT_LIB_PROFILE=Release
source /opt/intel/openvino_2024/setupvars.sh
cargo build --features ai-openvino

# AMD (source-build required, ~1 hour)
./scripts/build_onnxruntime.sh --vendor rocm
export ORT_LIB_LOCATION=$HOME/.cache/ultimateslice/onnxruntime-rocm/build/Linux/Release
export ORT_LIB_PROFILE=Release
cargo build --features ai-rocm
```

You can combine multiple features — e.g. `--features ai-webgpu,ai-cuda`
— which compiles both EPs in and lets `Auto` mode at runtime
register both. The in-tree Auto ordering is `CUDA → ROCm →
OpenVINO → WebGPU → CPU`: native EPs win when available; WebGPU is
the cross-vendor fallback above CPU.

## Selecting a backend at runtime

Open **Preferences → Models → AI Acceleration**. Pick from the
dropdown:

- `Auto (best available)` — register all compiled-in EPs and let
  ORT pick per-op.
- `NVIDIA CUDA`, `AMD ROCm`, `Intel OpenVINO`, `WebGPU (cross-vendor)`
  — explicit pick. Unavailable backends show `(unavailable)` and
  can't be selected.
- `CPU` — no GPU, guaranteed fallback.

Changes take effect on the next inference job — no restart needed.
The selection persists in `~/.config/ultimateslice/preferences.json`
and reloads at startup.

## Verifying end-to-end

The `segment_with_box_smoke` integration test is the canonical
smoke check. Run it with debug logging so you can see which EP
actually registered:

```bash
RUST_LOG=ort=debug cargo test --features ai-<your-vendor> \
    media::sam_cache::tests::segment_with_box_smoke -- \
    --ignored --nocapture 2>&1 | grep -iE "provider|execution"
```

Look for a line naming the EP you picked (e.g.
`Execution provider: CUDAExecutionProvider` / `OpenVINOExecutionProvider`
/ `ROCMExecutionProvider` / `WebGpuExecutionProvider`). The test
synthesizes a 512×512 image with a white square, runs SAM on it,
and asserts the resulting mask covers >50% of the square. Expected
timings:

| Backend | SAM inference time (512×512) |
|---|---|
| CPU | ~13 s |
| WebGPU | ~2-5 s |
| CUDA (prebuilts) | < 1 s on a discrete NVIDIA GPU |
| OpenVINO | 1-3 s on Arc, 2-4 s on iGPU |
| ROCm | 1-2 s on RX 7900 XTX |

## Known gaps

- **STT (Whisper)** has no OpenVINO or WebGPU backend — subtitle
  generation stays on CPU unless you build with `ai-cuda` or
  `ai-rocm`. Intel Arc users who want GPU STT today have no option;
  Phase 2 work would investigate whisper.cpp Vulkan/SYCL backends.
- **Windows / macOS support** for the native EPs is out of scope
  for this phase. WebGPU on macOS works in principle (Dawn → Metal)
  but is untested against this project.
- **Automatic backend benchmarking** is not implemented — `Auto`
  uses a static priority order (CUDA → ROCm → OpenVINO → WebGPU →
  CPU) rather than measuring per-hardware.

## Architecture reference

The routing layer lives in
[`src/media/ai_providers.rs`](../../src/media/ai_providers.rs). Every
AI cache that loads an ONNX Runtime `Session` must route its
`SessionBuilder` through `configure_session_builder(builder,
current_backend())`, which registers the selected EP in the right
order. The existing caches that follow this pattern:

- `src/media/sam_cache.rs::SamSessions::load`
- `src/media/bg_removal_cache.rs` (MODNet)
- `src/media/frame_interp_cache.rs` (RIFE)
- `src/media/music_gen.rs` (MusicGen)

When adding a new ONNX cache, follow the same pattern — `use
crate::media::ai_providers; let backend =
ai_providers::current_backend(); Session::builder().and_then(|b|
ai_providers::configure_session_builder(b, backend))…`

## See also

- [webgpu.md](webgpu.md) — WebGPU (cross-vendor, prebuilts)
- [nvidia-cuda.md](nvidia-cuda.md) — NVIDIA CUDA (prebuilts)
- [intel-arc-openvino.md](intel-arc-openvino.md) — Intel OpenVINO (source-build)
- [amd-rocm.md](amd-rocm.md) — AMD ROCm (source-build)
- [troubleshooting.md](troubleshooting.md) — common failure modes
- [`scripts/build_onnxruntime.sh`](../../scripts/build_onnxruntime.sh) — ORT source-build helper
