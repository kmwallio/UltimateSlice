#!/usr/bin/env bash
# install.sh — Native Linux installer for UltimateSlice
#
# Usage:
#   sudo ./install.sh                      # install to /usr/local (default)
#   sudo ./install.sh --system             # install to /usr
#   ./install.sh --prefix=$HOME/.local     # user-level install
#   sudo ./install.sh --uninstall          # remove all installed files
#   ./install.sh --help                    # show this help
#
# Must be run from the repository root.
#
# GPU feature builds: if you want GPU acceleration, build the
# binary YOURSELF first with the appropriate feature flag before
# running this script, e.g.
#
#   cargo build --release --features ai-webgpu
#   sudo ./install.sh
#
# install.sh auto-detects any .so files cargo dropped next to the
# binary (WebGPU's libwebgpu_dawn.so is the common case) and
# installs them into a private $PREFIX/lib/ultimate-slice/
# directory alongside the real binary, with $BIN_DIR/$BINARY_NAME
# as a symlink. The binary's $ORIGIN rpath resolves through the
# symlink so no wrapper script or LD_LIBRARY_PATH setup is needed.
#
# CPU-only builds install exactly as before — binary straight
# into $BIN_DIR.

set -e

APP_ID="io.github.kmwallio.ultimateslice"
BINARY_NAME="ultimate-slice"
PREFIX="/usr/local"
UNINSTALL=false

# --- Argument parsing ---
for arg in "$@"; do
    case "$arg" in
        --prefix=*)
            PREFIX="${arg#--prefix=}"
            ;;
        --system)
            PREFIX="/usr"
            ;;
        --uninstall)
            UNINSTALL=true
            ;;
        --help|-h)
            cat <<'EOF'
Usage:
  sudo ./install.sh                      install to /usr/local (default)
  sudo ./install.sh --system             install to /usr
  ./install.sh --prefix=$HOME/.local     user-level install
  sudo ./install.sh --uninstall          remove all installed files
  ./install.sh --help                    show this help

Must be run from the repository root.

GPU feature builds:
  Build the binary yourself first with the matching Cargo feature:
      cargo build --release --features ai-webgpu
      sudo ./install.sh
  install.sh detects any .so files cargo dropped next to the binary
  (libwebgpu_dawn.so for the ai-webgpu feature is the common case)
  and installs them into $PREFIX/lib/ultimate-slice/ alongside the
  real binary, with $PREFIX/bin/ultimate-slice as a symlink. The
  binary's $ORIGIN rpath resolves through the symlink so no wrapper
  script or LD_LIBRARY_PATH setup is needed at launch.

  CPU-only builds install exactly as before — binary straight into
  $PREFIX/bin.
EOF
            exit 0
            ;;
        *)
            echo "Unknown argument: $arg"
            echo "Run '$0 --help' for usage."
            exit 1
            ;;
    esac
done

# --- Guard: must run from repo root ---
if [[ ! -f "Cargo.toml" || ! -d "data" ]]; then
    echo "Error: must be run from the UltimateSlice repository root."
    exit 1
fi

# --- Derived install paths ---
BIN_DIR="$PREFIX/bin"
APPS_DIR="$PREFIX/share/applications"
ICONS_APPS_DIR="$PREFIX/share/icons/hicolor/scalable/apps"
ICONS_MIME_DIR="$PREFIX/share/icons/hicolor/scalable/mimetypes"
MIME_DIR="$PREFIX/share/mime/packages"
METAINFO_DIR="$PREFIX/share/metainfo"

