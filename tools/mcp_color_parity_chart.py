#!/usr/bin/env python3
"""Preview/export colour parity probe on a synthetic flat-patch chart.

Drives a running UltimateSlice instance over the MCP Unix socket:

1. renders an 84-patch colour chart (grey ramp, saturated hues, pastels,
   skin tones, random colours) and encodes it as a tagged BT.709 limited-range
   H.264 clip so both the GStreamer preview and the FFmpeg export decode the
   same values;
2. creates a project with that clip, then for each scenario sets a grade over
   MCP (`set_clip_color`, `set_clip_lut`, `set_clip_hsl_qualifier`), captures
   a live Program Monitor still (`capture_reference_still`) and an
   export-pipeline still (`capture_export_compare_still`);
3. compares the two by patch centres (robust to the preview's downscale) and
   prints mean / p95 / max absolute 8-bit error, the signed RGB bias and the
   worst patch.

Run with proxies, background prerender and (ideally) hardware decode off so
the live still is the real GStreamer colour chain:

    tools/mcp_call.py set_proxy_mode '{"mode":"off"}'
    tools/mcp_call.py set_background_prerender '{"enabled":false}'
    tools/mcp_call.py set_hardware_acceleration '{"enabled":false}'
    python3 tools/mcp_color_parity_chart.py --lut ~/LUTs/some.cube

Requires numpy + Pillow + ffmpeg. The identity scenario is the measurement
floor (GStreamer's YUV→RGB conversion sits ~2 code values under FFmpeg's);
a grade scenario within a code value of the floor is as good as it gets.
"""

from __future__ import annotations

import argparse
import colorsys
import json
import os
import subprocess
import sys
import tempfile
import time

import numpy as np
from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
MCP_CALL = os.path.join(HERE, "mcp_call.py")
COLS, ROWS = 12, 7
W, H = 1920, 1080


def call(tool: str, args: dict | None = None) -> dict:
    out = subprocess.run(
        [sys.executable, "-I", MCP_CALL, tool, json.dumps(args or {})],
        capture_output=True,
        text=True,
    ).stdout
    if not out.strip():
        raise SystemExit(f"{tool}: no reply from the MCP socket (is the app running?)")
    m = json.loads(out)
    r = m.get("result", m)
    c = r.get("content") if isinstance(r, dict) else None
    if c and isinstance(c, list) and "text" in c[0]:
        try:
            return json.loads(c[0]["text"])
        except json.JSONDecodeError:
            return {"text": c[0]["text"]}
    return m


def make_chart(png_path: str) -> list[tuple[int, int, int]]:
    rng = np.random.default_rng(7)
    patches: list[tuple[int, int, int]] = []
    skin = [(224, 172, 105), (198, 134, 66), (141, 85, 36), (255, 219, 172), (90, 60, 40), (230, 190, 170),
            (200, 150, 120), (120, 80, 60), (250, 230, 210), (160, 110, 90), (60, 40, 30), (240, 200, 180)]
    for r in range(ROWS):
        for c in range(COLS):
            if r == 0:
                v = int(round(255 * c / (COLS - 1)))
                col = (v, v, v)
            elif r == 1:
                col = tuple(int(round(255 * x)) for x in colorsys.hsv_to_rgb(c / COLS, 1.0, 1.0))
            elif r == 2:
                col = tuple(int(round(255 * x)) for x in colorsys.hsv_to_rgb(c / COLS, 0.5, 0.6))
            elif r == 3:
                col = skin[c]
            else:
                col = tuple(int(x) for x in rng.integers(0, 256, 3))
            patches.append(col)  # type: ignore[arg-type]
    img = np.zeros((H, W, 3), np.uint8)
    pw, ph = W // COLS, H // ROWS
    for idx, col in enumerate(patches):
        r, c = divmod(idx, COLS)
        img[r * ph:(r + 1) * ph, c * pw:(c + 1) * pw] = col
    Image.fromarray(img).save(png_path)
    return patches


def encode_chart(png_path: str, mp4_path: str) -> None:
    subprocess.run(
        ["ffmpeg", "-loglevel", "error", "-y", "-loop", "1", "-framerate", "30", "-i", png_path, "-t", "6",
         "-c:v", "libx264", "-preset", "veryfast", "-crf", "1", "-profile:v", "high", "-pix_fmt", "yuv420p",
         "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv", mp4_path],
        check=True,
    )


def patch_means(png: str) -> np.ndarray:
    im = np.asarray(Image.open(png).convert("RGB")).astype(np.float64)
    h, w = im.shape[:2]
    pw, ph = w / COLS, h / ROWS
    out = []
    for r in range(ROWS):
        for c in range(COLS):
            x0, x1 = int(c * pw + pw * 0.3), int(c * pw + pw * 0.7)
            y0, y1 = int(r * ph + ph * 0.3), int(r * ph + ph * 0.7)
            out.append(im[y0:y1, x0:x1].reshape(-1, 3).mean(axis=0))
    return np.array(out)


