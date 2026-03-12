#!/usr/bin/env python3
"""Run a fast MCP parity smoke check and fail on large regressions.

This helper wraps `tools/calibrate_mcp_color_match.py` with a low sample count,
then enforces pragmatic guardrails on the output:
  1) neutral baseline RMSE stays below a broad ceiling
  2) focus sliders keep normalized (delta-from-neutral) error under a ceiling

It is intended for lightweight CI/automation sanity checks, not full calibration.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path


def run(cmd: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, text=True, capture_output=True)


def max_abs_delta_for_slider(report: dict, slider: str) -> float:
    slider_data = report.get("sliders", {}).get(slider)
    if not slider_data:
        raise KeyError(f"missing slider `{slider}` in report")
    samples = slider_data.get("samples", [])
    if not samples:
        raise KeyError(f"slider `{slider}` has no samples")
    return float(max(abs(s["delta_from_neutral_total_rmse"]) for s in samples))


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run fast MCP parity smoke check")
    parser.add_argument(
        "--media",
        default=str(Path("Sample-Media") / "calibration_chart.mp4"),
        help="Calibration source media path.",
    )
    parser.add_argument(
        "--out",
        default="/tmp/us_mcp_parity_smoke",
        help="Output directory for intermediate/report files.",
    )
    parser.add_argument(
        "--export-mode",
        choices=["mp4", "prores_mov"],
        default="prores_mov",
        help="Export mode used by the calibration harness.",
    )
    parser.add_argument(
        "--export-preset-name",
        default="mcp-parity-prores",
        help="Preset name used when export-mode=prores_mov.",
    )
    parser.add_argument(
        "--steps",
        type=int,
        default=2,
        help="Samples per slider for smoke run (2 = min/default/max).",
    )
    parser.add_argument(
        "--seek-repeats",
        type=int,
        default=2,
        help="Seek stabilization repetitions.",
    )
    parser.add_argument(
        "--max-neutral-rmse",
        type=float,
        default=30.0,
        help="Fail if neutral baseline RMSE exceeds this value.",
    )
    parser.add_argument(
        "--max-focus-delta-rmse",
        type=float,
        default=25.0,
        help="Fail if any focus slider sample exceeds this abs(delta-from-neutral) ceiling.",
    )
    parser.add_argument(
        "--focus-sliders",
        default="contrast,saturation",
        help="Comma-separated slider names checked against max-focus-delta-rmse.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    out_dir = Path(args.out).resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    report_path = out_dir / "mcp_color_calibration_report.json"

    cmd = [
        "python3",
        str(Path(__file__).with_name("calibrate_mcp_color_match.py")),
        "--media",
        str(Path(args.media).resolve()),
        "--out",
        str(out_dir),
        "--steps",
        str(args.steps),
        "--seek-repeats",
        str(args.seek_repeats),
        "--export-mode",
        args.export_mode,
        "--export-preset-name",
        args.export_preset_name,
    ]
    proc = run(cmd)
    if proc.stdout:
        print(proc.stdout, end="")
    if proc.returncode != 0:
        if proc.stderr:
            print(proc.stderr, file=sys.stderr, end="")
        return proc.returncode

    if not report_path.exists():
        print(f"smoke check failed: missing report at {report_path}", file=sys.stderr)
        return 2

    report = json.loads(report_path.read_text())
    neutral_total = float(report["neutral_baseline"]["rmse"]["total"])
    focus_sliders = [s.strip() for s in args.focus_sliders.split(",") if s.strip()]
    focus_deltas = {s: max_abs_delta_for_slider(report, s) for s in focus_sliders}

    failures: list[str] = []
    if neutral_total > args.max_neutral_rmse:
        failures.append(
            f"neutral RMSE {neutral_total:.3f} exceeds max-neutral-rmse {args.max_neutral_rmse:.3f}"
        )
    for slider, delta in focus_deltas.items():
        if delta > args.max_focus_delta_rmse:
            failures.append(
                f"{slider} max abs(delta-from-neutral) {delta:.3f} exceeds "
                f"max-focus-delta-rmse {args.max_focus_delta_rmse:.3f}"
            )

    print(
        "smoke summary:",
        json.dumps(
            {
                "report": str(report_path),
                "neutral_total_rmse": round(neutral_total, 3),
                "focus_max_abs_delta_rmse": {k: round(v, 3) for k, v in focus_deltas.items()},
                "export_mode": report.get("export_mode"),
            }
        ),
    )
    if failures:
        for failure in failures:
            print(f"FAIL: {failure}", file=sys.stderr)
        return 3

    print("PASS: parity smoke thresholds satisfied")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
