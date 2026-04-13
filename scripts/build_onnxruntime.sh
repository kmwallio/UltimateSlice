#!/usr/bin/env bash
# Build Microsoft ONNX Runtime 1.24.2 from source with a vendor-
# specific execution provider enabled. Produces a library tree that
# the `ort` crate at version 2.0.0-rc.12 can link against when pointed
# at via the ORT_LIB_LOCATION environment variable.
#
# ONNX Runtime 1.24.2 is pinned intentionally — it matches the ABI
# that ort-sys 2.0.0-rc.12 targets (confirmed from ort-sys's
# dist.txt, which references `ms@1.24.2` in every prebuilt URL).
# Building a different version will produce a binary that links but
# crashes at runtime on symbol-version mismatch.
#
# Usage:
#   scripts/build_onnxruntime.sh --vendor openvino [--openvino-home /opt/intel/openvino_2024]
#   scripts/build_onnxruntime.sh --vendor rocm     [--rocm-home /opt/rocm]
#   scripts/build_onnxruntime.sh --vendor openvino --device GPU_FP16
#
# Flags:
#   --vendor {openvino|rocm}  Which execution provider to build (required).
#   --openvino-home PATH      Root of the Intel OpenVINO Toolkit install.
#                             Defaults to $INTEL_OPENVINO_DIR if set, else
#                             /opt/intel/openvino_2024.
#   --rocm-home PATH          Root of the ROCm install. Defaults to
#                             /opt/rocm.
#   --device DEVICE           OpenVINO device spec. One of CPU_FP32,
#                             GPU_FP16, GPU_FP32, AUTO, HETERO:GPU,CPU.
#                             Default: CPU_FP32 (also covers Intel iGPU).
#                             Use GPU_FP16 for discrete Intel Arc.
#   --jobs N                  Parallel build jobs. Default: nproc.
#   --yes                     Skip the disk-space / time confirmation.
#   --help                    Print this message and exit.
#
# Output (on success):
#   $XDG_CACHE_HOME/ultimateslice/onnxruntime-<vendor>/build/Linux/Release/
#     libonnxruntime.so.1.24.2  (and symlinks libonnxruntime.so.1, .so)
#
# Use the built library with:
#   export ORT_LIB_LOCATION=$XDG_CACHE_HOME/ultimateslice/onnxruntime-<vendor>/build/Linux/Release
#   export ORT_LIB_PROFILE=Release
#   cargo build --features ai-<vendor>
#
# System prerequisites (caller's responsibility — this script does
# NOT install them):
#   * GCC/G++ >= 11, CMake >= 3.26, Python 3.10+, git
#   * For --vendor openvino: Intel OpenVINO Toolkit 2024.4 (source
#     `setupvars.sh` before invoking this script so `INTEL_OPENVINO_DIR`
#     is set and OpenVINO's Python module is importable).
#   * For --vendor rocm: ROCm 6.x (install via AMD's official .deb /
#     .rpm packages). User must be in `video` and `render` groups.
#
# Build cost (typical):
#   Disk:  ~25 GB (clone + object files + libraries)
#   Time:  60-120 min on a 16-thread machine, more on laptops.
#   RAM:   ~16 GB peak; linker step is the bottleneck.
set -euo pipefail

ORT_VERSION="v1.24.2"
ORT_REPO="https://github.com/microsoft/onnxruntime.git"
BUILD_ROOT="${XDG_CACHE_HOME:-$HOME/.cache}/ultimateslice"

VENDOR=""
OPENVINO_HOME="${INTEL_OPENVINO_DIR:-/opt/intel/openvino_2024}"
ROCM_HOME="/opt/rocm"
OPENVINO_DEVICE="CPU_FP32"
PARALLEL_JOBS="$(nproc)"
SKIP_CONFIRM=""

print_help() {
	sed -n '2,50p' "$0" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
	case "$1" in
		--vendor) VENDOR="$2"; shift 2 ;;
		--openvino-home) OPENVINO_HOME="$2"; shift 2 ;;
		--rocm-home) ROCM_HOME="$2"; shift 2 ;;
		--device) OPENVINO_DEVICE="$2"; shift 2 ;;
		--jobs) PARALLEL_JOBS="$2"; shift 2 ;;
		--yes|-y) SKIP_CONFIRM="yes"; shift ;;
		--help|-h) print_help; exit 0 ;;
		*) echo "error: unknown flag: $1" >&2; print_help; exit 1 ;;
	esac
done

if [[ -z "$VENDOR" ]]; then
	echo "error: --vendor is required (openvino | rocm)" >&2
	print_help
	exit 1
fi

