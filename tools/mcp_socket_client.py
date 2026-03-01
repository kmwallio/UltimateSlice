#!/usr/bin/env python3
"""stdio <-> Unix socket bridge for UltimateSlice MCP.

Reads newline-delimited JSON-RPC from stdin, forwards to a Unix socket,
and writes socket responses/notifications back to stdout.
"""

from __future__ import annotations

import argparse
import os
import socket
import sys
import threading


def default_socket_path() -> str:
    runtime_dir = os.environ.get("XDG_RUNTIME_DIR")
    if runtime_dir:
        return os.path.join(runtime_dir, "ultimateslice-mcp.sock")
    return "/tmp/ultimateslice-mcp.sock"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Bridge MCP JSON-RPC between stdio and UltimateSlice Unix socket."
    )
    parser.add_argument(
        "--socket",
        dest="socket_path",
        default=default_socket_path(),
        help="Path to UltimateSlice MCP Unix socket (default: %(default)s).",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()

    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        sock.connect(args.socket_path)
    except OSError as exc:
        print(
            f"mcp_socket_client: failed to connect to {args.socket_path}: {exc}",
            file=sys.stderr,
        )
        return 2

    stop_event = threading.Event()

    def socket_to_stdout() -> None:
        buf = b""
        try:
            while not stop_event.is_set():
                chunk = sock.recv(4096)
                if not chunk:
                    break
                buf += chunk
                while b"\n" in buf:
                    line, buf = buf.split(b"\n", 1)
                    sys.stdout.buffer.write(line + b"\n")
                    sys.stdout.buffer.flush()
            if buf:
                sys.stdout.buffer.write(buf)
                sys.stdout.buffer.flush()
        except OSError:
            pass

    recv_thread = threading.Thread(target=socket_to_stdout, daemon=True)
    recv_thread.start()

    exit_code = 0
    try:
        while True:
            line = sys.stdin.buffer.readline()
            if not line:
                break
            try:
                sock.sendall(line)
            except OSError as exc:
                print(f"mcp_socket_client: socket write failed: {exc}", file=sys.stderr)
                exit_code = 3
                break
    except KeyboardInterrupt:
        exit_code = 130
    finally:
        stop_event.set()
        try:
            sock.shutdown(socket.SHUT_WR)
        except OSError:
            pass
        recv_thread.join(timeout=1.0)
        sock.close()

    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
