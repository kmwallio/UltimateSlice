# GPU backend troubleshooting

Common failure modes and fixes across all four GPU backends
(WebGPU, CUDA, ROCm, OpenVINO). Read the vendor-specific page first
for backend-level issues; this page covers cross-cutting problems.

## Diagnostic commands

Before filing issues, gather the following:

```bash
# Which backend does the running build think it's using?
RUST_LOG=ort=debug ./target/debug/ultimate-slice 2>&1 | grep -iE "provider|execution"

# Which GPU libraries are actually on the library path?
ldconfig -p | grep -iE "onnxruntime|cudart|miopen|openvino|vulkan"

# What's in my preferences file?
jq '.ai_backend' ~/.config/ultimateslice/preferences.json

# Does vulkan work at all (relevant for WebGPU)?
vulkaninfo | grep -i "deviceName"
```

## "Backend shows (unavailable) in Preferences"

The build includes the requested EP but ort can't load its runtime
dependencies on this machine. The Preferences dropdown marks it with
`(unavailable)` and falls back to the next option.

Cause: one of the backend's shared libraries is missing, has the
wrong version, or isn't on `LD_LIBRARY_PATH`.

**Fix per backend**:

- **CUDA**: `ldconfig -p | grep cudart` — must find libcudart.so.
  Install the CUDA toolkit; confirm `nvidia-smi` works.
- **ROCm**: `ldconfig -p | grep MIOpen` — must find libMIOpen.so.
  Confirm `rocminfo` lists your GPU as an Agent.
- **OpenVINO**: `ldconfig -p | grep openvino` — must find
  libopenvino.so. `source /opt/intel/openvino_2024/setupvars.sh` or
  add the runtime path to `/etc/ld.so.conf.d/openvino.conf`.
- **WebGPU**: Dawn is bundled in the ort prebuilt, so the only
  runtime dep is Vulkan: `vulkaninfo` must list a device. Missing
  mesa-vulkan or intel-opencl-icd is the usual culprit.

If you confirmed the libraries are present and the backend is still
`unavailable`, the ort `is_available()` check is returning false —
often a driver/version mismatch. Check `dmesg` for GPU-related
errors (`dmesg | grep -iE "amdgpu|nvidia|i915"`).

## "cargo build --features ai-* fails at link time"

### ai-webgpu / ai-cuda

These use prebuilt ort binaries. The build script downloads the
matching tarball from `cdn.pyke.io` during `cargo build`. If it
fails:

