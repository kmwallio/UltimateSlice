# Python MCP Socket Commands

Use this guide to connect to a running UltimateSlice instance over the MCP Unix socket using Python.

## Prerequisites

- UltimateSlice is running.
- **Preferences → Integration → Enable MCP socket server** is enabled.
- Socket path is available at:
  - `$XDG_RUNTIME_DIR/ultimateslice-mcp.sock` (default), or
  - your custom path passed with `--socket`.

## Start the Python socket client

From the repository root:

```bash
python3 tools/mcp_socket_client.py
```

With custom socket path:

```bash
python3 tools/mcp_socket_client.py --socket /tmp/ultimateslice-mcp.sock
```

## Send JSON-RPC commands (stdin)

You can pipe newline-delimited JSON requests into the client.

Initialize:

```bash
printf '%s\n' \
'{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"python-cli","version":"1.0"}}}' \
| python3 tools/mcp_socket_client.py
```

Initialize + list tracks:

```bash
printf '%s\n' \
'{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"python-cli","version":"1.0"}}}' \
'{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_tracks","arguments":{}}}' \
| python3 tools/mcp_socket_client.py
```

## Helper scripts for perf/FPS checks

- `python3 tools/mcp_call.py <tool_name> '<json-args>'`
  - Sends one MCP `tools/call` over the socket and prints the response JSON.
- `python3 tools/proxy_fps_regression.py --project Sample-Media/three-video-tracks.fcpxml`
  - Runs a relative FPS regression check (optimized config vs baseline) using playhead-speed measurement.
- `tools/proxy_perf_matrix.sh <app-pid> <project.fcpxml>`
  - Runs the 2x2x2 hardware/occlusion/realtime perf matrix and writes per-run `perf stat` artifacts.
- `python3 tools/calibrate_mcp_color_match.py --media Sample-Media/calibration_chart.mp4 --out /tmp/us_mcp_color_calib`
  - Sweeps full clip color controls (primary + extended grading) via MCP and measures Program Monitor preview vs exported frame RMSE.
  - Use `--sliders temperature,tint,shadows,midtones,highlights` to run focused retune sweeps on a subset of controls.
  - Use `--lut-path /absolute/path/look.cube` to include a clip LUT in preview/export parity sweeps.
  - Use `--proxy-mode half_res|quarter_res` to force proxy-backed preview capture; if `--lut-path` is set while `--proxy-mode off`, calibration auto-switches to `quarter_res` so LUT processing is active.
  - Optional stability hardening: `--sample-retries <N>` runs each sample multiple times and keeps the median-attempt RMSE; `--neutral-baseline-retries <N>` retries neutral baseline capture and selects the median-attempt baseline.
  - Supports export capture modes: default `--export-mode mp4` or low-loss `--export-mode prores_mov` (via MCP export preset).
  - Uses repeated seek/settle stabilization (configurable with `--seek-repeats`) and re-applies each sample state before export capture to reduce stale-frame races.
  - Captures a neutral baseline RMSE first, then records per-sample deltas from neutral to help separate global baseline offset from control-specific divergence.
  - Reports now include signed preview/export bias (`bias`) per sample and neutral baseline (`export - preview`) to support direction-aware calibration fitting.
  - Writes three pass signals per sample: `pass_absolute` (`--threshold-total-rmse`), `pass_delta` (`--threshold-delta-rmse`), and combined `pass`.
  - Includes default-sample stale-frame retry protection (`--default-sample-retries`) and reports frei0r compatibility diagnostics (`three_point_balance` naming on FFmpeg).
- `python3 tools/mcp_parity_smoke_check.py --media Sample-Media/calibration_chart.mp4`
  - Runs a low-sample parity sweep wrapper intended for CI/automation smoke checks.
  - Defaults to low-loss export mode (`prores_mov`) and fails fast on large normalized deltas for focus sliders (`contrast`, `saturation`) or unusually high neutral baseline RMSE.
  - Supports `--sliders ...` passthrough for targeted smoke checks on specific controls.
  - Also forwards `--sample-retries`, `--neutral-baseline-retries`, `--lut-path`, and `--proxy-mode`.
  - Supports multiple media clips in one pass by repeating `--media`; each run writes a per-media report and one aggregate summary (`smoke_aggregate_report.json`) with mean guardrails.
- `python3 tools/compare_mcp_parity_reports.py --baseline <report-or-dir> --candidate <report-or-dir>`
  - Compares two calibration reports with weighted improvement scoring (`mean_abs_delta` + `max_abs_delta`) and endpoint guardrails.
  - Default guardrails protect historically fragile endpoints: `shadows +1`, `midtones -1`, `highlights -1`.
  - Use repeatable `--guardrail slider:value:max_regression` for custom thresholds and `--out` to save a JSON decision artifact.
- `python3 tools/compare_mcp_parity_profiles.py --profile chart <base> <cand> --profile natural <base> <cand>`
  - Runs the same comparator logic across multiple media profiles and computes weighted aggregate score.
  - Candidate passes only if each profile passes its score/guardrails and aggregate weighted score meets threshold.
  - Optional `--profile-weight name=weight` supports profile weighting (for example, bias toward natural-footage parity).
- `python3 tools/run_mcp_parity_retune_cycle.py --profile-media chart=... --baseline-report chart=... --profile-media natural=... --baseline-report natural=... --out /tmp/us_retune_cycle`
  - One-command loop: runs focused calibration sweeps per profile, single-profile comparison gates, then multi-profile aggregate gate.
  - Automatically adds temperature endpoint guardrails (`temperature@2000`, `temperature@10000`) unless `--no-temperature-guardrails` is set.
  - Writes per-profile compare artifacts plus `retune_cycle_summary.json`.
