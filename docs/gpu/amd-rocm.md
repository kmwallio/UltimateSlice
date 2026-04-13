# AMD ROCm

The `ai-rocm` Cargo feature uses ONNX Runtime's ROCm execution
provider — AMD's CUDA equivalent — which delivers the best
performance on supported AMD GPUs by leveraging MIOpen's hand-tuned
kernels and `rocBLAS` for matmul. Like OpenVINO, there is **no
prebuilt path**: you must source-build ONNX Runtime with the ROCm
EP enabled.

If you'd rather skip the source build, [WebGPU](webgpu.md) runs on
AMD too via RADV/Vulkan, delivers roughly 60-80% of ROCm's
throughput, and works on AMD GPUs (including many older cards) that
aren't on AMD's ROCm compatibility list. This page is for users who
want maximum performance on supported hardware.

`ai-rocm` also bridges through to `whisper-rs/hipblas`, giving you
GPU-accelerated subtitle generation for free.

## Prerequisites

### Hardware

ROCm supports only a subset of AMD GPUs. As of ROCm 6.2
(what ONNX Runtime 1.24.2 targets):

- **Officially supported**: RDNA2 (RX 6800 / 6900 / 7900 XT family),
  RDNA3 (RX 7900 XTX / XT / GRE), CDNA2/3 (MI200/MI300 datacenter).
- **Unofficially works** via `HSA_OVERRIDE_GFX_VERSION`: RDNA1 (RX
  5700/5600) and Vega 20. Older Polaris (RX 580 / 590) is not
  supported by modern ROCm at all — use WebGPU instead.
- Check the [ROCm compatibility matrix][rocm-compat] for your exact
  card before investing time in the source build.

[rocm-compat]: https://rocm.docs.amd.com/projects/install-on-linux/en/latest/reference/system-requirements.html

### System software

- **AMD ROCm 6.2+** (older ROCm 5.x may work but has not been
  tested). Install via AMD's official packages:
  - Ubuntu 24.04 / 22.04: [amdgpu-install][amdgpu-install] script,
    then `amdgpu-install --usecase=rocm`.
  - Fedora: third-party COPR repo (unofficial).
  - Arch: `rocm-hip-sdk rocm-opencl-sdk miopen-hip rocblas`.
- **Build toolchain**: GCC/G++ ≥ 11, CMake ≥ 3.26, Python 3.10+,
  ~25 GB free disk, ~16 GB RAM for the linker.
- **User group membership**: your user must be in both `video` and
  `render` groups. After `usermod -a -G video,render $USER`, log out
  and back in.

[amdgpu-install]: https://rocm.docs.amd.com/projects/install-on-linux/en/latest/install/amdgpu-install.html

### Verify the ROCm install

```bash
rocminfo | grep -A1 "Agent "        # lists GPUs; Agent 1 is CPU, Agent 2+ are GPUs
rocm-smi                            # live GPU status / temperatures
hipconfig --full                    # confirms HIP is installed
ls /opt/rocm/lib/libMIOpen*         # MIOpen kernels for DNN ops
```

If `rocminfo` doesn't list your GPU as an Agent, ROCm can't see it.
Check `dmesg | grep amdgpu` for kernel-level errors and confirm you
rebooted after adding yourself to the `video` / `render` groups.

## Step 1 — Build ONNX Runtime from source

```bash
./scripts/build_onnxruntime.sh --vendor rocm --rocm-home /opt/rocm
```

The script clones `microsoft/onnxruntime` at `v1.24.2`, runs
`./build.sh --use_rocm --rocm_home /opt/rocm`, and produces
`$XDG_CACHE_HOME/ultimateslice/onnxruntime-rocm/build/Linux/Release/libonnxruntime.so.1.24.2`.
Expect 60-120 minutes on a 16-thread machine.

If ROCm is installed at a non-standard location, pass `--rocm-home
/path/to/rocm` explicitly.

### Unsupported GPU workaround

If you have an RDNA1 card (RX 5700 / 5600), set
`HSA_OVERRIDE_GFX_VERSION` to the nearest supported target before
running the script:

```bash
export HSA_OVERRIDE_GFX_VERSION=10.3.0    # mimic RDNA2 target
./scripts/build_onnxruntime.sh --vendor rocm
```

This works on many RDNA1 GPUs because the compute-side instruction
set is similar enough. Your mileage may vary — if MIOpen crashes at
runtime, fall back to WebGPU.