case "$VENDOR" in
	openvino)
		if [[ ! -d "$OPENVINO_HOME" ]]; then
			echo "error: OpenVINO toolkit not found at $OPENVINO_HOME" >&2
			echo "Install from https://storage.openvinotoolkit.org/repositories/openvino/packages/" >&2
			echo "then source setupvars.sh or pass --openvino-home /path/to/openvino" >&2
			exit 1
		fi
		export INTEL_OPENVINO_DIR="$OPENVINO_HOME"
		BUILD_FLAGS=(
			--use_openvino "$OPENVINO_DEVICE"
		)
		;;
	rocm)
		if [[ ! -d "$ROCM_HOME" ]]; then
			echo "error: ROCm not found at $ROCM_HOME" >&2
			echo "Install via AMD's official packages (https://rocm.docs.amd.com/)," >&2
			echo "then re-run with --rocm-home /opt/rocm (or your install path)" >&2
			exit 1
		fi
		BUILD_FLAGS=(
			--use_rocm
			--rocm_home "$ROCM_HOME"
		)
		;;
	*)
		echo "error: --vendor must be 'openvino' or 'rocm' (got: $VENDOR)" >&2
		exit 1
		;;
esac

BUILD_DIR="$BUILD_ROOT/onnxruntime-$VENDOR"
mkdir -p "$BUILD_ROOT"

echo "==================================================================="
echo "  UltimateSlice — ONNX Runtime source build"
echo "==================================================================="
echo "  Target version:  Microsoft ONNX Runtime $ORT_VERSION"
echo "  Vendor:          $VENDOR"
if [[ "$VENDOR" == "openvino" ]]; then
	echo "  OpenVINO home:   $OPENVINO_HOME"
	echo "  OpenVINO device: $OPENVINO_DEVICE"
elif [[ "$VENDOR" == "rocm" ]]; then
	echo "  ROCm home:       $ROCM_HOME"
fi
echo "  Parallel jobs:   $PARALLEL_JOBS"
echo "  Build dir:       $BUILD_DIR"
echo
echo "  Disk usage:      ~25 GB"
echo "  Estimated time:  60-120 min (16-thread machine)"
echo "==================================================================="

if [[ -z "$SKIP_CONFIRM" ]]; then
	read -r -p "Proceed? [y/N] " response
	if [[ ! "$response" =~ ^[Yy]$ ]]; then
		echo "Aborted." >&2
		exit 1
	fi
fi

# Clone or reuse a shallow checkout at v1.24.2. Use a vendor-specific
# directory because CMake's build cache is vendor-specific and would
# conflict if we shared one clone between vendors.
if [[ ! -d "$BUILD_DIR/.git" ]]; then
	echo "Cloning ONNX Runtime $ORT_VERSION into $BUILD_DIR ..."
	git clone --depth 1 --branch "$ORT_VERSION" --recurse-submodules \
		"$ORT_REPO" "$BUILD_DIR"
else
	echo "Reusing existing clone at $BUILD_DIR"
	# Ensure we're on the right tag even if someone previously pulled
	# a newer commit. Do not fetch — stay offline and reuse whatever
	# was cloned the first time.
	(cd "$BUILD_DIR" && git -c advice.detachedHead=false checkout "$ORT_VERSION") || true
fi

cd "$BUILD_DIR"

echo "Building ONNX Runtime — this will take a while ..."
./build.sh \
	--config Release \
	--build_shared_lib \
	--parallel "$PARALLEL_JOBS" \
	--skip_tests \
	--skip_submodule_sync \
	--compile_no_warning_as_error \
	--allow_running_as_root \
	"${BUILD_FLAGS[@]}"

# Sanity-check the output. ort-sys expects to find libonnxruntime.so
# (with a version suffix) in the build output directory pointed at
# by ORT_LIB_LOCATION.
LIB_PATH="$BUILD_DIR/build/Linux/Release"
if [[ ! -f "$LIB_PATH/libonnxruntime.so" && ! -f "$LIB_PATH/libonnxruntime.so.1.24.2" ]]; then
	echo "error: build completed but libonnxruntime.so not found under $LIB_PATH" >&2
	echo "Check the ONNX Runtime build log above for errors." >&2
	exit 1
fi

SO_SIZE="$(ls -lh "$LIB_PATH"/libonnxruntime.so* 2>/dev/null | head -1 | awk '{print $5}')"
echo
echo "==================================================================="
echo "  Build succeeded."
echo "==================================================================="
echo "  libonnxruntime.so:  $LIB_PATH/libonnxruntime.so ($SO_SIZE)"
echo
echo "  Next steps:"
echo
echo "    export ORT_LIB_LOCATION=$LIB_PATH"
echo "    export ORT_LIB_PROFILE=Release"
if [[ "$VENDOR" == "openvino" ]]; then
	echo "    source $OPENVINO_HOME/setupvars.sh   # makes libopenvino.so findable"
fi
echo "    cargo build --features ai-$VENDOR"
echo
echo "  For verification, run:"
echo "    RUST_LOG=ort=debug cargo test media::sam_cache::tests::segment_with_box_smoke -- --ignored --nocapture 2>&1 | grep -i 'provider\\|execution'"
echo "  and confirm the log shows the expected EP was registered."
echo "==================================================================="
