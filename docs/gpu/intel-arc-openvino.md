# Intel Arc / OpenVINO

The `ai-openvino` Cargo feature uses ONNX Runtime's OpenVINO
execution provider, which delivers the best performance on Intel
GPUs and integrated graphics by leveraging OpenVINO's graph-level
INT8 quantization, layer fusion, and Intel-specific kernel
optimizations. Unlike WebGPU and CUDA, **there is no prebuilt path**
for OpenVINO — you must build ONNX Runtime from source with the
OpenVINO EP enabled, then point `ORT_LIB_LOCATION` at your build
before `cargo build`-ing the project.

If you'd rather skip the source build, [WebGPU](webgpu.md) runs on
Intel Arc too via Vulkan and delivers roughly 70-80% of the
performance. This page is for users who want maximum throughput and
are willing to spend an hour building ONNX Runtime once.

STT (Whisper subtitle generation) **stays on CPU** with `ai-openvino`
— whisper-rs has no OpenVINO backend. If you need GPU-accelerated
STT, use `ai-cuda` or `ai-rocm` depending on your hardware.

## Prerequisites

### Hardware
- **Intel Arc discrete GPU** (A-series: A310, A380, A580, A750, A770,
  or B-series: B570, B580). Use `--device GPU_FP16` when running the
  build script.
- OR **Intel iGPU** on 11th-gen Core or newer (Xe Graphics, Iris Xe,
  Arc Graphics). Use the default `--device CPU_FP32` — OpenVINO's
  CPU device plugin also handles iGPU offload transparently.
- OR **Intel CPU only** — OpenVINO's CPU plugin is highly optimized
  (competitive with oneDNN), and is often faster than the built-in
  CPU EP for transformer models like SAM.

### System software

- **Intel OpenVINO Toolkit 2024.4** or newer. The build script
  defaults to `/opt/intel/openvino_2024`, which is where Intel's
  official archive installer unpacks.
- **Intel Graphics Compute Runtime** (`intel-compute-runtime`,
  `intel-opencl-icd`, `intel-level-zero-gpu`) for GPU offload on Arc.
  On Ubuntu 24.04: `sudo apt install intel-opencl-icd
  intel-level-zero-gpu level-zero`.