## Step 2 — Build UltimateSlice

```bash
export ORT_LIB_LOCATION=$HOME/.cache/ultimateslice/onnxruntime-rocm/build/Linux/Release
export ORT_LIB_PROFILE=Release
cargo build --features ai-rocm
```

`cargo build --features ai-rocm` also pulls in the `hipblas` feature
of `whisper-rs`, so subtitle generation accelerates too. You should
see `whisper-rs-sys` rebuild with HIP support during `cargo build`.

## Step 3 — Running

ROCm's shared libraries live under `/opt/rocm/lib` which should
already be on the linker path after install (`ldconfig -p | grep
libMIOpen` should hit). If not:

```bash
echo "/opt/rocm/lib" | sudo tee /etc/ld.so.conf.d/rocm.conf
sudo ldconfig
```

Launch the binary, open **Preferences → Models**, pick `AMD ROCm`
from the Backend dropdown. Applies on the next inference job.

If the dropdown shows `AMD ROCm (unavailable)`, the EP is compiled
in but ort's runtime load failed. See troubleshooting below.

## Verification

```bash
RUST_LOG=ort=debug cargo test --features ai-rocm \
    media::sam_cache::tests::segment_with_box_smoke -- \
    --ignored --nocapture 2>&1 | grep -iE "provider|execution|rocm"
```

Look for `Execution provider: ROCMExecutionProvider`. Expected SAM
inference time on RX 7900 XTX: under 2 s (vs ~13 s CPU, ~4-6 s
WebGPU).

Test all four ONNX caches through their UI paths:
- SAM: run `segment_with_box` via MCP `generate_sam_mask`.
- MODNet: enable background removal on a clip in the Inspector.
- RIFE: enable frame interpolation on a clip in the Inspector.
- MusicGen: use the music generator.
- STT (bonus): run `generate_subtitles` — should also be faster.

## Troubleshooting

### "ROCm provider failed to load" at session.commit

1. `rocminfo` doesn't list the GPU → kernel driver problem. Check
   `dmesg | grep amdgpu`.
2. User not in `render` group → `groups` output doesn't include
   `render`. Fix with `sudo usermod -a -G render $USER` and log out.
3. `libMIOpen.so.1` missing → `ldconfig -p | grep MIOpen` returns
   nothing. Reinstall `miopen-hip`.
4. GPU target not compiled into MIOpen. Set
   `HSA_OVERRIDE_GFX_VERSION=10.3.0` (or your nearest target) in
   the shell before launching UltimateSlice.

### Build fails with `Could not find rocm`

The script defaults to `/opt/rocm` for `--rocm-home`. If ROCm is
installed elsewhere (e.g. `/opt/rocm-6.2.0`), pass that path
explicitly:

```bash
./scripts/build_onnxruntime.sh --vendor rocm --rocm-home /opt/rocm-6.2.0
```

### `hipcc: command not found` during ORT build

`/opt/rocm/bin` must be on your `PATH`. Add to your shell rc:

```bash
export PATH=/opt/rocm/bin:$PATH
```

Then re-run the script.

### Subtitle generation still uses CPU after building with `ai-rocm`

`whisper-rs-sys` may have cached a previous build without `hipblas`.
Force a rebuild of just whisper:

```bash
touch $(find ~/.cargo/registry -name 'whisper-rs-sys-*' -type d)
cargo clean -p whisper-rs-sys
cargo build --features ai-rocm
```

### Performance is worse than expected

Check `rocm-smi` during inference — GPU utilization should hit
80-100% on the transformer steps. If it's under 30%, MIOpen is
falling back to slow kernels, usually because of:
- Unsupported op (ORT's ROCm EP is less complete than CUDA). Check
  `RUST_LOG=ort=debug` output for "CPU fallback" messages.
- Batch size 1 with small transformer — MIOpen's autotuner prefers
  larger batches. Nothing to do without code changes to the model.

## See also

- [docs/gpu/webgpu.md](webgpu.md) — zero-friction AMD alternative via
  Vulkan. Works on many AMD GPUs that aren't on the ROCm support
  list.
- [docs/gpu/troubleshooting.md](troubleshooting.md) — common failure
  modes across all GPU backends
- [ROCm Execution Provider docs][rocm-ep] (upstream Microsoft)

[rocm-ep]: https://onnxruntime.ai/docs/execution-providers/ROCm-ExecutionProvider.html