# --- Uninstall ---
if $UNINSTALL; then
    echo "Uninstalling UltimateSlice from $PREFIX ..."
    # Remove the bin entry. This may be a regular binary (CPU-only
    # install) or a symlink into the private lib directory (GPU
    # feature build). `rm -f` handles both transparently.
    rm -f "$BIN_DIR/$BINARY_NAME"
    # Remove the private lib directory if present — created only
    # by GPU feature builds that needed to ship a dynamic library
    # (e.g. libwebgpu_dawn.so alongside the `ai-webgpu` feature).
    if [[ -d "$PREFIX/lib/ultimate-slice" ]]; then
        rm -rf "$PREFIX/lib/ultimate-slice"
    fi
    rm -f "$APPS_DIR/$APP_ID.desktop"
    rm -f "$ICONS_APPS_DIR/$APP_ID.svg"
    rm -f "$ICONS_MIME_DIR/$APP_ID-file.svg"
    rm -f "$MIME_DIR/$APP_ID.mime.xml"
    rm -f "$METAINFO_DIR/$APP_ID.metainfo.xml"

    echo "Running post-uninstall database updates..."
    if command -v update-desktop-database &>/dev/null; then
        update-desktop-database "$APPS_DIR" 2>/dev/null || true
    fi
    if command -v update-mime-database &>/dev/null; then
        update-mime-database "$PREFIX/share/mime" 2>/dev/null || true
    fi
    if command -v gtk-update-icon-cache &>/dev/null; then
        gtk-update-icon-cache -f -t "$PREFIX/share/icons/hicolor" 2>/dev/null || true
    fi

    echo "Done. UltimateSlice has been uninstalled."
    exit 0
fi

# --- Build if binary is missing ---
BINARY_PATH="target/release/$BINARY_NAME"
if [[ ! -f "$BINARY_PATH" ]]; then
    echo "Binary not found at $BINARY_PATH — building with cargo..."
    cargo build --release
fi

# --- Detect GPU-build dynamic libraries needed by the binary ---
#
# When the binary is built with `--features ai-webgpu` (or any
# other GPU feature that pulls a prebuilt with a non-statically-
# linked runtime), `ort-sys` drops symlinks next to the binary at
# `target/release/*.so`, pointing at the real files under
# `~/.cache/ort.pyke.io/...`. The most common case today is
# `libwebgpu_dawn.so` for the WebGPU EP.
#
# IMPORTANT: we detect these by asking `readelf` which libraries
# the CURRENT binary actually needs at runtime (its `NEEDED` ELF
# entries), then intersecting that list with the files in
# `target/release/`. This correctly ignores STALE symlinks left
# over from a previous feature build — e.g. a leftover
# `libwebgpu_dawn.so` symlink from an old `cargo build --features
# ai-webgpu` after the user has since rebuilt with plain
# `cargo build --release`. Only libraries the binary will
# actually dlopen at launch get bundled into the install.
#
# If cargo dropped any matching .so files, we install them into
# a private directory at `$PREFIX/lib/ultimate-slice/` alongside
# the real binary, and symlink `$BIN_DIR/$BINARY_NAME` into that
# directory. The binary's ELF `DT_RUNPATH` is set to `$ORIGIN`
# by `.cargo/config.toml`, which resolves through the symlink to
# the real binary's directory — so the dynamic loader finds the
# bundled libraries at launch without any LD_LIBRARY_PATH
# fiddling or wrapper script.
#
# Pure CPU builds (binary has no bundled library dependencies)
# fall through to the classic "binary straight into $BIN_DIR"
# layout.
SHIPPED_LIBS=()
if command -v readelf &>/dev/null; then
    while IFS= read -r needed; do
        # readelf NEEDED entries are bare sonames like
        # "libgtk-4.so.1" or "libwebgpu_dawn.so". System libs
        # won't have a matching file in target/release — they
        # come from the user's distro and the install.sh has no
        # business shipping them. Bundled libs (only) land in
        # target/release via cargo's build-script output.
        if [[ -n "$needed" && -e "target/release/$needed" ]]; then
            SHIPPED_LIBS+=("target/release/$needed")
        fi
    done < <(readelf -d "$BINARY_PATH" 2>/dev/null \
        | awk '/\(NEEDED\)/ { gsub(/[\[\]]/, "", $NF); print $NF }')
