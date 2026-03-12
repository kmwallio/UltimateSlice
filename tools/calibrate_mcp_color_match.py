#!/usr/bin/env python3
"""Calibrate preview/export color parity through the live MCP server.

This script drives UltimateSlice through MCP, sweeps clip color sliders, and
compares:
  - Program preview frame (`export_displayed_frame`)
  - Exported MP4 frame (via `export_mp4` + ffmpeg frame extraction)

It writes a JSON report with per-slider/per-value RMSE metrics and plugin
availability checks for GStreamer and FFmpeg frei0r modules.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path

import numpy as np
from PIL import Image


SLIDERS: dict[str, tuple[float, float, float, int]] = {
    "shadows": (-1.0, 1.0, 0.0, 9),
    "midtones": (-1.0, 1.0, 0.0, 9),
    "highlights": (-1.0, 1.0, 0.0, 9),
    "exposure": (-1.0, 1.0, 0.0, 9),
    "black_point": (-1.0, 1.0, 0.0, 9),
    "highlights_warmth": (-1.0, 1.0, 0.0, 9),
    "highlights_tint": (-1.0, 1.0, 0.0, 9),
    "midtones_warmth": (-1.0, 1.0, 0.0, 9),
    "midtones_tint": (-1.0, 1.0, 0.0, 9),
    "shadows_warmth": (-1.0, 1.0, 0.0, 9),
    "shadows_tint": (-1.0, 1.0, 0.0, 9),
}


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
                "clientInfo": {"name": "mcp-calibrate", "version": "1.0"},
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


def tool_payload(resp: dict) -> dict:
    if "error" in resp:
        raise RuntimeError(f"MCP transport error: {resp['error']}")
    text = resp.get("result", {}).get("content", [{}])[0].get("text", "{}")
    try:
        payload = json.loads(text)
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"Failed to decode MCP payload: {text}") from exc
    return payload


def ensure_ok(name: str, payload: dict) -> None:
    if payload.get("ok") is False:
        raise RuntimeError(f"{name} failed: {payload}")
    if "success" in payload and payload.get("success") is False:
        raise RuntimeError(f"{name} failed: {payload}")


def run(cmd: list[str], check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, check=check, text=True, capture_output=True)


def check_ffmpeg_module(module_name: str) -> bool:
    cmd = [
        "ffmpeg",
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=64x64:d=0.05",
        "-vf",
        f"frei0r=filter_name={module_name}",
        "-frames:v",
        "1",
        "-f",
        "null",
        "-",
    ]
    return run(cmd, check=False).returncode == 0


def plugin_report() -> dict:
    ffmpeg_filters = run(["ffmpeg", "-hide_banner", "-filters"], check=False)
    filter_text = (ffmpeg_filters.stdout or "") + (ffmpeg_filters.stderr or "")
    has_frei0r_filter = " frei0r " in filter_text or "frei0r" in filter_text
    report = {
        "gstreamer": {
            "frei0r-filter-coloradj-rgb": shutil.which("gst-inspect-1.0") is not None
            and run(["gst-inspect-1.0", "frei0r-filter-coloradj-rgb"], check=False).returncode == 0,
            "frei0r-filter-3-point-color-balance": shutil.which("gst-inspect-1.0") is not None
            and run(["gst-inspect-1.0", "frei0r-filter-3-point-color-balance"], check=False).returncode
            == 0,
        },
        "ffmpeg": {
            "has_frei0r_filter": has_frei0r_filter,
            "module_coloradj_RGB": check_ffmpeg_module("coloradj_RGB"),
            "module_3-point-color-balance": check_ffmpeg_module("3-point-color-balance"),
            "module_three_point_balance": check_ffmpeg_module("three_point_balance"),
        },
    }
    report["cross_runtime_candidate"] = (
        report["gstreamer"]["frei0r-filter-coloradj-rgb"]
        and report["ffmpeg"]["module_coloradj_RGB"]
        and report["gstreamer"]["frei0r-filter-3-point-color-balance"]
        and report["ffmpeg"]["module_three_point_balance"]
    )
    return report


def compute_rmse(a: np.ndarray, b: np.ndarray) -> dict:
    a64 = a.astype(np.float64)
    b64 = b.astype(np.float64)
    diff = a64 - b64
    return {
        "r": float(np.sqrt(np.mean(diff[..., 0] ** 2))),
        "g": float(np.sqrt(np.mean(diff[..., 1] ** 2))),
        "b": float(np.sqrt(np.mean(diff[..., 2] ** 2))),
        "total": float(np.sqrt(np.mean(diff**2))),
    }


def extract_export_frame(export_mp4: Path, out_png: Path, seek_seconds: float) -> None:
    run(
        [
            "ffmpeg",
            "-y",
            "-v",
            "error",
            "-ss",
            f"{seek_seconds:.6f}",
            "-i",
            str(export_mp4),
            "-frames:v",
            "1",
            str(out_png),
        ]
    )


@dataclass
class SessionContext:
    clip_id: str
    source_out_ns: int


def setup_session(media_path: Path, clip_duration_ns: int) -> SessionContext:
    payload = tool_payload(call_tool("create_project", {"title": "MCP Calibration"}))
    ensure_ok("create_project", payload)

    payload = tool_payload(call_tool("import_media", {"path": str(media_path)}))
    ensure_ok("import_media", payload)

    source_out_ns = max(1_000_000, min(clip_duration_ns, 2_000_000_000))
    payload = tool_payload(
        call_tool(
            "add_clip",
            {
                "source_path": str(media_path),
                "track_index": 0,
                "timeline_start_ns": 0,
                "source_in_ns": 0,
                "source_out_ns": source_out_ns,
            },
        )
    )
    ensure_ok("add_clip", payload)
    clip_id = payload.get("clip_id")
    if not clip_id:
        raise RuntimeError(f"add_clip did not return clip_id: {payload}")

    # Stabilize preview behavior.
    for tool_name, args in (
        ("set_proxy_mode", {"mode": "off"}),
        ("set_preview_quality", {"quality": "full"}),
        ("set_realtime_preview", {"enabled": False}),
    ):
        p = tool_payload(call_tool(tool_name, args))
        ensure_ok(tool_name, p)

    return SessionContext(clip_id=clip_id, source_out_ns=source_out_ns)


def slider_values(vmin: float, vmax: float, default: float, steps: int) -> list[float]:
    vals = np.linspace(vmin, vmax, max(2, steps)).tolist()
    if default not in vals:
        vals.append(default)
    vals = sorted(set(round(v, 6) for v in vals))
    return vals


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Calibrate preview/export parity via MCP")
    parser.add_argument(
        "--media",
        default=str(Path("Sample-Media") / "calibration_chart.mp4"),
        help="Calibration source media path.",
    )
    parser.add_argument("--out", default="/tmp/us_mcp_color_calib", help="Output directory.")
    parser.add_argument("--steps", type=int, default=9, help="Sweep samples per slider.")
    parser.add_argument(
        "--seek-ns",
        type=int,
        default=500_000_000,
        help="Timeline timestamp for frame capture in nanoseconds.",
    )
    parser.add_argument(
        "--settle-ms",
        type=int,
        default=250,
        help="Wait time after seek/set before exporting preview frame.",
    )
    parser.add_argument(
        "--check-plugins-only",
        action="store_true",
        help="Only report plugin availability, skip MCP calibration.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    out_dir = Path(args.out).resolve()
    out_dir.mkdir(parents=True, exist_ok=True)

    report: dict = {"plugins": plugin_report(), "sliders": {}}
    if args.check_plugins_only:
        out_path = out_dir / "mcp_plugin_report.json"
        out_path.write_text(json.dumps(report, indent=2))
        print(f"Wrote {out_path}")
        return 0

    media_path = Path(args.media).resolve()
    if not media_path.exists():
        raise FileNotFoundError(f"Media not found: {media_path}")

    if not os.path.exists(mcp_socket_path()):
        raise RuntimeError(
            f"MCP socket not found at {mcp_socket_path()}. Start UltimateSlice with --mcp first."
        )

    ctx = setup_session(media_path, clip_duration_ns=2_000_000_000)
    seek_ns = min(max(0, args.seek_ns), max(0, ctx.source_out_ns - 1_000_000))
    seek_s = seek_ns / 1_000_000_000.0

    for slider, (vmin, vmax, default, _default_steps) in SLIDERS.items():
        vals = slider_values(vmin, vmax, default, args.steps)
        slider_rows = []
        for value in vals:
            color_args = {"clip_id": ctx.clip_id, slider: value}
            payload = tool_payload(call_tool("set_clip_color", color_args))
            ensure_ok("set_clip_color", payload)
            payload = tool_payload(call_tool("seek_playhead", {"timeline_pos_ns": seek_ns}))
            ensure_ok("seek_playhead", payload)
            time.sleep(max(0.0, args.settle_ms / 1000.0))

            preview_ppm = out_dir / f"preview_{slider}_{value:+.3f}.ppm"
            export_mp4 = out_dir / f"export_{slider}_{value:+.3f}.mp4"
            export_png = out_dir / f"export_{slider}_{value:+.3f}.png"

            payload = tool_payload(call_tool("export_displayed_frame", {"path": str(preview_ppm)}))
            ensure_ok("export_displayed_frame", payload)
            payload = tool_payload(call_tool("export_mp4", {"path": str(export_mp4)}))
            ensure_ok("export_mp4", payload)

            extract_export_frame(export_mp4, export_png, seek_s)

            preview_img = np.array(Image.open(preview_ppm).convert("RGB"), dtype=np.float32)
            export_img = np.array(Image.open(export_png).convert("RGB"), dtype=np.float32)
            if preview_img.shape != export_img.shape:
                export_img = np.array(
                    Image.fromarray(export_img.astype(np.uint8)).resize(
                        (preview_img.shape[1], preview_img.shape[0]), Image.Resampling.LANCZOS
                    ),
                    dtype=np.float32,
                )

            rmse = compute_rmse(preview_img, export_img)
            print(f"{slider:18s} {value:+.3f}  rmse={rmse['total']:.3f}")
            slider_rows.append({"value": value, "rmse": rmse})

        report["sliders"][slider] = {
            "range": {"min": vmin, "max": vmax, "default": default},
            "samples": slider_rows,
            "summary": {
                "mean_rmse": float(np.mean([r["rmse"]["total"] for r in slider_rows])),
                "max_rmse": float(np.max([r["rmse"]["total"] for r in slider_rows])),
                "min_rmse": float(np.min([r["rmse"]["total"] for r in slider_rows])),
            },
        }

    out_path = out_dir / "mcp_color_calibration_report.json"
    out_path.write_text(json.dumps(report, indent=2))
    print(f"Wrote {out_path}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(f"error: {exc}", file=sys.stderr)
        raise SystemExit(1)
