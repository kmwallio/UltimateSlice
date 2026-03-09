#!/usr/bin/env bash
set -euo pipefail

# Repeatable perf matrix runner for Program Monitor playback.
# Requires a running UltimateSlice instance with MCP socket enabled.
#
# Usage:
#   tools/proxy_perf_matrix.sh <app-pid> [project-fcpxml]
#
# Output directory:
#   /tmp/proxyperf-<epoch>/

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <app-pid> [project-fcpxml]" >&2
  exit 2
fi

APP_PID="$1"
PROJECT_PATH="${2:-$(pwd)/Sample-Media/three-video-tracks.fcpxml}"
OUT_DIR="/tmp/proxyperf-$(date +%s)"
mkdir -p "$OUT_DIR"

MCP_CALL="$(pwd)/tools/mcp_call.py"
if [[ ! -x "$MCP_CALL" ]]; then
  echo "Expected executable helper at $MCP_CALL" >&2
  exit 3
fi

python3 "$MCP_CALL" open_fcpxml "{\"path\":\"$PROJECT_PATH\"}" >/dev/null
python3 "$MCP_CALL" set_proxy_mode '{"mode":"quarter_res"}' >/dev/null
python3 "$MCP_CALL" set_preview_quality '{"quality":"quarter"}' >/dev/null
python3 "$MCP_CALL" set_playback_priority '{"priority":"smooth"}' >/dev/null

printf 'label,hardware_accel,occlusion,realtime\n' > "$OUT_DIR/matrix.csv"

for hw in false true; do
  for occ in false true; do
    for rt in false true; do
      label="hw_${hw}_occ_${occ}_rt_${rt}"
      echo "Running $label"

      python3 "$MCP_CALL" set_hardware_acceleration "{\"enabled\":$hw}" > "$OUT_DIR/resp_${label}_hw.json"
      python3 "$MCP_CALL" set_experimental_preview_optimizations "{\"enabled\":$occ}" > "$OUT_DIR/resp_${label}_occ.json"
      python3 "$MCP_CALL" set_realtime_preview "{\"enabled\":$rt}" > "$OUT_DIR/resp_${label}_rt.json"
      python3 "$MCP_CALL" seek_playhead '{"timeline_pos_ns":9000000000}' > "$OUT_DIR/resp_${label}_seek.json"
      python3 "$MCP_CALL" play '{}' > "$OUT_DIR/resp_${label}_play.json"
      sleep 1

      perf stat -x, -e task-clock,context-switches,cpu-migrations,page-faults,cycles,instructions,cache-misses -p "$APP_PID" -- sleep 6 \
        2> "$OUT_DIR/perf_${label}.csv"

      python3 "$MCP_CALL" pause '{}' > "$OUT_DIR/resp_${label}_pause.json"
      sleep 1

      printf '%s,%s,%s,%s\n' "$label" "$hw" "$occ" "$rt" >> "$OUT_DIR/matrix.csv"
    done
  done
done

echo "Matrix complete: $OUT_DIR"

