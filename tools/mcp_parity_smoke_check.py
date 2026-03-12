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
        action="append",
        help=(
            "Calibration source media path. Repeat --media to run multiple clips "
            "in one smoke pass."
        ),
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
        "--lut-path",
        default="",
        help="Optional .cube LUT path forwarded to calibration runs.",
    )
    parser.add_argument(
        "--proxy-mode",
        choices=["off", "half_res", "quarter_res"],
        default="off",
        help="Proxy mode forwarded to calibration runs.",
    )
    parser.add_argument(
        "--sliders",
        default="",
        help="Optional comma-separated slider names to pass through to calibration sweep.",
    )
    parser.add_argument(
        "--seek-repeats",
        type=int,
        default=2,
        help="Seek stabilization repetitions.",
    )
    parser.add_argument(
        "--sample-retries",
        type=int,
        default=0,
        help="Extra per-sample attempts passed through to calibrate_mcp_color_match.py.",
    )
    parser.add_argument(
        "--neutral-baseline-retries",
        type=int,
        default=2,
        help="Extra neutral-baseline attempts passed through to calibrate_mcp_color_match.py.",
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
    parser.add_argument(
        "--max-mean-neutral-rmse",
        type=float,
        default=30.0,
        help="Fail if mean neutral baseline RMSE across all media exceeds this value.",
    )
    parser.add_argument(
        "--max-mean-focus-delta-rmse",
        type=float,
        default=25.0,
        help=(
            "Fail if mean of per-media max abs(delta-from-neutral) for any focus slider "
            "exceeds this value."
        ),
    )
    return parser.parse_args()


def sanitize_media_slug(path: Path) -> str:
    stem = path.stem.strip().lower().replace(" ", "_")
    safe = "".join(ch if ch.isalnum() or ch in ("_", "-", ".") else "_" for ch in stem)
    return safe or "media"


def run_one_media(
    media_path: Path,
    out_dir: Path,
    args: argparse.Namespace,
    focus_sliders: list[str],
) -> tuple[dict, list[str]]:
    report_path = out_dir / "mcp_color_calibration_report.json"

    cmd = [
        "python3",
        str(Path(__file__).with_name("calibrate_mcp_color_match.py")),
        "--media",
        str(media_path.resolve()),
        "--out",
        str(out_dir),
        "--steps",
        str(args.steps),
        "--seek-repeats",
        str(args.seek_repeats),
        "--sample-retries",
        str(args.sample_retries),
        "--neutral-baseline-retries",
        str(args.neutral_baseline_retries),
        "--export-mode",
        args.export_mode,
        "--export-preset-name",
        args.export_preset_name,
        "--proxy-mode",
        args.proxy_mode,
    ]
    if args.sliders.strip():
        cmd.extend(["--sliders", args.sliders])
    if args.lut_path.strip():
        cmd.extend(["--lut-path", args.lut_path])
    proc = run(cmd)
    if proc.stdout:
        print(proc.stdout, end="")
    if proc.returncode != 0:
        if proc.stderr:
            print(proc.stderr, file=sys.stderr, end="")
        raise RuntimeError(f"calibration run failed for {media_path}")

    if not report_path.exists():
        raise RuntimeError(f"smoke check failed: missing report at {report_path}")

    report = json.loads(report_path.read_text())
    neutral_total = float(report["neutral_baseline"]["rmse"]["total"])
    focus_deltas = {s: max_abs_delta_for_slider(report, s) for s in focus_sliders}

    failures: list[str] = []
    if neutral_total > args.max_neutral_rmse:
        failures.append(
            f"{media_path.name}: neutral RMSE {neutral_total:.3f} exceeds "
            f"max-neutral-rmse {args.max_neutral_rmse:.3f}"
        )
    for slider, delta in focus_deltas.items():
        if delta > args.max_focus_delta_rmse:
            failures.append(
                f"{media_path.name}: {slider} max abs(delta-from-neutral) {delta:.3f} exceeds "
                f"max-focus-delta-rmse {args.max_focus_delta_rmse:.3f}"
            )

    print(
        "smoke summary:",
        json.dumps(
            {
                "media": str(media_path.resolve()),
                "report": str(report_path),
                "neutral_total_rmse": round(neutral_total, 3),
                "focus_max_abs_delta_rmse": {k: round(v, 3) for k, v in focus_deltas.items()},
                "export_mode": report.get("export_mode"),
            }
        ),
    )
    return {
        "media": str(media_path.resolve()),
        "report": str(report_path),
        "neutral_total_rmse": neutral_total,
        "focus_max_abs_delta_rmse": focus_deltas,
        "export_mode": report.get("export_mode"),
    }, failures


def main() -> int:
    args = parse_args()
    out_root = Path(args.out).resolve()
    out_root.mkdir(parents=True, exist_ok=True)
    focus_sliders = [s.strip() for s in args.focus_sliders.split(",") if s.strip()]
    media_paths = [Path(m).resolve() for m in args.media] if args.media else [
        Path("Sample-Media/calibration_chart.mp4").resolve()
    ]

    run_summaries: list[dict] = []
    failures: list[str] = []
    for idx, media_path in enumerate(media_paths):
        if not media_path.exists():
            print(f"FAIL: media not found: {media_path}", file=sys.stderr)
            return 2
        media_out = out_root / f"{idx:02d}_{sanitize_media_slug(media_path)}"
        media_out.mkdir(parents=True, exist_ok=True)
        try:
            summary, run_failures = run_one_media(media_path, media_out, args, focus_sliders)
        except RuntimeError as exc:
            print(f"FAIL: {exc}", file=sys.stderr)
            return 2
        run_summaries.append(summary)
        failures.extend(run_failures)

    mean_neutral = sum(s["neutral_total_rmse"] for s in run_summaries) / max(1, len(run_summaries))
    mean_focus = {
        slider: sum(s["focus_max_abs_delta_rmse"][slider] for s in run_summaries)
        / max(1, len(run_summaries))
        for slider in focus_sliders
    }

    aggregate_summary = {
        "runs": len(run_summaries),
        "mean_neutral_total_rmse": round(mean_neutral, 3),
        "mean_focus_max_abs_delta_rmse": {k: round(v, 3) for k, v in mean_focus.items()},
    }
    print("smoke aggregate:", json.dumps(aggregate_summary))

    if mean_neutral > args.max_mean_neutral_rmse:
        failures.append(
            f"mean neutral RMSE {mean_neutral:.3f} exceeds max-mean-neutral-rmse "
            f"{args.max_mean_neutral_rmse:.3f}"
        )
    for slider, val in mean_focus.items():
        if val > args.max_mean_focus_delta_rmse:
            failures.append(
                f"mean {slider} max abs(delta-from-neutral) {val:.3f} exceeds "
                f"max-mean-focus-delta-rmse {args.max_mean_focus_delta_rmse:.3f}"
            )

    aggregate_path = out_root / "smoke_aggregate_report.json"
    aggregate_path.write_text(
        json.dumps(
            {
                "runs": run_summaries,
                "aggregate": aggregate_summary,
                "focus_sliders": focus_sliders,
                "thresholds": {
                    "max_neutral_rmse": args.max_neutral_rmse,
                    "max_focus_delta_rmse": args.max_focus_delta_rmse,
                    "max_mean_neutral_rmse": args.max_mean_neutral_rmse,
                    "max_mean_focus_delta_rmse": args.max_mean_focus_delta_rmse,
                },
                "failures": failures,
            },
            indent=2,
        )
    )
    print(f"wrote aggregate report: {aggregate_path}")

    if failures:
        for failure in failures:
            print(f"FAIL: {failure}", file=sys.stderr)
        return 3
    print("PASS: parity smoke thresholds satisfied")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
