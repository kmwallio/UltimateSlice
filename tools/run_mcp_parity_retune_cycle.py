#!/usr/bin/env python3
"""Run one full parity retune cycle: sweep + compare + aggregate gate.

This wrapper is intended for fast iteration:
1) run focused calibration sweeps per profile (chart/natural/LUT, etc)
2) run single-profile baseline/candidate comparators
3) run multi-profile aggregate comparator with weights
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path


def parse_name_value_pairs(values: list[str], label: str) -> dict[str, str]:
    parsed: dict[str, str] = {}
    for item in values:
        if "=" not in item:
            raise ValueError(f"invalid {label} '{item}', expected name=value")
        name, value = item.split("=", 1)
        name = name.strip()
        value = value.strip()
        if not name or not value:
            raise ValueError(f"invalid {label} '{item}', empty name or value")
        parsed[name] = value
    return parsed


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run a full MCP parity retune cycle")
    parser.add_argument(
        "--profile-media",
        action="append",
        required=True,
        metavar="NAME=MEDIA_PATH",
        help="Profile media mapping (repeatable), e.g. chart=Sample-Media/calibration_chart.mp4",
    )
    parser.add_argument(
        "--baseline-report",
        action="append",
        required=True,
        metavar="NAME=REPORT_OR_DIR",
        help="Baseline report mapping (repeatable), e.g. chart=/tmp/us_mcp_phase2_baseline_chart_now",
    )
    parser.add_argument(
        "--profile-weight",
        action="append",
        default=[],
        metavar="NAME=WEIGHT",
        help="Optional profile weight mapping for aggregate comparator.",
    )
    parser.add_argument("--out", required=True, help="Output directory for cycle artifacts.")
    parser.add_argument(
        "--sliders",
        default="temperature,shadows,midtones,highlights",
        help="Focused slider list forwarded to calibration/comparators.",
    )
    parser.add_argument("--steps", type=int, default=3, help="Calibration steps per slider.")
    parser.add_argument("--seek-repeats", type=int, default=2, help="Calibration seek repeats.")
    parser.add_argument("--sample-retries", type=int, default=1, help="Calibration sample retries.")
    parser.add_argument(
        "--neutral-baseline-retries",
        type=int,
        default=2,
        help="Calibration neutral baseline retries.",
    )
    parser.add_argument(
        "--export-mode",
        choices=["mp4", "prores_mov"],
        default="prores_mov",
        help="Calibration export mode.",
    )
    parser.add_argument(
        "--export-preset-name",
        default="mcp-parity-prores",
        help="Preset name when export-mode=prores_mov.",
    )
    parser.add_argument(
        "--proxy-mode",
        choices=["off", "half_res", "quarter_res"],
        default="off",
        help="Proxy mode forwarded to calibration.",
    )
    parser.add_argument("--lut-path", default="", help="Optional LUT path forwarded to calibration.")
    parser.add_argument(
        "--guardrail",
        action="append",
        default=[],
        help="Extra comparator guardrail in form slider:value:max_regression.",
    )
    parser.add_argument(
        "--temperature-max-regression",
        type=float,
        default=0.5,
        help="Allowed regression for auto-added temperature endpoint guardrails.",
    )
    parser.add_argument(
        "--no-temperature-guardrails",
        action="store_true",
        help="Disable automatic temperature endpoint guardrails (2000 and 10000).",
    )
    parser.add_argument(
        "--min-profile-score",
        type=float,
        default=0.0,
        help="Minimum score each profile must meet.",
    )
    parser.add_argument(
        "--min-total-score",
        type=float,
        default=0.0,
        help="Minimum weighted aggregate score across profiles.",
    )
    parser.add_argument(
        "--python",
        default=sys.executable or "python3",
        help="Python interpreter used to invoke helper scripts.",
    )
    return parser.parse_args()


def run(cmd: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, text=True, capture_output=True)


def print_proc(proc: subprocess.CompletedProcess) -> None:
    if proc.stdout:
        print(proc.stdout, end="")
    if proc.stderr:
        print(proc.stderr, file=sys.stderr, end="")


def resolve_report_path(raw: str) -> Path:
    p = Path(raw).resolve()
    if p.is_dir():
        p = p / "mcp_color_calibration_report.json"
    return p


def main() -> int:
    args = parse_args()
    out_root = Path(args.out).resolve()
    out_root.mkdir(parents=True, exist_ok=True)

    profile_media = parse_name_value_pairs(args.profile_media, "--profile-media")
    baseline_reports = parse_name_value_pairs(args.baseline_report, "--baseline-report")
    profile_weights = parse_name_value_pairs(args.profile_weight, "--profile-weight")

    profile_names = list(profile_media.keys())
    missing_baselines = [name for name in profile_names if name not in baseline_reports]
    if missing_baselines:
        raise ValueError(f"missing baseline-report for profile(s): {', '.join(missing_baselines)}")

    guardrails = list(args.guardrail)
    if not args.no_temperature_guardrails:
        guardrails.extend(
            [
                f"temperature:2000:{args.temperature_max_regression}",
                f"temperature:10000:{args.temperature_max_regression}",
            ]
        )

    calibrate_script = Path(__file__).with_name("calibrate_mcp_color_match.py")
    compare_one_script = Path(__file__).with_name("compare_mcp_parity_reports.py")
    compare_multi_script = Path(__file__).with_name("compare_mcp_parity_profiles.py")

    profile_results: list[dict] = []
    profile_triplets: list[tuple[str, Path, Path]] = []
    all_ok = True

    for name in profile_names:
        media_path = Path(profile_media[name]).resolve()
        baseline_path = resolve_report_path(baseline_reports[name])
        if not media_path.exists():
            raise FileNotFoundError(f"profile '{name}' media not found: {media_path}")
        if not baseline_path.exists():
            raise FileNotFoundError(f"profile '{name}' baseline report not found: {baseline_path}")

        profile_out = out_root / name
        profile_out.mkdir(parents=True, exist_ok=True)
        candidate_report = profile_out / "mcp_color_calibration_report.json"
        compare_out = out_root / f"compare_{name}.json"

        calibrate_cmd = [
            args.python,
            str(calibrate_script),
            "--media",
            str(media_path),
            "--out",
            str(profile_out),
            "--sliders",
            args.sliders,
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
        if args.lut_path.strip():
            calibrate_cmd.extend(["--lut-path", args.lut_path])

        print(f"\n[retune-cycle] calibrating profile={name}")
        calibrate_proc = run(calibrate_cmd)
        print_proc(calibrate_proc)
        if calibrate_proc.returncode != 0 or not candidate_report.exists():
            print(f"[retune-cycle] FAIL: calibration failed for profile={name}", file=sys.stderr)
            all_ok = False
            profile_results.append(
                {"name": name, "stage": "calibrate", "ok": False, "candidate_report": str(candidate_report)}
            )
            continue

        compare_cmd = [
            args.python,
            str(compare_one_script),
            "--baseline",
            str(baseline_path),
            "--candidate",
            str(candidate_report),
            "--sliders",
            args.sliders,
            "--min-score",
            str(args.min_profile_score),
            "--out",
            str(compare_out),
        ]
        for guardrail in guardrails:
            compare_cmd.extend(["--guardrail", guardrail])

        print(f"[retune-cycle] comparing profile={name}")
        compare_proc = run(compare_cmd)
        print_proc(compare_proc)
        profile_ok = compare_proc.returncode == 0
        all_ok = all_ok and profile_ok
        profile_results.append(
            {
                "name": name,
                "stage": "compare",
                "ok": profile_ok,
                "baseline_report": str(baseline_path),
                "candidate_report": str(candidate_report),
                "compare_report": str(compare_out),
            }
        )
        profile_triplets.append((name, baseline_path, candidate_report))

    multi_out = out_root / "compare_multi.json"
    multi_ok = False
    if profile_triplets:
        multi_cmd = [args.python, str(compare_multi_script)]
        for name, baseline_path, candidate_report in profile_triplets:
            multi_cmd.extend(["--profile", name, str(baseline_path), str(candidate_report)])
        for mapping in args.profile_weight:
            multi_cmd.extend(["--profile-weight", mapping])
        for guardrail in guardrails:
            multi_cmd.extend(["--guardrail", guardrail])
        multi_cmd.extend(
            [
                "--sliders",
                args.sliders,
                "--min-profile-score",
                str(args.min_profile_score),
                "--min-total-score",
                str(args.min_total_score),
                "--out",
                str(multi_out),
            ]
        )
        print("\n[retune-cycle] running multi-profile gate")
        multi_proc = run(multi_cmd)
        print_proc(multi_proc)
        multi_ok = multi_proc.returncode == 0
        all_ok = all_ok and multi_ok

    summary = {
        "profiles": profile_results,
        "profile_weights": profile_weights,
        "guardrails": guardrails,
        "multi_report": str(multi_out),
        "multi_ok": multi_ok,
        "pass": all_ok,
    }
    summary_path = out_root / "retune_cycle_summary.json"
    summary_path.write_text(json.dumps(summary, indent=2))
    print(f"\n[retune-cycle] wrote summary: {summary_path}")

    if not all_ok:
        print("[retune-cycle] FAIL: one or more profile gates failed", file=sys.stderr)
        return 1
    print("[retune-cycle] PASS: all profile gates passed")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(f"error: {exc}", file=sys.stderr)
        raise SystemExit(1)
