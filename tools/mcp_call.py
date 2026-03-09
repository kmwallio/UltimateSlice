#!/usr/bin/env python3
"""Call a single UltimateSlice MCP tool over the Unix socket."""

from __future__ import annotations

import json
import os
import socket
import sys


def socket_path() -> str:
    runtime = os.environ.get("XDG_RUNTIME_DIR", "/tmp")
    return os.path.join(runtime, "ultimateslice-mcp.sock")


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: mcp_call.py <tool_name> [args_json]", file=sys.stderr)
        return 2

    tool_name = sys.argv[1]
    args = json.loads(sys.argv[2]) if len(sys.argv) > 2 else {}

    requests = [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "mcp-call", "version": "1.0"},
            },
        },
        {
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": tool_name, "arguments": args},
        },
    ]

    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(socket_path())
    for req in requests:
        s.sendall((json.dumps(req, separators=(",", ":")) + "\n").encode())

    buf = b""
    response = {}
    while True:
        chunk = s.recv(4096)
        if not chunk:
            break
        buf += chunk
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            if not line.strip():
                continue
            msg = json.loads(line.decode())
            if msg.get("id") == 2:
                response = msg
                s.close()
                print(json.dumps(response, separators=(",", ":")))
                return 0

    s.close()
    print(json.dumps(response, separators=(",", ":")))
    return 1


if __name__ == "__main__":
    raise SystemExit(main())

