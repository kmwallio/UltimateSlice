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

Useful playback-tuning toggles:

- `python3 tools/mcp_call.py set_realtime_preview '{"enabled":true}'`
- `python3 tools/mcp_call.py set_experimental_preview_optimizations '{"enabled":true}'`
- `python3 tools/mcp_call.py set_background_prerender '{"enabled":true}'`
- `python3 tools/mcp_call.py get_performance_snapshot '{}'`

`set_background_prerender` enables temporary disk prerender of complex upcoming overlap sections (cleaned when the app/player closes).

## Keyframe MCP examples

Set a scale keyframe on a clip at an absolute timeline time:

```bash
python3 tools/mcp_call.py set_clip_keyframe '{"clip_id":"<clip-id>","property":"scale","timeline_pos_ns":1000000000,"value":1.35}'
```

Remove the keyframe for that property at the same timeline time:

```bash
python3 tools/mcp_call.py remove_clip_keyframe '{"clip_id":"<clip-id>","property":"scale","timeline_pos_ns":1000000000}'
```

Use `list_clips` to discover `clip_id` values and inspect phase-1 keyframe arrays (`scale_keyframes`, `opacity_keyframes`, `position_x_keyframes`, `position_y_keyframes`, `volume_keyframes`).

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