def clear_stills() -> None:
    for s in call("list_reference_stills").get("reference_stills", []):
        call("delete_reference_still", {"id": s["id"]})


def capture_pair(label: str, settle: float) -> tuple[str, str]:
    time.sleep(settle)
    clear_stills()
    live = {}
    for _ in range(4):
        live = call("capture_reference_still", {"label": f"live-{label}"})
        if live.get("path"):
            break
        time.sleep(1.5)
    exp = call("capture_export_compare_still", {"label": f"export-{label}"})
    if not live.get("path") or not exp.get("path"):
        raise SystemExit(f"{label}: capture failed: live={live} export={exp}")
    return live["path"], exp["path"]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--lut", help="a .cube file for the LUT scenarios (skipped when omitted)")
    ap.add_argument("--settle", type=float, default=5.0, help="seconds to wait after a grade change before capturing")
    ap.add_argument("--only", help="comma-separated scenario names to run")
    ap.add_argument("--workdir", help="where to put the chart files (default: a temp dir)")
    args = ap.parse_args()

    work = args.workdir or tempfile.mkdtemp(prefix="us-parity-")
    os.makedirs(work, exist_ok=True)
    png = os.path.join(work, "chart.png")
    mp4 = os.path.join(work, "chart.mp4")
    patches = make_chart(png)
    encode_chart(png, mp4)
    src = np.array(patches, dtype=float)

    call("create_project", {"title": "Colour parity chart"})
    call("import_media", {"path": mp4})
    call("add_clip", {"source_path": mp4, "track_index": 0, "timeline_start_ns": 0,
                      "source_in_ns": 0, "source_out_ns": 6_000_000_000})
    clips = call("list_clips")
    clip_id = (clips["clips"] if isinstance(clips, dict) else clips)[0]["id"]
    call("seek_playhead", {"timeline_pos_ns": 2_000_000_000})
    time.sleep(3)

    neutral = {"brightness": 0, "contrast": 1, "saturation": 1, "exposure": 0, "temperature": 6500, "tint": 0,
               "shadows": 0, "midtones": 0, "highlights": 0, "black_point": 0, "highlights_warmth": 0,
               "highlights_tint": 0, "midtones_warmth": 0, "midtones_tint": 0}
    hsl = {"enabled": True, "hue_min": 180, "hue_max": 260, "hue_softness": 10, "sat_min": 0.15,
           "sat_softness": 0.05, "saturation": 0.2, "brightness": 0.05}
    scenarios: list[tuple[str, dict, list[str], dict | None]] = [
        ("identity", {}, [], None),
        ("primaries", {"brightness": 0.15, "contrast": 1.4, "saturation": 1.5, "exposure": 0.1}, [], None),
        ("temp-tint", {"temperature": 4000, "tint": 0.3}, [], None),
        ("tonal", {"shadows": 0.3, "midtones": -0.2, "highlights": -0.3, "black_point": 0.1,
                   "highlights_warmth": 0.3, "midtones_tint": -0.2}, [], None),
        ("hsl", {}, [], hsl),
    ]
    if args.lut:
        scenarios += [
            ("lut-only", {}, [args.lut], None),
            ("all-with-lut", {"brightness": 0.15, "contrast": 1.4, "saturation": 1.5, "exposure": 0.1,
                              "temperature": 4000, "tint": 0.3, "shadows": 0.3, "midtones": -0.2,
                              "highlights": -0.3, "black_point": 0.1, "highlights_warmth": 0.3,
                              "midtones_tint": -0.2}, [args.lut], None),
        ]
    only = set(args.only.split(",")) if args.only else None

    print(f"chart: {mp4}")
    for name, color, luts, q in scenarios:
        if only and name not in only:
            continue
        call("set_clip_color", dict(clip_id=clip_id, **{**neutral, **color}))
        call("set_clip_lut", {"clip_id": clip_id, "lut_paths": luts})
        if q is None:
            call("set_clip_hsl_qualifier", {"clip_id": clip_id, "clear": True})
        else:
            call("set_clip_hsl_qualifier", dict(clip_id=clip_id, **q))
        live_png, export_png = capture_pair(name, args.settle)
        live, exp = patch_means(live_png), patch_means(export_png)
        d = exp - live
        ad = np.abs(d)
        worst = int(np.argmax(ad.max(axis=1)))
        print(f"{name:13s} mean_abs={ad.mean():5.2f} p95={np.percentile(ad, 95):5.2f} max={ad.max():5.1f} "
              f"signed_rgb={[round(float(x), 2) for x in d.mean(axis=0)]} "
              f"worst patch {worst}: src={patches[worst]} live={live[worst].round(0).astype(int).tolist()} "
              f"export={exp[worst].round(0).astype(int).tolist()}")
        if name == "identity":
            for side, arr in (("live", live), ("export", exp)):
                e = np.abs(arr - src)
                print(f"              {side} vs source: mean_abs={e.mean():.2f} max={e.max():.0f} "
                      f"signed={[round(float(x), 2) for x in (arr - src).mean(axis=0)]}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