- **Build toolchain**: GCC/G++ ≥ 11, CMake ≥ 3.26, Python 3.10+,
  Python packages `onnx numpy` (for ORT's build scripts), ~25 GB
  free disk, ~16 GB RAM for the linker step.

### Installing OpenVINO

Intel distributes OpenVINO as an archive installer. As of
2024.4.0 (the version ORT 1.24.2 targets):

```bash
cd /tmp
wget https://storage.openvinotoolkit.org/repositories/openvino/packages/2024.4/linux/l_openvino_toolkit_ubuntu24_2024.4.0.16579.c3152d32c9c_x86_64.tgz
sudo mkdir -p /opt/intel
sudo tar -xzf l_openvino_toolkit_*.tgz -C /opt/intel
sudo mv /opt/intel/l_openvino_toolkit_* /opt/intel/openvino_2024
source /opt/intel/openvino_2024/setupvars.sh
python3 -c "import openvino; print('OpenVINO', openvino.__version__)"
```

`setupvars.sh` sets `INTEL_OPENVINO_DIR`, prepends the library path
to `LD_LIBRARY_PATH`, and makes the `openvino` Python module
importable. **You must source it in every shell that builds or runs
UltimateSlice** (or add the env vars to your shell profile).

For other distros, grab the matching archive from the
[OpenVINO download page][ov-download].

[ov-download]: https://storage.openvinotoolkit.org/repositories/openvino/packages/

## Step 1 — Build ONNX Runtime from source

```bash
source /opt/intel/openvino_2024/setupvars.sh
./scripts/build_onnxruntime.sh --vendor openvino --device CPU_FP32
```

Flags:
- `--device CPU_FP32` (default) — works on iGPU + dGPU + CPU
  transparently via OpenVINO's device auto-selection. Best default
  unless you explicitly want to pin GPU.
- `--device GPU_FP16` — force FP16 execution on discrete Intel Arc.
  ~2× the throughput of FP32 on Arc for transformer models (SAM,
  MusicGen) but slightly lower precision.
- `--device AUTO` — OpenVINO picks at runtime.
- `--device HETERO:GPU,CPU` — run what fits on GPU, fall back to CPU
  for unsupported ops. Useful when your model has ops OpenVINO's GPU
  plugin doesn't implement.

The script clones `microsoft/onnxruntime` at tag `v1.24.2`, builds
with `--use_openvino <device>`, and produces
`$XDG_CACHE_HOME/ultimateslice/onnxruntime-openvino/build/Linux/Release/libonnxruntime.so.1.24.2`
plus symlinks. Expect 60-120 minutes on a modern machine.

Re-running the script with the same vendor is incremental — CMake's
build cache picks up where it left off.

## Step 2 — Build UltimateSlice

```bash
export ORT_LIB_LOCATION=$HOME/.cache/ultimateslice/onnxruntime-openvino/build/Linux/Release
export ORT_LIB_PROFILE=Release
source /opt/intel/openvino_2024/setupvars.sh   # must be sourced even for build
cargo build --features ai-openvino
```

`ORT_LIB_LOCATION` replaces the default "download a prebuilt" path
and tells `ort-sys` to link against your locally-built ONNX Runtime
instead. `ORT_LIB_PROFILE=Release` tells it to use the Release
subdirectory.

If you see `error: ORT 1.XX.X is not supported by ort 2.0.0-rc.12`
during `cargo build`, you built the wrong ORT version — the script
is pinned to `v1.24.2` for a reason. Re-run the script after
`rm -rf $HOME/.cache/ultimateslice/onnxruntime-openvino` to clean
the cache.

## Step 3 — Running

Every shell that launches the built binary must also have
`setupvars.sh` sourced, so `libopenvino.so` is on `LD_LIBRARY_PATH`:

```bash
source /opt/intel/openvino_2024/setupvars.sh
target/debug/ultimate-slice
```

**Alternative**: add OpenVINO to the system-wide linker config once:

```bash
echo "/opt/intel/openvino_2024/runtime/lib/intel64" | \
    sudo tee /etc/ld.so.conf.d/openvino.conf
sudo ldconfig
```

Then `libopenvino.so` is found without sourcing `setupvars.sh`.

Launch, open **Preferences → Models**, pick `Intel OpenVINO` from the
Backend dropdown. Applies on the next inference job.

## Verification

```bash
RUST_LOG=ort=debug cargo test --features ai-openvino \
    media::sam_cache::tests::segment_with_box_smoke -- \
    --ignored --nocapture 2>&1 | grep -iE "provider|execution|openvino"
```

Look for `Execution provider: OpenVINOExecutionProvider` or
`OpenVINO: registered successfully`. Expected SAM inference time on
Arc: 1-3 s (vs ~13 s CPU, ~3-5 s WebGPU).

Also smoke-test MODNet background removal and RIFE frame
interpolation via the Inspector UI — they use separate ONNX sessions
and each needs to independently register the OpenVINO EP.

## Troubleshooting

### "OpenVINO provider failed to load"

1. `setupvars.sh` not sourced. Check `echo $INTEL_OPENVINO_DIR` —
   should be set.
2. Wrong OpenVINO version. Run `python3 -c "import openvino;
   print(openvino.__version__)"`; must be 2024.4 or newer for ORT
   1.24.2.
3. `/etc/OpenCL/vendors/intel.icd` missing — reinstall
   `intel-opencl-icd`.
4. Kernel `i915` driver complaining in `dmesg` — Arc needs kernel
   ≥ 6.2 for stable Vulkan + compute.

### Build fails at `./build.sh --use_openvino`

1. Check the ORT build log for `Could not find OpenVINO`. The script
   relies on `$INTEL_OPENVINO_DIR` being set; run
   `source /opt/intel/openvino_2024/setupvars.sh` before invoking
   the script.
2. CMake ≥ 3.26 is required. Ubuntu 22.04 ships 3.22 — install a
   newer CMake from pip or snap.

### Arc GPU not detected at runtime

```bash
clinfo | grep "Device Name"
```

Should list your Arc. If not:
- Confirm kernel ≥ 6.2 (`uname -r`).
- Confirm firmware loaded: `dmesg | grep i915 | grep -i firmware`.
- User must be in `video` and `render` groups: `groups $USER`.

### Switching between `CPU_FP32` and `GPU_FP16`

Rebuild ONNX Runtime with the new `--device` flag:

```bash
rm -rf $HOME/.cache/ultimateslice/onnxruntime-openvino
./scripts/build_onnxruntime.sh --vendor openvino --device GPU_FP16
```

Clean rebuild is necessary because `--use_openvino` bakes the device
string into the compiled EP.

## See also

- [docs/gpu/webgpu.md](webgpu.md) — zero-friction Intel Arc
  alternative via Vulkan (70-80% of OpenVINO performance, no source
  build)
- [docs/gpu/troubleshooting.md](troubleshooting.md) — common failure
  modes
- [OpenVINO Execution Provider docs][ov-ep] (upstream Microsoft
  documentation)

[ov-ep]: https://onnxruntime.ai/docs/execution-providers/OpenVINO-ExecutionProvider.html
