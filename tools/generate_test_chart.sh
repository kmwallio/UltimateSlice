#!/usr/bin/env bash
# Generate a 1920x1080 test chart for color calibration.
# Produces a 2-second MP4 with SMPTE-like color bars, gradients, and skin-tone patches.
set -euo pipefail

OUT_DIR="${1:-$(dirname "$0")/../Sample-Media}"
mkdir -p "$OUT_DIR"
OUT="$OUT_DIR/calibration_chart.mp4"
FRAME="$OUT_DIR/calibration_chart.png"

ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i "smptebars=size=1920x1080:duration=2:rate=24,drawbox=x=0:y=810:w=1920:h=270:color=black@1:t=fill,drawbox=x=0:y=810:w=192:h=135:color=0xD2A87A@1:t=fill,drawbox=x=192:y=810:w=192:h=135:color=0xB58666@1:t=fill,drawbox=x=384:y=810:w=192:h=135:color=0x8D5524@1:t=fill,drawbox=x=576:y=810:w=192:h=135:color=0xE8D4B8@1:t=fill,drawbox=x=768:y=810:w=192:h=135:color=0x5C3A21@1:t=fill,drawbox=x=960:y=810:w=192:h=135:color=0x808080@1:t=fill,drawbox=x=1152:y=810:w=192:h=135:color=0x404040@1:t=fill,drawbox=x=1344:y=810:w=192:h=135:color=0xC0C0C0@1:t=fill,drawbox=x=1536:y=810:w=192:h=135:color=0x200000@1:t=fill,drawbox=x=1728:y=810:w=192:h=135:color=0x002000@1:t=fill" \
  -c:v libx264 -preset fast -crf 1 -pix_fmt yuv420p \
  "$OUT"

# Also export a single PNG frame for offline calibration
ffmpeg -y -hide_banner -loglevel error \
  -i "$OUT" -frames:v 1 "$FRAME"

echo "Test chart:  $OUT"
echo "Test frame:  $FRAME"
