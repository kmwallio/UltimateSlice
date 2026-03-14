#!/usr/bin/env python3
"""Calibrate blend-mode preview/export parity through the live MCP server.

Drives UltimateSlice via MCP to:
  1. Create a project with two overlapping video clips on separate tracks.
  2. For each blend mode, set it on the upper clip.
  3. Capture the Program Monitor preview frame.
  4. Export to MP4, extract the same frame with ffmpeg.
  5. Compare preview vs export with RMSE metrics.

Requires: numpy, Pillow, a running UltimateSlice instance with MCP socket.
Usage:  python3 tools/calibrate_mcp_blend_modes.py --source /path/to/video.mp4
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import numpy as np
from PIL import Image


BLEND_MODES = ["normal", "multiply", "screen", "overlay", "add", "difference", "soft_light"]

# Seek position: 1 second into the timeline (in nanoseconds)
SEEK_NS = 1_000_000_000


def mcp_socket_path() -> str:
    return os.path.join(
        os.environ.get("XDG_RUNTIME_DIR", "/tmp"), "ultimateslice-mcp.sock"
    )


def call_tool(name: str, args: dict | None = None, retries: int = 3) -> dict:
    args = args or {}
    requests = [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "mcp-blend-calibrate", "version": "1.0"},
            },
        },
        {
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": name, "arguments": args},
        },
    ]

    for attempt in range(retries):
        try:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.settimeout(30)
            s.connect(mcp_socket_path())
            break
        except (ConnectionRefusedError, FileNotFoundError) as e:
            if attempt == retries - 1:
                print(
                    f"\nERROR: Cannot connect to MCP socket at {mcp_socket_path()}\n"
                    "Make sure UltimateSlice is running.\n",
                    file=sys.stderr,
                )
                raise
            time.sleep(1)

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


def stabilize_seek(pos_ns: int, settle: float = 0.8) -> None:
    """Seek and wait for the preview to settle."""
    for _ in range(2):
        resp = call_tool("seek_playhead", {"timeline_pos_ns": pos_ns})
        ensure_ok("seek_playhead", tool_payload(resp))
        time.sleep(settle)


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


def extract_frame_from_mp4(mp4_path: str, time_s: float, out_ppm: str) -> None:
    """Extract a single frame from an MP4 at the given time as PPM."""
    subprocess.run(
        [
            "ffmpeg", "-y",
            "-ss", f"{time_s:.3f}",
            "-i", mp4_path,
            "-frames:v", "1",
            "-f", "image2",
            "-pix_fmt", "rgb24",
            out_ppm,
        ],
        check=True,
        capture_output=True,
    )


def setup_project(source_path: str) -> tuple[str, str]:
    """Create a 2-track project with the same clip on both tracks.
    Returns (base_clip_id, overlay_clip_id)."""
    print("  Creating project...")
    resp = call_tool("create_project", {"title": "BlendModeCalibration"})
    ensure_ok("create_project", tool_payload(resp))
    time.sleep(2.0)

    # Import media
    print(f"  Importing media: {source_path}")
    resp = call_tool("import_media", {"path": source_path})
    ensure_ok("import_media", tool_payload(resp))
    time.sleep(1.0)

    # Add base clip on track 0 (5 seconds)
    print("  Adding base clip (track 0)...")
    resp = call_tool("add_clip", {
        "source_path": source_path,
        "track_index": 0,
        "timeline_start_ns": 0,
        "source_in_ns": 0,
        "source_out_ns": 5_000_000_000,
    })
    base_payload = tool_payload(resp)
    ensure_ok("add_clip (base)", base_payload)
    base_clip_id = base_payload.get("clip_id", "")
    time.sleep(3.0)

    # Seek to stabilize the first clip before adding the second
    print("  Stabilizing base clip...")
    stabilize_seek(SEEK_NS, 2.0)

    # Add overlay clip on track 2 (separate video track, overlapping same range)
    print("  Adding overlay clip (track 2)...")
    resp = call_tool("add_clip", {
        "source_path": source_path,
        "track_index": 2,
        "timeline_start_ns": 0,
        "source_in_ns": 0,
        "source_out_ns": 5_000_000_000,
    })
    overlay_payload = tool_payload(resp)
    ensure_ok("add_clip (overlay)", overlay_payload)
    overlay_clip_id = overlay_payload.get("clip_id", "")
    time.sleep(2.0)

    # Verify clips
    resp = call_tool("list_clips")
    clips = tool_payload(resp)
    print(f"  Timeline has {len(clips)} clips")
    for c in clips:
        print(f"    clip={c.get('id','?')[:8]}  track={c.get('track_index')}  blend={c.get('blend_mode','?')}")

    return base_clip_id, overlay_clip_id


def run_calibration(source_path: str, work_dir: str, settle: float) -> dict:
    """Run blend mode calibration and return results dict."""
    base_clip_id, overlay_clip_id = setup_project(source_path)

    results = {}

    for mode in BLEND_MODES:
        print(f"\n--- Blend mode: {mode} ---")

        # Set blend mode on overlay clip
        resp = call_tool("set_clip_blend_mode", {
            "clip_id": overlay_clip_id,
            "blend_mode": mode,
        })
        ensure_ok("set_clip_blend_mode", tool_payload(resp))
        time.sleep(0.3)

        # Seek and stabilize
        stabilize_seek(SEEK_NS, settle)

        # Capture preview frame
        preview_path = os.path.join(work_dir, f"preview_{mode}.ppm")
        resp = call_tool("export_displayed_frame", {"path": preview_path})
        ensure_ok("export_displayed_frame", tool_payload(resp))

        # Export MP4
        export_path = os.path.join(work_dir, f"export_{mode}.mp4")
        print(f"  Exporting MP4: {export_path}")
        resp = call_tool("export_mp4", {"path": export_path})
        export_payload = tool_payload(resp)
        ensure_ok("export_mp4", export_payload)

        # Extract frame from exported MP4 at same timestamp
        export_frame_path = os.path.join(work_dir, f"export_frame_{mode}.ppm")
        time_s = SEEK_NS / 1_000_000_000.0
        extract_frame_from_mp4(export_path, time_s, export_frame_path)

        # Load and compare
        if not os.path.exists(preview_path):
            print(f"  WARNING: preview frame missing: {preview_path}")
            results[mode] = {"error": "preview frame missing"}
            continue
        if not os.path.exists(export_frame_path):
            print(f"  WARNING: export frame missing: {export_frame_path}")
            results[mode] = {"error": "export frame missing"}
            continue

        preview_img = np.array(Image.open(preview_path).convert("RGB"))
        export_img = np.array(Image.open(export_frame_path).convert("RGB"))

        # Resize export to match preview if dimensions differ
        if preview_img.shape != export_img.shape:
            ph, pw = preview_img.shape[:2]
            export_pil = Image.open(export_frame_path).convert("RGB").resize(
                (pw, ph), Image.LANCZOS
            )
            export_img = np.array(export_pil)
            print(f"  Resized export frame to {pw}x{ph} to match preview")

        rmse = compute_rmse(preview_img, export_img)
        results[mode] = {
            "rmse": rmse,
            "preview_shape": list(preview_img.shape),
            "export_shape": list(export_img.shape),
        }
        total = rmse["total"]
        status = "PASS" if total < 10.0 else ("WARN" if total < 25.0 else "FAIL")
        print(f"  RMSE: R={rmse['r']:.2f}  G={rmse['g']:.2f}  B={rmse['b']:.2f}  total={total:.2f}  [{status}]")

    return results


def main():
    parser = argparse.ArgumentParser(description="Blend mode preview/export parity calibration")
    parser.add_argument("--source", required=True, help="Absolute path to a source video file (≥5s)")
    parser.add_argument("--work-dir", default=None, help="Working directory for temp files (default: auto)")
    parser.add_argument("--settle", type=float, default=0.8, help="Settle time after seek (seconds)")
    parser.add_argument("--report", default=None, help="Path to write JSON report")
    args = parser.parse_args()

    source = os.path.abspath(args.source)
    if not os.path.isfile(source):
        print(f"ERROR: source file not found: {source}", file=sys.stderr)
        sys.exit(1)

    if args.work_dir:
        work_dir = os.path.abspath(args.work_dir)
        os.makedirs(work_dir, exist_ok=True)
    else:
        work_dir = tempfile.mkdtemp(prefix="blend_cal_")

    print(f"Source: {source}")
    print(f"Work dir: {work_dir}")
    print(f"MCP socket: {mcp_socket_path()}")
    print()

    results = run_calibration(source, work_dir, args.settle)

    # Summary
    print("\n=== SUMMARY ===")
    all_pass = True
    for mode in BLEND_MODES:
        r = results.get(mode, {})
        if "error" in r:
            print(f"  {mode:12s}  ERROR: {r['error']}")
            all_pass = False
        else:
            total = r["rmse"]["total"]
            status = "PASS" if total < 10.0 else ("WARN" if total < 25.0 else "FAIL")
            if status != "PASS":
                all_pass = False
            print(f"  {mode:12s}  RMSE={total:6.2f}  [{status}]")

    report = {
        "source": source,
        "work_dir": work_dir,
        "blend_modes": results,
        "all_pass": all_pass,
    }

    report_path = args.report or os.path.join(work_dir, "blend_mode_report.json")
    with open(report_path, "w") as f:
        json.dump(report, f, indent=2)
    print(f"\nReport written to: {report_path}")

    sys.exit(0 if all_pass else 1)


if __name__ == "__main__":
    main()
