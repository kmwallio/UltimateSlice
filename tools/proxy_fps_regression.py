#!/usr/bin/env python3
"""Relative FPS regression check for Program Monitor playback via MCP."""

from __future__ import annotations

import argparse
import json
import os
import socket
import statistics
import time


def mcp_socket_path() -> str:
    return os.path.join(os.environ.get("XDG_RUNTIME_DIR", "/tmp"), "ultimateslice-mcp.sock")


def call_tool(name: str, args: dict | None = None) -> dict:
    args = args or {}
    requests = [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "fps-regression", "version": "1.0"},
            },
        },
        {
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": name, "arguments": args},
        },
    ]

    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(mcp_socket_path())
    for req in requests:
        s.sendall((json.dumps(req, separators=(",", ":")) + "\n").encode())

    buf = b""
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
                s.close()
                return msg
    s.close()
    return {}


def tool_result_payload(resp: dict) -> dict:
    text = (
        resp.get("result", {})
        .get("content", [{}])[0]
        .get("text", "{}")
    )
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return {}


def get_playhead_ns() -> int:
    resp = call_tool("get_playhead_position")
    payload = tool_result_payload(resp)
    return int(payload.get("timeline_pos_ns", 0))


def set_combo(hardware: bool, occlusion: bool, realtime: bool) -> None:
    call_tool("set_hardware_acceleration", {"enabled": hardware})
    call_tool("set_experimental_preview_optimizations", {"enabled": occlusion})
    call_tool("set_realtime_preview", {"enabled": realtime})


def measure_effective_fps(duration_s: float, nominal_fps: float, start_ns: int) -> float:
    call_tool("seek_playhead", {"timeline_pos_ns": start_ns})
    start_playhead = get_playhead_ns()
    call_tool("play")
    time.sleep(duration_s)
    end_playhead = get_playhead_ns()
    call_tool("pause")

    timeline_delta_s = max(0.0, (end_playhead - start_playhead) / 1_000_000_000.0)
    realtime_ratio = timeline_delta_s / duration_s if duration_s > 0 else 0.0
    return nominal_fps * realtime_ratio


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="Relative FPS regression check")
    p.add_argument(
        "--project",
        default="Sample-Media/three-video-tracks.fcpxml",
        help="Path to sample project file",
    )
    p.add_argument("--duration", type=float, default=3.0, help="Seconds per sample")
    p.add_argument("--repeats", type=int, default=3, help="Samples per combo")
    p.add_argument("--nominal-fps", type=float, default=24.0, help="Project nominal FPS")
    p.add_argument(
        "--min-ratio",
        type=float,
        default=0.95,
        help="optimized_fps must be >= baseline_fps * min_ratio",
    )
    p.add_argument(
        "--start-ns",
        type=int,
        default=9_000_000_000,
        help="Playhead start position (ns) for overlap region",
    )
    return p.parse_args()


def main() -> int:
    args = parse_args()
    project_path = os.path.abspath(args.project)

    call_tool("open_fcpxml", {"path": project_path})
    call_tool("set_proxy_mode", {"mode": "quarter_res"})
    call_tool("set_preview_quality", {"quality": "quarter"})

    # Baseline: no occlusion/realtime, software path preference.
    set_combo(hardware=False, occlusion=False, realtime=False)
    baseline_samples = [
        measure_effective_fps(args.duration, args.nominal_fps, args.start_ns)
        for _ in range(max(1, args.repeats))
    ]
    baseline = statistics.median(baseline_samples)

    # Optimized: occlusion enabled, realtime disabled to avoid steady-state overhead.
    set_combo(hardware=False, occlusion=True, realtime=False)
    optimized_samples = [
        measure_effective_fps(args.duration, args.nominal_fps, args.start_ns)
        for _ in range(max(1, args.repeats))
    ]
    optimized = statistics.median(optimized_samples)

    ratio = optimized / baseline if baseline > 0 else 0.0
    result = {
        "baseline_samples_fps": baseline_samples,
        "optimized_samples_fps": optimized_samples,
        "baseline_median_fps": baseline,
        "optimized_median_fps": optimized,
        "ratio": ratio,
        "min_ratio": args.min_ratio,
        "pass": ratio >= args.min_ratio,
    }
    print(json.dumps(result, indent=2))
    return 0 if result["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

