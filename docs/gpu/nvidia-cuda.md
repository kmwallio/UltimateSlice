# NVIDIA CUDA

The `ai-cuda` Cargo feature uses ONNX Runtime's CUDA execution
provider via prebuilt binaries distributed through `cdn.pyke.io`.
It's the fastest path on NVIDIA hardware for the four ONNX-backed
caches (SAM, MODNet, RIFE, MusicGen), and also enables GPU
acceleration for Whisper-based subtitle generation via
`whisper-rs/cuda`.

> **Not verified at runtime.** This document describes the build and
> runtime setup, but the project's CI machine has no NVIDIA GPU, so
> the `cargo build --features ai-cuda` path is compile-checked only.
> Expect to hit minor config issues on real NVIDIA hardware; please
> open an issue if you find one and we'll amend the docs.

## Prerequisites

### Hardware
- NVIDIA GPU with compute capability ≥ 5.2 (Maxwell or newer).
- Turing / Ampere / Ada / Hopper / Blackwell are all well-tested.

### Software
- NVIDIA proprietary driver **≥ 535** for CUDA 12.x, or **≥ 575**
  for CUDA 13.x.
- CUDA Toolkit **12.x** or **13.x**. Pick the one that matches what
  ort's prebuilt expects (see "Choosing CUDA 12 vs 13" below).
- cuDNN **9.x** matching your CUDA major version.
- Linux. Windows CUDA is supported by ort itself but has additional
  `ORT_VCPKG_TARGET` plumbing that this project does not cover.

### Distro package names

| Distro | Packages |
|---|---|
| Ubuntu 24.04 | `nvidia-driver-550 cuda-toolkit-12-4 libcudnn9-cuda-12` |
| Fedora 41 | `xorg-x11-drv-nvidia-cuda cuda-toolkit libcudnn9` (via RPM Fusion) |
| Arch | `nvidia cuda cudnn` |

Verify the install with:

```bash
nvidia-smi              # shows driver + GPU
nvcc --version          # shows CUDA toolkit version (must be 12.x or 13.x)
ldconfig -p | grep cudart   # confirms libcudart.so is on the library path
```

## Choosing CUDA 12 vs 13

The `ort-sys` build script picks one of two prebuilt flavors at
`cargo build` time:

- `x86_64-unknown-linux-gnu+cu12.tar.lzma2` — CUDA 12.x
- `x86_64-unknown-linux-gnu+cu13.tar.lzma2` — CUDA 13.x

Set `ORT_CUDA_VERSION` to pin which one gets downloaded:

```bash
export ORT_CUDA_VERSION=12
cargo build --features ai-cuda
```

Leaving it unset lets ort-sys choose the default flavor for the
current release (currently 12 for ort 2.0.0-rc.12). Pin it
explicitly for reproducible builds.

## Building

```bash
export ORT_CUDA_VERSION=12     # or 13
cargo build --features ai-cuda
```

First run downloads ~500 MB of prebuilt ORT + CUDA support libs from
`cdn.pyke.io/0/pyke:ort-rs/ms@1.24.2/`.

If you're behind a firewall, set `ORT_SKIP_DOWNLOAD=1` and point
`ORT_LIB_LOCATION` at a pre-fetched copy — see the
[troubleshooting page](troubleshooting.md#prebuilt-download-fails).

## STT (whisper-rs) bridge

`ai-cuda` also bridges through to `whisper-rs/cuda`, so picking this
feature gives you GPU-accelerated subtitle generation for free (3-5×
speedup over CPU on typical NVIDIA hardware). No additional flags
required.

## Selecting CUDA at runtime

Open **Preferences → Models**, pick `NVIDIA CUDA` from the Backend
dropdown. Applies to the next inference job.

If you picked CUDA but the dropdown shows `NVIDIA CUDA (unavailable)`,
the build contains the CUDA EP but ort couldn't load it at startup —
usually a driver / library path problem. See the troubleshooting
section below.

## Verification

```bash
RUST_LOG=ort=debug cargo test --features ai-cuda \
    media::sam_cache::tests::segment_with_box_smoke -- \
    --ignored --nocapture 2>&1 | grep -iE "provider|execution"
```

Look for `Execution provider: CUDAExecutionProvider`. Expect SAM
inference to drop from ~13 s on CPU to sub-second range on a modern
NVIDIA GPU.

## Runtime requirements summary

The built binary dynamically links:
- `libcudart.so.12` (or `.13`) — CUDA runtime
- `libcublas.so.12` — cuBLAS
- `libcudnn.so.9` — cuDNN
- `libcufft.so.11` — cuFFT (some models)
- `libonnxruntime_providers_cuda.so` — bundled in the ort prebuilt

All except the last must be on `LD_LIBRARY_PATH` or discoverable via
`ldconfig`. A standard CUDA toolkit install puts them in
`/usr/local/cuda/lib64`; ensure that's in `/etc/ld.so.conf.d/` or
your shell's `LD_LIBRARY_PATH`.

## Troubleshooting

### "CUDA provider failed to load" at session.commit time

The CUDA EP shared library loaded but `cudaSetDevice` or a similar
init call failed. Causes:

1. Driver too old. `nvidia-smi` reports a driver version; the
   [CUDA compatibility matrix][cuda-compat] lists the minimum for
   each CUDA toolkit major. Ubuntu 24.04 users in particular should
   check — the default `nvidia-driver-535` is fine for CUDA 12 but
   not 13.
2. Missing cuDNN. `ldconfig -p | grep cudnn` should return a hit for
   `libcudnn.so.9`.
3. GPU in exclusive compute mode. `nvidia-smi -c DEFAULT` to fix.
4. Running under a container without `--gpus all` / nvidia-container-
   toolkit configured.

[cuda-compat]: https://docs.nvidia.com/deploy/cuda-compatibility/

### Build fails with "libcudart.so not found"

The build script downloads ort's CUDA EP but does NOT vendor
libcudart — that comes from your local CUDA toolkit install.
Install the CUDA toolkit before building.

### Build downloads the wrong CUDA variant

Check `ORT_CUDA_VERSION`. If it's unset or mismatched, ort-sys
picks a default that might not match your installed toolkit. Pin
explicitly and `cargo clean` if you previously had the wrong
variant cached.

## See also

- [docs/gpu/webgpu.md](webgpu.md) — cross-vendor alternative (no
  toolkit install needed, slightly lower performance)
- [docs/gpu/troubleshooting.md](troubleshooting.md) — common failure
  modes across all GPU backends
