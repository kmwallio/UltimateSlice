#!/usr/bin/env python3
"""Validate an UltimateSlice-exported AAF by reading it back with pyaaf2.

UltimateSlice writes AAF natively in Rust (`src/aaf/`). This dev/CI helper opens
the result with the canonical pyaaf2 library and prints the Mob / Slot /
SourceClip tree so structural regressions are caught without a DAW.

    pip install pyaaf2
    python3 tools/validate_aaf.py path/to/export.aaf

Exit code 0 if pyaaf2 opens and walks the file; non-zero otherwise. Pass
--require to assert minimum counts, e.g. `--require comps=1 mobs=2`.
"""
import argparse
import sys


def main() -> int:
    ap = argparse.ArgumentParser(description="Validate an AAF with pyaaf2.")
    ap.add_argument("aaf", help="path to the .aaf file")
    ap.add_argument(
        "--require",
        nargs="*",
        default=[],
        metavar="KEY=N",
        help="minimum counts to assert: mobs=, comps=, sources=",
    )
    args = ap.parse_args()

    try:
        import aaf2
    except ImportError:
        print("pyaaf2 not installed; run: pip install pyaaf2", file=sys.stderr)
        return 2

    reqs = dict(kv.split("=") for kv in args.require)
    counts = {"mobs": 0, "comps": 0, "sources": 0}

    with aaf2.open(args.aaf, "r") as f:
        for mob in f.content.mobs:
            counts["mobs"] += 1
            kind = type(mob).__name__
            if kind == "CompositionMob":
                counts["comps"] += 1
            elif kind == "SourceMob":
                counts["sources"] += 1
            print(f"\nMOB [{kind}] name={mob.name!r}")
            for slot in mob.slots:
                seg = slot.segment
                print(
                    f"  slot {slot.slot_id} kind={slot.media_kind} "
                    f"editrate={slot.edit_rate} seg={type(seg).__name__} "
                    f"len={getattr(seg, 'length', None)}"
                )
                if hasattr(seg, "components"):
                    for c in seg.components:
                        extra = ""
                        if type(c).__name__ == "SourceClip":
                            extra = f" -> mob={c.mob_id} slot={c.slot_id} start={c.start}"
                        print(f"      {type(c).__name__} len={getattr(c, 'length', None)}{extra}")
            desc = getattr(mob, "descriptor", None)
            if desc is not None:
                locs = (
                    [loc["URLString"].value for loc in desc["Locator"]]
                    if "Locator" in desc
                    else []
                )
                print(f"  descriptor={type(desc).__name__} locators={locs}")

    ok = True
    for key, want in reqs.items():
        got = counts.get(key, 0)
        if got < int(want):
            print(f"FAIL: {key} {got} < required {want}", file=sys.stderr)
            ok = False
    print(f"\n{'OK' if ok else 'FAIL'}: {counts}")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
