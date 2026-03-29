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
    rm -f "$BIN_DIR/$BINARY_NAME"
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

# --- Install ---
echo "Installing UltimateSlice to $PREFIX ..."

install -Dm755 "$BINARY_PATH"                          "$BIN_DIR/$BINARY_NAME"
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
echo "  Binary:  $BIN_DIR/$BINARY_NAME"
echo "  Desktop: $APPS_DIR/$APP_ID.desktop"