- **"Failed to download"** — network issue, cdn.pyke.io blocked, or
  cargo offline mode. See
  [prebuilt-download-fails](#prebuilt-download-fails) below.
- **"libcudart.so.12: No such file or directory"** (ai-cuda only)
  — CUDA toolkit not installed. The prebuilt ort links against
  `libcudart.so` at build time even if you'll only run on a
  CUDA-enabled machine later.

### ai-rocm / ai-openvino

These require `ORT_LIB_LOCATION` pointing at a source-built ONNX
Runtime. If you skipped Step 1 of the vendor page, the build will
fail immediately with an ort-sys error about missing system library
path.

Fix: run `scripts/build_onnxruntime.sh --vendor rocm` (or openvino)
first, then export `ORT_LIB_LOCATION` and `ORT_LIB_PROFILE` before
`cargo build`.

### "ORT 1.X.Y is not supported" version mismatch

The `ort` 2.0.0-rc.12 crate targets ONNX Runtime 1.24.2 specifically.
If `scripts/build_onnxruntime.sh` built a different version (e.g.
the pinned tag was overridden via a git checkout), you'll hit this
error.

Fix:

```bash
rm -rf $HOME/.cache/ultimateslice/onnxruntime-<vendor>
./scripts/build_onnxruntime.sh --vendor <vendor>
```

## Inference silently runs on CPU despite selecting GPU backend

ort registers EPs in order and skips any that fail. If your
selection's EP fails to initialize, ort silently falls back to the
next in the list — no error, just slow inference.

**Enable debug logging** to see what actually loaded:

```bash
RUST_LOG=ort=debug,ultimate_slice=debug cargo run 2>&1 | \
    grep -iE "provider|execution|registered"
```

You should see lines like:

```
Registered execution provider: CUDAExecutionProvider
```

or equivalent. If you see `CPUExecutionProvider` despite picking
CUDA in Preferences, the CUDA EP's `register()` call failed —
usually the underlying CUDA / cuDNN version mismatch.

Run the same SAM smoke test with and without the feature to compare:

```bash
# CPU baseline
cargo test media::sam_cache::tests::segment_with_box_smoke -- --ignored --nocapture

# Your GPU backend
RUST_LOG=ort=debug cargo test --features ai-<vendor> \
    media::sam_cache::tests::segment_with_box_smoke -- --ignored --nocapture
```

If both report the same inference time (~13 s on CPU), the GPU
backend isn't actually being used.

## Prebuilt download fails

The `ort-sys` build script downloads prebuilts from
`cdn.pyke.io/0/pyke:ort-rs/ms@1.24.2/...`. If your network blocks
the CDN or you need to build offline:

### Option 1: fetch the tarball manually

```bash
# For ai-webgpu
curl -sLO https://cdn.pyke.io/0/pyke:ort-rs/ms@1.24.2/x86_64-unknown-linux-gnu+wgpu.tar.lzma2

# For ai-cuda (CUDA 12)
curl -sLO https://cdn.pyke.io/0/pyke:ort-rs/ms@1.24.2/x86_64-unknown-linux-gnu+cu12.tar.lzma2
```

Extract with `tar --lzma -xf <file>.tar.lzma2 -C /path/to/extract`
(you may need `xz-utils` for lzma support). Point `ORT_LIB_LOCATION`
at the extracted `build/Linux/Release` directory (or equivalent)
and set `ORT_SKIP_DOWNLOAD=1`:

```bash
export ORT_SKIP_DOWNLOAD=1
export ORT_LIB_LOCATION=/path/to/extracted/lib
cargo build --features ai-webgpu
```

### Option 2: build from source

Same recipe as `ai-rocm` / `ai-openvino` — run the source-build
script against `microsoft/onnxruntime` at `v1.24.2`, optionally with
`--use_webgpu` or `--use_cuda`, then export `ORT_LIB_LOCATION` and
build. This is harder than just fetching the prebuilt but works in
fully-offline environments.

## "First run takes forever"

### WebGPU

Dawn compiles shaders on the first inference and caches the results
under `$XDG_CACHE_HOME/dawn/`. Expect the first SAM inference to
take 20-60 seconds; subsequent runs hit the cache and drop to 2-5 s.

If it never finishes on the first run, one of Dawn's shader compiles
is stuck. Check `dmesg` for GPU hangs, and consider switching to
`--device CPU_FP32` on the OpenVINO path as a diagnostic.

### CUDA

The CUDA EP runs autotuning on the first session-per-model,
picking the fastest kernel for your specific GPU + input shape.
First run can take 30+ seconds. `cudnn_benchmark=true` is on by
default in ort.

Cache location: ort writes cuDNN's tuning cache to the temp dir;
persistence across runs is not guaranteed.

### ROCm / OpenVINO

Source-built paths hit the same pattern — MIOpen and OpenVINO both
JIT-compile on first use. 20-60 seconds is normal.

## Still stuck?

- Confirm the default build works: `cargo test` (no features) should
  pass all ~1053 tests. If it doesn't, the GPU feature is a
  distraction — fix the base build first.
- Rebuild with `cargo clean && cargo build --features ai-<vendor>`.
  Sometimes incremental compilation caches stale object files.
- File an issue with `RUST_LOG=ort=debug` output + `uname -a` +
  `nvidia-smi` / `rocminfo` / `vulkaninfo` output + the exact cargo
  and ort versions.
