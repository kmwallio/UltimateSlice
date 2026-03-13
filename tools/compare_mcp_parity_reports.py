#!/usr/bin/env python3
"""Compare baseline/candidate MCP parity reports with guardrails.

This script is for retune loops where you want a quick, objective answer:
- Did weighted parity improve overall?
- Did risky endpoints regress (e.g. highlights -1)?
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass
from pathlib import Path


DEFAULT_GUARDRAILS = [
    ("shadows", 1.0, 0.5),
    ("midtones", -1.0, 0.5),
    ("highlights", -1.0, 0.5),
]


@dataclass(frozen=True)
class Guardrail:
    slider: str
    value: float
    max_regression: float


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Compare two MCP color parity reports")
    parser.add_argument(
        "--baseline",
        required=True,
        help="Baseline report path (or directory containing mcp_color_calibration_report.json).",
    )
    parser.add_argument(
        "--candidate",
        required=True,
        help="Candidate report path (or directory containing mcp_color_calibration_report.json).",
    )
    parser.add_argument(
        "--sliders",
        default="",
        help="Optional comma-separated slider list. Default: common sliders in both reports.",
    )
    parser.add_argument(
        "--weight-mean",
        type=float,
        default=1.0,
        help="Weight for mean abs delta-from-neutral improvement.",
    )
    parser.add_argument(
        "--weight-max",
        type=float,
        default=1.5,
        help="Weight for max abs delta-from-neutral improvement.",
    )
    parser.add_argument(
        "--guardrail",
        action="append",
        default=[],
        help=(
            "Extra endpoint guardrail in form slider:value:max_regression. "
            "Repeatable; e.g. --guardrail highlights:-1:0.25"
        ),
    )
    parser.add_argument(
        "--no-default-guardrails",
        action="store_true",
        help="Disable default risky-endpoint guardrails.",
    )
    parser.add_argument(
        "--min-score",
        type=float,
        default=0.0,
        help="Minimum total weighted score required to pass.",
    )
    parser.add_argument(
        "--out",
        default="",
        help="Optional path for JSON comparison output.",
    )
    return parser.parse_args()


def resolve_report_path(raw: str) -> Path:
    p = Path(raw).resolve()
    if p.is_dir():
        p = p / "mcp_color_calibration_report.json"
    return p


def load_report(path: Path) -> dict:
    if not path.exists():
        raise FileNotFoundError(f"report not found: {path}")
    return json.loads(path.read_text())


def parse_slider_filter(raw: str, baseline: dict, candidate: dict) -> list[str]:
    base = set(baseline.get("sliders", {}).keys())
    cand = set(candidate.get("sliders", {}).keys())
    common = sorted(base & cand)
    if not raw.strip():
        if not common:
            raise ValueError("no common sliders found between reports")
        return common
    requested = [s.strip() for s in raw.split(",") if s.strip()]
    unknown = [s for s in requested if s not in common]
    if unknown:
        raise ValueError(
            f"unknown slider(s) for this report pair: {', '.join(unknown)}; common={', '.join(common)}"
        )
    return list(dict.fromkeys(requested))


def parse_guardrails(args: argparse.Namespace) -> list[Guardrail]:
    guardrails: list[Guardrail] = []
    if not args.no_default_guardrails:
        guardrails.extend(Guardrail(s, v, r) for s, v, r in DEFAULT_GUARDRAILS)
    for item in args.guardrail:
        parts = item.split(":")
        if len(parts) != 3:
            raise ValueError(
                f"invalid --guardrail '{item}', expected slider:value:max_regression"
            )
        slider, value_raw, reg_raw = parts
        guardrails.append(
            Guardrail(
                slider=slider.strip(),
                value=float(value_raw),
                max_regression=float(reg_raw),
            )
        )
    return guardrails


def sample_for_value(samples: list[dict], target: float) -> dict | None:
    if not samples:
        return None
    return min(samples, key=lambda s: abs(float(s["value"]) - target))


def compare_slider(baseline_slider: dict, candidate_slider: dict, weight_mean: float, weight_max: float) -> dict:
    b_summary = baseline_slider["summary"]
    c_summary = candidate_slider["summary"]
    b_mean = float(b_summary["mean_abs_delta_from_neutral_total_rmse"])
    c_mean = float(c_summary["mean_abs_delta_from_neutral_total_rmse"])
    b_max = float(b_summary["max_abs_delta_from_neutral_total_rmse"])
    c_max = float(c_summary["max_abs_delta_from_neutral_total_rmse"])
    mean_improvement = b_mean - c_mean
    max_improvement = b_max - c_max
    weighted_score = mean_improvement * weight_mean + max_improvement * weight_max
    return {
        "baseline_mean_abs_delta": b_mean,
        "candidate_mean_abs_delta": c_mean,
        "baseline_max_abs_delta": b_max,
        "candidate_max_abs_delta": c_max,
        "mean_improvement": mean_improvement,
        "max_improvement": max_improvement,
        "weighted_score": weighted_score,
    }


def evaluate_guardrail(guardrail: Guardrail, baseline: dict, candidate: dict) -> dict:
    b_slider = baseline.get("sliders", {}).get(guardrail.slider)
    c_slider = candidate.get("sliders", {}).get(guardrail.slider)
    if not b_slider or not c_slider:
        return {
            "slider": guardrail.slider,
            "value": guardrail.value,
            "max_regression": guardrail.max_regression,
            "status": "skipped",
            "reason": "slider missing in one or both reports",
        }
    b_sample = sample_for_value(b_slider.get("samples", []), guardrail.value)
    c_sample = sample_for_value(c_slider.get("samples", []), guardrail.value)
    if b_sample is None or c_sample is None:
        return {
            "slider": guardrail.slider,
            "value": guardrail.value,
            "max_regression": guardrail.max_regression,
            "status": "skipped",
            "reason": "sample missing in one or both reports",
        }
    b_abs = abs(float(b_sample["delta_from_neutral_total_rmse"]))
    c_abs = abs(float(c_sample["delta_from_neutral_total_rmse"]))
    regression = c_abs - b_abs
    passed = regression <= guardrail.max_regression
    return {
        "slider": guardrail.slider,
        "value": guardrail.value,
        "max_regression": guardrail.max_regression,
        "status": "pass" if passed else "fail",
        "baseline_abs_delta": b_abs,
        "candidate_abs_delta": c_abs,
        "regression": regression,
    }


def main() -> int:
    args = parse_args()
    baseline_path = resolve_report_path(args.baseline)
    candidate_path = resolve_report_path(args.candidate)
    baseline = load_report(baseline_path)
    candidate = load_report(candidate_path)

    sliders = parse_slider_filter(args.sliders, baseline, candidate)
    guardrails = parse_guardrails(args)

    slider_comparison = {}
    total_score = 0.0
    for slider in sliders:
        result = compare_slider(
            baseline["sliders"][slider],
            candidate["sliders"][slider],
            weight_mean=args.weight_mean,
            weight_max=args.weight_max,
        )
        slider_comparison[slider] = result
        total_score += result["weighted_score"]

    guardrail_results = [
        evaluate_guardrail(g, baseline, candidate)
        for g in guardrails
    ]
    failed_guardrails = [g for g in guardrail_results if g["status"] == "fail"]
    pass_score = total_score >= args.min_score
    passed = pass_score and not failed_guardrails

    summary = {
        "baseline_report": str(baseline_path),
        "candidate_report": str(candidate_path),
        "weights": {"mean_abs_delta": args.weight_mean, "max_abs_delta": args.weight_max},
        "sliders": sliders,
        "slider_comparison": slider_comparison,
        "total_weighted_score": total_score,
        "min_score": args.min_score,
        "pass_score": pass_score,
        "guardrails": guardrail_results,
        "failed_guardrails": len(failed_guardrails),
        "pass": passed,
    }

    print(
        "comparison summary:",
        json.dumps(
            {
                "pass": passed,
                "total_weighted_score": round(total_score, 3),
                "failed_guardrails": len(failed_guardrails),
                "sliders": sliders,
            }
        ),
    )
    for slider in sliders:
        r = slider_comparison[slider]
        print(
            f"{slider:14s} mean {r['baseline_mean_abs_delta']:.3f}->{r['candidate_mean_abs_delta']:.3f} "
            f"max {r['baseline_max_abs_delta']:.3f}->{r['candidate_max_abs_delta']:.3f} "
            f"score={r['weighted_score']:.3f}"
        )
    for g in guardrail_results:
        if g["status"] in ("pass", "fail"):
            print(
                f"guardrail {g['slider']}@{g['value']:+.3f}: "
                f"baseline={g['baseline_abs_delta']:.3f} candidate={g['candidate_abs_delta']:.3f} "
                f"regression={g['regression']:.3f} max={g['max_regression']:.3f} [{g['status']}]"
            )
        else:
            print(
                f"guardrail {g['slider']}@{g['value']:+.3f}: skipped ({g['reason']})"
            )

    if args.out.strip():
        out_path = Path(args.out).resolve()
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(json.dumps(summary, indent=2))
        print(f"wrote comparison report: {out_path}")

    if not passed:
        print("FAIL: candidate parity is not accepted by current score/guardrail policy", file=sys.stderr)
        return 1
    print("PASS: candidate parity beats baseline under current score/guardrail policy")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(f"error: {exc}", file=sys.stderr)
        raise SystemExit(1)