- `python3 tools/optimize_mcp_temperature_gain.py --profile-media chart=... --baseline-report chart=... --profile-media natural=... --baseline-report natural=... --out /tmp/us_temp_opt`
  - Sweeps candidate export gain sets and runs a full retune cycle for each trial.
  - Supports piecewise cool-side temperature gains (`US_EXPORT_COOL_TEMP_GAIN_FAR`, `US_EXPORT_COOL_TEMP_GAIN_NEAR`) with legacy fallback (`US_EXPORT_COOL_TEMP_GAIN`), plus optional tonal-side gains for endpoint controls (`US_EXPORT_SHADOWS_POS_GAIN`, `US_EXPORT_MIDTONES_NEG_GAIN`, `US_EXPORT_HIGHLIGHTS_NEG_GAIN`).
  - Use `--cool-far-gains` and `--cool-near-gains` to search cool-temperature curve shape; when omitted, both fall back to `--gains`.
  - Picks the best candidate by pass status + weighted aggregate multi-profile score.
  - Writes `parity_gain_optimization_summary.json` with trial scores and selected gain set.

Example multi-media smoke run:

```bash
python3 tools/mcp_parity_smoke_check.py \
  --media Sample-Media/calibration_chart.mp4 \
  --media Sample-Media/GX010426.MP4 \
  --out /tmp/us_mcp_parity_smoke_multi
```

Useful playback-tuning toggles:

- `python3 tools/mcp_call.py set_realtime_preview '{"enabled":true}'`
- `python3 tools/mcp_call.py set_experimental_preview_optimizations '{"enabled":true}'`
- `python3 tools/mcp_call.py set_background_prerender '{"enabled":true}'`
- `python3 tools/mcp_call.py get_performance_snapshot '{}'`
- `python3 tools/mcp_call.py save_project_with_media '{"path":"/absolute/path/MyProject.uspxml"}'`

`set_background_prerender` enables temporary disk prerender of complex upcoming overlap sections (cleaned when the app/player closes).

## Keyframe MCP examples

Set a scale keyframe on a clip at an absolute timeline time:

```bash
python3 tools/mcp_call.py set_clip_keyframe '{"clip_id":"<clip-id>","property":"scale","timeline_pos_ns":1000000000,"value":1.35}'
```

Set a keyframe with custom Bezier controls for its outgoing segment:

```bash
python3 tools/mcp_call.py set_clip_keyframe '{"clip_id":"<clip-id>","property":"scale","timeline_pos_ns":1000000000,"value":1.35,"bezier_controls":{"x1":0.20,"y1":0.05,"x2":0.80,"y2":0.95}}'
```

Remove the keyframe for that property at the same timeline time:

```bash
python3 tools/mcp_call.py remove_clip_keyframe '{"clip_id":"<clip-id>","property":"scale","timeline_pos_ns":1000000000}'
```

Use `list_clips` to discover `clip_id` values and inspect phase-1 keyframe arrays (`scale_keyframes`, `opacity_keyframes`, `position_x_keyframes`, `position_y_keyframes`, `volume_keyframes`). Keyframes may include optional `bezier_controls` for custom tangent-authored segments.

## Frei0r effects MCP examples

List all available frei0r filter plugins:

```bash
python3 tools/mcp_call.py list_frei0r_plugins '{}'
```

Add a "cartoon" effect to a clip:

```bash
python3 tools/mcp_call.py add_clip_frei0r_effect '{"clip_id":"<clip-id>","plugin_name":"cartoon"}'
```

Add a "cairogradient" effect with a string parameter override:

```bash
python3 tools/mcp_call.py add_clip_frei0r_effect '{"clip_id":"<clip-id>","plugin_name":"cairogradient","string_params":{"blend-mode":"multiply"}}'
```

List effects applied to a clip:

```bash
python3 tools/mcp_call.py list_clip_frei0r_effects '{"clip_id":"<clip-id>"}'
```

Update effect parameters (numeric and string):

```bash
python3 tools/mcp_call.py set_clip_frei0r_effect_params '{"clip_id":"<clip-id>","effect_id":"<effect-id>","params":{"Triplevel":0.7}}'
python3 tools/mcp_call.py set_clip_frei0r_effect_params '{"clip_id":"<clip-id>","effect_id":"<effect-id>","params":{},"string_params":{"blend-mode":"screen"}}'
```

Reorder effects on a clip:

```bash
python3 tools/mcp_call.py reorder_clip_frei0r_effects '{"clip_id":"<clip-id>","effect_ids":["<eid2>","<eid1>"]}'
```

Remove an effect:

```bash
python3 tools/mcp_call.py remove_clip_frei0r_effect '{"clip_id":"<clip-id>","effect_id":"<effect-id>"}'
```

## `.mcp.json` server entry

This repository includes a Python socket entry:

```json
"ultimate-slice-python-socket": {
  "command": "python3",
  "args": ["tools/mcp_socket_client.py"],
  "cwd": "UltimateSlice"
}
```

## Troubleshooting

- **Connection failed / socket not found**:
  - Confirm MCP socket server is enabled in Preferences.
  - Verify `echo "$XDG_RUNTIME_DIR"` is set and socket file exists.
- **No response**:
  - Ensure each request is one JSON object per line.
  - Ensure `initialize` is sent before tool calls.
