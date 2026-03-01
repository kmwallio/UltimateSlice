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