else
    echo "  Warning: 'readelf' not found on \$PATH."
    echo "  install.sh needs readelf (from binutils) to detect GPU"
    echo "  feature libraries the binary depends on. Install it with"
    echo "  'apt install binutils' / 'dnf install binutils' and re-run."
    echo "  Continuing with the CPU-only install layout — GPU feature"
    echo "  builds will not have their runtime libraries bundled."
fi

PRIVATE_LIB_DIR="$PREFIX/lib/ultimate-slice"

# --- Install ---
echo "Installing UltimateSlice to $PREFIX ..."

if [[ ${#SHIPPED_LIBS[@]} -gt 0 ]]; then
    echo "  Detected GPU-feature build with ${#SHIPPED_LIBS[@]} bundled"
    echo "  dynamic librar$([ ${#SHIPPED_LIBS[@]} -eq 1 ] && echo y || echo ies):"
    for lib in "${SHIPPED_LIBS[@]}"; do
        echo "    $(basename "$lib")"
    done
    echo "  → installing binary + libs to $PRIVATE_LIB_DIR"
    echo "  → symlinking $BIN_DIR/$BINARY_NAME to the real binary"

    # Real binary goes into the private lib dir, alongside the
    # shared libs it depends on. `install` follows source
    # symlinks by default so the .so entries become regular files
    # in the destination.
    install -Dm755 "$BINARY_PATH" "$PRIVATE_LIB_DIR/$BINARY_NAME"
    for lib in "${SHIPPED_LIBS[@]}"; do
        install -Dm644 "$lib" "$PRIVATE_LIB_DIR/$(basename "$lib")"
    done

    # Relative symlink in $BIN_DIR pointing at the private lib
    # dir. Using a relative target (`../lib/ultimate-slice/…`)
    # means the install survives a later `mv $PREFIX /elsewhere`
    # without breaking.
    mkdir -p "$BIN_DIR"
    ln -sfT "../lib/ultimate-slice/$BINARY_NAME" "$BIN_DIR/$BINARY_NAME"
else
    # CPU-only build — classic layout.
    install -Dm755 "$BINARY_PATH"                      "$BIN_DIR/$BINARY_NAME"
fi

install -Dm644 "data/$APP_ID.desktop"                  "$APPS_DIR/$APP_ID.desktop"
install -Dm644 "data/$APP_ID.svg"                      "$ICONS_APPS_DIR/$APP_ID.svg"
install -Dm644 "data/$APP_ID-file.svg"                 "$ICONS_MIME_DIR/$APP_ID-file.svg"
install -Dm644 "data/$APP_ID.mime.xml"                 "$MIME_DIR/$APP_ID.mime.xml"
install -Dm644 "data/$APP_ID.metainfo.xml"             "$METAINFO_DIR/$APP_ID.metainfo.xml"

echo "Running post-install database updates..."
if command -v update-desktop-database &>/dev/null; then
    update-desktop-database "$APPS_DIR"
else
    echo "  Warning: update-desktop-database not found — skipping."
fi
if command -v update-mime-database &>/dev/null; then
    update-mime-database "$PREFIX/share/mime"
else
    echo "  Warning: update-mime-database not found — skipping."
fi
if command -v gtk-update-icon-cache &>/dev/null; then
    gtk-update-icon-cache -f -t "$PREFIX/share/icons/hicolor"
else
    echo "  Warning: gtk-update-icon-cache not found — skipping."
fi

echo ""
echo "UltimateSlice installed successfully."
if [[ ${#SHIPPED_LIBS[@]} -gt 0 ]]; then
    echo "  Launcher:   $BIN_DIR/$BINARY_NAME  →  $PRIVATE_LIB_DIR/$BINARY_NAME"
    echo "  Real bin:   $PRIVATE_LIB_DIR/$BINARY_NAME"
    echo "  Bundled libs:"
    for lib in "${SHIPPED_LIBS[@]}"; do
        echo "    $PRIVATE_LIB_DIR/$(basename "$lib")"
    done
else
    echo "  Binary:  $BIN_DIR/$BINARY_NAME"
fi
echo "  Desktop: $APPS_DIR/$APP_ID.desktop"
