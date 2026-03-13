#!/usr/bin/env python3
"""Search export parity gains using the retune cycle wrapper.

This runs `run_mcp_parity_retune_cycle.py` for each candidate gain and picks
the best trial by:
  1) pass status (preferred)
  2) weighted multi-profile score
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from itertools import product
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


def parse_gains(raw: str, label: str) -> list[float]:
    gains = []
    for part in raw.split(","):
        part = part.strip()
        if not part:
            continue
        gains.append(float(part))
    if not gains:
        raise ValueError(f"{label} must include at least one value")
    return gains


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Optimize export parity gains")
    parser.add_argument(
        "--profile-media",
        action="append",
        required=True,
        metavar="NAME=MEDIA_PATH",
        help="Profile media mapping (repeatable).",
    )
    parser.add_argument(
        "--baseline-report",
        action="append",
        required=True,
        metavar="NAME=REPORT_OR_DIR",
        help="Baseline report mapping (repeatable).",
    )
    parser.add_argument(
        "--profile-weight",
        action="append",
        default=[],
        metavar="NAME=WEIGHT",
        help="Optional profile weights forwarded to retune cycle wrapper.",
    )
    parser.add_argument(
        "--gains",
        default="0.94,0.95,0.96,0.97,0.98,1.00",
        help="Legacy cool-side gain list (used for both far/near when specific lists are unset).",
    )
    parser.add_argument(
        "--cool-far-gains",
        default="",
        help="Optional comma-separated gains for far-cool endpoint (~2000K).",
    )
    parser.add_argument(
        "--cool-near-gains",
        default="",
        help="Optional comma-separated gains for near-cool endpoint (~5000K).",
    )
    parser.add_argument(
        "--shadows-pos-gains",
        default="1.00",
        help="Comma-separated gains for positive shadows export tonal inputs.",
    )
    parser.add_argument(
        "--midtones-neg-gains",
        default="1.00",
        help="Comma-separated gains for negative midtones export tonal inputs.",
    )
    parser.add_argument(
        "--highlights-neg-gains",
        default="1.00",
        help="Comma-separated gains for negative highlights export tonal inputs.",
    )
    parser.add_argument("--out", required=True, help="Output directory for optimizer artifacts.")
    parser.add_argument(
        "--sliders",
        default="temperature,shadows,midtones,highlights",
        help="Sliders forwarded to retune cycle wrapper.",
    )
    parser.add_argument("--steps", type=int, default=3, help="Calibration steps.")
    parser.add_argument("--seek-repeats", type=int, default=2, help="Seek repeats.")
    parser.add_argument("--sample-retries", type=int, default=1, help="Sample retries.")
    parser.add_argument(
        "--neutral-baseline-retries",
        type=int,
        default=2,
        help="Neutral baseline retries.",
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
    parser.add_argument("--lut-path", default="", help="Optional LUT path.")
    parser.add_argument(
        "--guardrail",
        action="append",
        default=[],
        help="Extra guardrails forwarded to retune cycle wrapper.",
    )
    parser.add_argument(
        "--temperature-max-regression",
        type=float,
        default=0.5,
        help="Temperature endpoint regression threshold.",
    )
    parser.add_argument(
        "--no-temperature-guardrails",
        action="store_true",
        help="Disable automatic temperature endpoint guardrails.",
    )
    parser.add_argument(
        "--min-profile-score",
        type=float,
        default=0.0,
        help="Minimum profile score for pass.",
    )
    parser.add_argument(
        "--min-total-score",
        type=float,
        default=0.0,
        help="Minimum aggregate score for pass.",
    )
    parser.add_argument(
        "--python",
        default=sys.executable or "python3",
        help="Python interpreter used for helper scripts.",
    )
    return parser.parse_args()


def run(cmd: list[str], env: dict[str, str]) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, text=True, capture_output=True, env=env)


def print_proc(proc: subprocess.CompletedProcess) -> None:
    if proc.stdout:
        print(proc.stdout, end="")
    if proc.stderr:
        print(proc.stderr, file=sys.stderr, end="")


def main() -> int:
    args = parse_args()
    _profile_media = parse_name_value_pairs(args.profile_media, "--profile-media")
    _baseline_reports = parse_name_value_pairs(args.baseline_report, "--baseline-report")
    legacy_gains = parse_gains(args.gains, "--gains")
    cool_far_gains = (
        parse_gains(args.cool_far_gains, "--cool-far-gains")
        if args.cool_far_gains.strip()
        else legacy_gains
    )
    cool_near_gains = (
        parse_gains(args.cool_near_gains, "--cool-near-gains")
        if args.cool_near_gains.strip()
        else legacy_gains
    )
    shadows_pos_gains = parse_gains(args.shadows_pos_gains, "--shadows-pos-gains")
    midtones_neg_gains = parse_gains(args.midtones_neg_gains, "--midtones-neg-gains")
    highlights_neg_gains = parse_gains(args.highlights_neg_gains, "--highlights-neg-gains")

    out_root = Path(args.out).resolve()
    out_root.mkdir(parents=True, exist_ok=True)
    wrapper = Path(__file__).with_name("run_mcp_parity_retune_cycle.py")

    trials: list[dict] = []
    best: dict | None = None

    for (
        cool_far_gain,
        cool_near_gain,
        shadows_pos_gain,
        midtones_neg_gain,
        highlights_neg_gain,
    ) in product(
        cool_far_gains,
        cool_near_gains,
        shadows_pos_gains,
        midtones_neg_gains,
        highlights_neg_gains,
    ):
        tag = (
            f"cfar_{cool_far_gain:.3f}__cnear_{cool_near_gain:.3f}"
            f"__shp_{shadows_pos_gain:.3f}"
            f"__midn_{midtones_neg_gain:.3f}__hin_{highlights_neg_gain:.3f}"
        )
        trial_out = out_root / tag
        cmd = [
            args.python,
            str(wrapper),
            "--out",
            str(trial_out),
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
            "--min-profile-score",
            str(args.min_profile_score),
            "--min-total-score",
            str(args.min_total_score),
        ]
        for entry in args.profile_media:
            cmd.extend(["--profile-media", entry])
        for entry in args.baseline_report:
            cmd.extend(["--baseline-report", entry])
        for entry in args.profile_weight:
            cmd.extend(["--profile-weight", entry])
        for entry in args.guardrail:
            cmd.extend(["--guardrail", entry])
        if args.lut_path.strip():
            cmd.extend(["--lut-path", args.lut_path])
        if args.no_temperature_guardrails:
            cmd.append("--no-temperature-guardrails")
        else:
            cmd.extend(["--temperature-max-regression", str(args.temperature_max_regression)])

        env = os.environ.copy()
        env["US_EXPORT_COOL_TEMP_GAIN"] = f"{cool_near_gain:.6f}"
        env["US_EXPORT_COOL_TEMP_GAIN_FAR"] = f"{cool_far_gain:.6f}"
        env["US_EXPORT_COOL_TEMP_GAIN_NEAR"] = f"{cool_near_gain:.6f}"
        env["US_EXPORT_SHADOWS_POS_GAIN"] = f"{shadows_pos_gain:.6f}"
        env["US_EXPORT_MIDTONES_NEG_GAIN"] = f"{midtones_neg_gain:.6f}"
        env["US_EXPORT_HIGHLIGHTS_NEG_GAIN"] = f"{highlights_neg_gain:.6f}"

        print(f"\n[temp-opt] trial {tag}")
        proc = run(cmd, env=env)
        print_proc(proc)

        summary_path = trial_out / "retune_cycle_summary.json"
        multi_path = trial_out / "compare_multi.json"
        summary = json.loads(summary_path.read_text()) if summary_path.exists() else {}
        multi = json.loads(multi_path.read_text()) if multi_path.exists() else {}
        pass_flag = bool(summary.get("pass", False))
        score = float(multi.get("weighted_total_score", float("-inf")))
        trial = {
            "gain": cool_near_gain,
            "cool_far_gain": cool_far_gain,
            "cool_near_gain": cool_near_gain,
            "shadows_pos_gain": shadows_pos_gain,
            "midtones_neg_gain": midtones_neg_gain,
            "highlights_neg_gain": highlights_neg_gain,
            "trial_out": str(trial_out),
            "wrapper_exit_code": proc.returncode,
            "pass": pass_flag,
            "weighted_total_score": score,
        }
        trials.append(trial)

        if best is None:
            best = trial
        else:
            best_key = (1 if best["pass"] else 0, best["weighted_total_score"])
            trial_key = (1 if trial["pass"] else 0, trial["weighted_total_score"])
            if trial_key > best_key:
                best = trial

    result = {"trials": trials, "best": best}
    result_path = out_root / "parity_gain_optimization_summary.json"
    result_path.write_text(json.dumps(result, indent=2))
    print(f"\n[temp-opt] wrote optimization summary: {result_path}")
    if best is not None:
        print(
            f"[temp-opt] best cool_far={best['cool_far_gain']:.3f} "
            f"cool_near={best['cool_near_gain']:.3f} "
            f"shadows_pos={best['shadows_pos_gain']:.3f} "
            f"midtones_neg={best['midtones_neg_gain']:.3f} "
            f"highlights_neg={best['highlights_neg_gain']:.3f} "
            f"pass={best['pass']} "
            f"weighted_total_score={best['weighted_total_score']:.3f}"
        )
    if best is None or not best["pass"]:
        print("[temp-opt] no passing gain found in this sweep", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(f"error: {exc}", file=sys.stderr)
        raise SystemExit(1)
