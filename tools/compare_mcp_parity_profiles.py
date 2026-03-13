#!/usr/bin/env python3
"""Compare parity candidates across multiple media profiles.

Each profile compares one baseline report against one candidate report and
applies the same weighted scoring + endpoint guardrails used by
`compare_mcp_parity_reports.py`. The aggregate result passes only when:
  1) every profile passes its own score/guardrails
  2) weighted aggregate score passes `--min-total-score`
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from compare_mcp_parity_reports import (
    compare_slider,
    evaluate_guardrail,
    load_report,
    parse_guardrails,
    parse_slider_filter,
    resolve_report_path,
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Compare parity candidates across multiple profiles")
    parser.add_argument(
        "--profile",
        nargs=3,
        action="append",
        metavar=("NAME", "BASELINE", "CANDIDATE"),
        required=True,
        help="Profile triplet: profile name, baseline report (or dir), candidate report (or dir).",
    )
    parser.add_argument(
        "--profile-weight",
        action="append",
        default=[],
        help="Optional profile weight as name=weight. Default weight is 1.0.",
    )
    parser.add_argument(
        "--sliders",
        default="",
        help="Optional comma-separated slider list. Default: common sliders in each profile.",
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
        help="Extra endpoint guardrail in form slider:value:max_regression.",
    )
    parser.add_argument(
        "--no-default-guardrails",
        action="store_true",
        help="Disable default risky-endpoint guardrails.",
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
        "--out",
        default="",
        help="Optional path for aggregate JSON report.",
    )
    return parser.parse_args()


def parse_profile_weights(raw_weights: list[str]) -> dict[str, float]:
    weights: dict[str, float] = {}
    for item in raw_weights:
        if "=" not in item:
            raise ValueError(f"invalid --profile-weight '{item}', expected name=weight")
        name, weight = item.split("=", 1)
        name = name.strip()
        if not name:
            raise ValueError(f"invalid --profile-weight '{item}', empty name")
        weights[name] = float(weight)
    return weights


def main() -> int:
    args = parse_args()
    profile_weights = parse_profile_weights(args.profile_weight)
    guardrails = parse_guardrails(args)

    profiles_summary: list[dict] = []
    weighted_total_score = 0.0
    all_profiles_pass = True

    for name, baseline_raw, candidate_raw in args.profile:
        baseline_path = resolve_report_path(baseline_raw)
        candidate_path = resolve_report_path(candidate_raw)
        baseline = load_report(baseline_path)
        candidate = load_report(candidate_path)
        sliders = parse_slider_filter(args.sliders, baseline, candidate)

        slider_comparison = {}
        profile_score = 0.0
        for slider in sliders:
            result = compare_slider(
                baseline["sliders"][slider],
                candidate["sliders"][slider],
                weight_mean=args.weight_mean,
                weight_max=args.weight_max,
            )
            slider_comparison[slider] = result
            profile_score += result["weighted_score"]

        guardrail_results = [evaluate_guardrail(g, baseline, candidate) for g in guardrails]
        failed_guardrails = [g for g in guardrail_results if g["status"] == "fail"]
        profile_pass = profile_score >= args.min_profile_score and not failed_guardrails
        all_profiles_pass = all_profiles_pass and profile_pass

        weight = profile_weights.get(name, 1.0)
        weighted_total_score += profile_score * weight
        profiles_summary.append(
            {
                "name": name,
                "weight": weight,
                "baseline_report": str(baseline_path),
                "candidate_report": str(candidate_path),
                "sliders": sliders,
                "score": profile_score,
                "min_profile_score": args.min_profile_score,
                "pass_score": profile_score >= args.min_profile_score,
                "guardrails": guardrail_results,
                "failed_guardrails": len(failed_guardrails),
                "pass": profile_pass,
                "slider_comparison": slider_comparison,
            }
        )

    pass_total_score = weighted_total_score >= args.min_total_score
    passed = all_profiles_pass and pass_total_score

    summary = {
        "profiles": profiles_summary,
        "weighted_total_score": weighted_total_score,
        "min_total_score": args.min_total_score,
        "pass_total_score": pass_total_score,
        "pass_all_profiles": all_profiles_pass,
        "pass": passed,
    }

    print(
        "multi-profile summary:",
        json.dumps(
            {
                "profiles": len(profiles_summary),
                "pass": passed,
                "weighted_total_score": round(weighted_total_score, 3),
                "pass_all_profiles": all_profiles_pass,
            }
        ),
    )
    for profile in profiles_summary:
        print(
            f"{profile['name']}: score={profile['score']:.3f} weight={profile['weight']:.3f} "
            f"failed_guardrails={profile['failed_guardrails']} pass={profile['pass']}"
        )

    if args.out.strip():
        out_path = Path(args.out).resolve()
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(json.dumps(summary, indent=2))
        print(f"wrote aggregate profile comparison: {out_path}")

    if not passed:
        print(
            "FAIL: candidate parity did not satisfy multi-profile score/guardrails",
            file=sys.stderr,
        )
        return 1
    print("PASS: candidate parity satisfies multi-profile score/guardrails")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(f"error: {exc}", file=sys.stderr)
        raise SystemExit(1)
