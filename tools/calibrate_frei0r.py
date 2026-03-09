#!/usr/bin/env python3
"""
Calibrate frei0r preview elements against FFmpeg export filters.

Calibrates:
  - frei0r coloradj_RGB (temperature, tint) against FFmpeg colortemperature/colorbalance
  - frei0r 3-point-color-balance (shadows, midtones, highlights) against FFmpeg colorbalance

For each slider value:
  1. Render FFmpeg export reference (ground truth)
  2. Simulate frei0r preview element in numpy
  3. Optimize frei0r parameters to minimise RMSE vs export
  4. Fit polynomial mapping from slider values to optimal frei0r params

Usage:
    python3 tools/calibrate_frei0r.py [--frame path.png] [--out /tmp/us_frei0r_calib]

Requirements: numpy, scipy, pillow
"""

import argparse, json, os, subprocess, sys
from pathlib import Path
import numpy as np
from scipy.optimize import minimize

from PIL import Image

# ---------------------------------------------------------------------------
# Slider definitions
# ---------------------------------------------------------------------------
SLIDERS = {
    "temperature": (2000.0, 11000.0, 6500.0, 21),
    "tint":        (-1.0,   1.0,     0.0,    21),
    "shadows":     (-1.0,   1.0,     0.0,    21),
    "midtones":    (-1.0,   1.0,     0.0,    21),
    "highlights":  (-1.0,   1.0,     0.0,    21),
}

# ---------------------------------------------------------------------------
# numpy simulation of frei0r coloradj_RGB (multiply mode)
# ---------------------------------------------------------------------------
def apply_coloradj_rgb(img_rgb, r_param, g_param, b_param):
    """Simulate frei0r coloradj_RGB in multiply mode.
    gain = param * 2.0 (param 0.5 = gain 1.0, neutral)."""
    out = img_rgb.copy()
    out[..., 0] *= r_param * 2.0
    out[..., 1] *= g_param * 2.0
    out[..., 2] *= b_param * 2.0
    return np.clip(out, 0.0, 1.0)


# ---------------------------------------------------------------------------
# numpy simulation of frei0r 3-point-color-balance (parabola transfer)
# ---------------------------------------------------------------------------
def solve_parabola(x1, y1, x2, y2, x3, y3):
    """Fit y = ax^2 + bx + c through three points."""
    A = np.array([[x1*x1, x1, 1], [x2*x2, x2, 1], [x3*x3, x3, 1]])
    b = np.array([y1, y2, y3])
    try:
        coeffs = np.linalg.solve(A, b)
    except np.linalg.LinAlgError:
        coeffs = np.array([0.0, 1.0, 0.0])
    return coeffs


def apply_3point(img_rgb, black, gray, white):
    """Simulate frei0r 3-point-color-balance (all channels equal)."""
    coeffs = solve_parabola(black, 0.0, gray, 0.5, white, 1.0)
    out = img_rgb.copy()
    for ch in range(3):
        x = out[..., ch]
        y = coeffs[0] * x * x + coeffs[1] * x + coeffs[2]
        out[..., ch] = np.clip(y, 0, 1)
    return out


# ---------------------------------------------------------------------------
# Tanner Helland: Kelvin -> RGB gains (current Rust implementation)
# ---------------------------------------------------------------------------
def kelvin_to_rgb(temp_k):
    t = temp_k / 100.0
    r = 255.0 if t <= 66 else max(0, min(255, 329.698727446 * (t - 60) ** -0.1332047592))
    if t <= 66:
        g = max(0, min(255, 99.4708025861 * np.log(t) - 161.1195681661))
    else:
        g = max(0, min(255, 288.1221695283 * (t - 60) ** -0.0755148492))
    b = 255.0 if t >= 66 else (0.0 if t <= 19 else max(0, min(255, 138.5177312231 * np.log(t - 10) - 305.0447927307)))
    return r, g, b


def current_coloradj_params(temperature, tint):
    """Current Rust mapping: compute_coloradj_params."""
    ref_r, ref_g, ref_b = kelvin_to_rgb(6500.0)
    tgt_r, tgt_g, tgt_b = kelvin_to_rgb(max(1000, min(15000, temperature)))
    gain_r = tgt_r / ref_r
    gain_g = tgt_g / ref_g
    gain_b = tgt_b / ref_b
    t = max(-1, min(1, tint))
    gain_r += t * 0.25
    gain_g -= t * 0.50
    gain_b += t * 0.25
    return {
        "r": max(0, min(1, gain_r / 2.0)),
        "g": max(0, min(1, gain_g / 2.0)),
        "b": max(0, min(1, gain_b / 2.0)),
    }


def current_3point_params(shadows, midtones, highlights):
    """Current Rust mapping: compute_3point_params."""
    sh = max(-1, min(1, shadows))
    mid = max(-1, min(1, midtones))
    hi = max(-1, min(1, highlights))
    black = max(0, min(0.8, max(0, sh) * 0.5))
    white = max(0.2, min(1.0, 1.0 + min(0, hi) * 0.5))
    gray = max(0.05, min(0.95,
        0.5 + mid * 0.25 + min(0, sh) * 0.15 + max(0, hi) * 0.15))
    return {"black": black, "gray": gray, "white": white}


# ---------------------------------------------------------------------------
# FFmpeg reference
# ---------------------------------------------------------------------------
def ffmpeg_export_filter(slider, value):
    if slider == "temperature":
        return f"colortemperature=temperature={value:.0f}"
    elif slider == "tint":
        if abs(value) < 0.001:
            return "null"
        t = max(-1, min(1, value))
        return f"colorbalance=rm={t*0.25:.4f}:gm={-t*0.5:.4f}:bm={t*0.25:.4f}"
    elif slider == "shadows":
        s = max(-1, min(1, value))
        return f"colorbalance=rs={s:.4f}:gs={s:.4f}:bs={s:.4f}"
    elif slider == "midtones":
        m = max(-1, min(1, value))
        return f"colorbalance=rm={m:.4f}:gm={m:.4f}:bm={m:.4f}"
    elif slider == "highlights":
        h = max(-1, min(1, value))
        return f"colorbalance=rh={h:.4f}:gh={h:.4f}:bh={h:.4f}"
    return "null"


def generate_ffmpeg_reference(frame_path, slider, value, out_path):
    filt = ffmpeg_export_filter(slider, value)
    cmd = ["ffmpeg", "-y", "-hide_banner", "-loglevel", "error",
           "-i", frame_path, "-vf", filt, "-frames:v", "1", out_path]
    subprocess.run(cmd, check=True)


# ---------------------------------------------------------------------------
# RMSE
# ---------------------------------------------------------------------------
def compute_rmse(a, b):
    return float(np.sqrt(np.mean(((a - b) * 255.0) ** 2)))


# ---------------------------------------------------------------------------
# Optimization: temperature/tint -> coloradj_RGB params
# ---------------------------------------------------------------------------
def optimize_coloradj(ref_img, src_img, seed):
    def objective(params):
        r, g, b = params
        preview = apply_coloradj_rgb(src_img, r, g, b)
        return compute_rmse(preview, ref_img)

    bounds = [(0.0, 1.0), (0.0, 1.0), (0.0, 1.0)]
    best = None
    # Multi-start: seed + 5 random restarts
    starts = [[seed["r"], seed["g"], seed["b"]]]
    rng = np.random.RandomState(42)
    for _ in range(5):
        starts.append([rng.uniform(0.1, 0.9), rng.uniform(0.1, 0.9), rng.uniform(0.1, 0.9)])
    for x0 in starts:
        result = minimize(objective, x0, method='L-BFGS-B', bounds=bounds,
                          options={'maxiter': 200, 'ftol': 1e-7})
        if best is None or result.fun < best.fun:
            best = result
    r, g, b = best.x
    return {"r": float(r), "g": float(g), "b": float(b), "rmse": float(best.fun)}


# ---------------------------------------------------------------------------
# Optimization: shadows/midtones/highlights -> 3-point params
# ---------------------------------------------------------------------------
def optimize_3point(ref_img, src_img, seed):
    def objective(params):
        black, gray, white = params
        preview = apply_3point(src_img, black, gray, white)
        return compute_rmse(preview, ref_img)

    bounds = [(0.0, 0.95), (0.01, 0.99), (0.05, 1.0)]
    best = None
    # Multi-start: seed + 8 random restarts
    starts = [[seed["black"], seed["gray"], seed["white"]]]
    rng = np.random.RandomState(42)
    for _ in range(8):
        starts.append([rng.uniform(0.0, 0.5), rng.uniform(0.2, 0.8), rng.uniform(0.5, 1.0)])
    for x0 in starts:
        result = minimize(objective, x0, method='L-BFGS-B', bounds=bounds,
                          options={'maxiter': 200, 'ftol': 1e-7})
        if best is None or result.fun < best.fun:
            best = result
    black, gray, white = best.x
    return {"black": float(black), "gray": float(gray), "white": float(white),
            "rmse": float(best.fun)}


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
def run_calibration(frame_path, out_dir):
    src_pil = Image.open(frame_path).convert("RGB").resize((480, 270), Image.LANCZOS)
    src_img = np.array(src_pil).astype(np.float32) / 255.0
    os.makedirs(out_dir, exist_ok=True)
    small_frame = os.path.join(out_dir, "_small_ref.png")
    src_pil.save(small_frame)

    results = {}

    for slider, (smin, smax, sdefault, nsteps) in SLIDERS.items():
        print(f"\n{'='*70}")
        print(f"Calibrating: {slider}  [{smin} .. {smax}]  (default={sdefault})")
        print(f"{'='*70}")

        is_coloradj = slider in ("temperature", "tint")
        values = np.linspace(smin, smax, nsteps).tolist()
        slider_results = []

        for val in values:
            # Skip default (identity)
            is_default = False
            if slider in ("tint", "shadows", "midtones", "highlights") and abs(val) < 1e-6:
                is_default = True
            if slider == "temperature" and abs(val - 6500.0) < 1.0:
                is_default = True

            if is_default:
                if is_coloradj:
                    slider_results.append({"value": val,
                        "current_rmse": 0.0, "optimal_rmse": 0.0,
                        "current_params": {"r": 0.5, "g": 0.5, "b": 0.5},
                        "optimal_params": {"r": 0.5, "g": 0.5, "b": 0.5}})
                else:
                    slider_results.append({"value": val,
                        "current_rmse": 0.0, "optimal_rmse": 0.0,
                        "current_params": {"black": 0.0, "gray": 0.5, "white": 1.0},
                        "optimal_params": {"black": 0.0, "gray": 0.5, "white": 1.0}})
                continue

            # Generate FFmpeg reference
            ref_path = os.path.join(out_dir, f"ref_{slider}_{val:.4f}.png")
            generate_ffmpeg_reference(small_frame, slider, val, ref_path)
            ref_img = np.array(Image.open(ref_path).convert("RGB")).astype(np.float32) / 255.0

            if is_coloradj:
                # Temperature / tint
                if slider == "temperature":
                    cur = current_coloradj_params(val, 0.0)
                else:
                    cur = current_coloradj_params(6500.0, val)

                cur_preview = apply_coloradj_rgb(src_img, cur["r"], cur["g"], cur["b"])
                cur_rmse = compute_rmse(cur_preview, ref_img)
                opt = optimize_coloradj(ref_img, src_img, cur)
                opt_params = {k: opt[k] for k in ("r", "g", "b")}
                opt_rmse = opt["rmse"]

                improvement = ((cur_rmse - opt_rmse) / max(cur_rmse, 1e-9)) * 100
                print(f"  {slider}={val:>9.1f}  cur_rmse={cur_rmse:6.2f}  opt_rmse={opt_rmse:6.2f}  "
                      f"Δ={improvement:+5.1f}%")
                print(f"    current:  r={cur['r']:.4f} g={cur['g']:.4f} b={cur['b']:.4f}")
                print(f"    optimal:  r={opt_params['r']:.4f} g={opt_params['g']:.4f} b={opt_params['b']:.4f}")

                slider_results.append({"value": val,
                    "current_rmse": cur_rmse, "optimal_rmse": opt_rmse,
                    "current_params": cur, "optimal_params": opt_params})
            else:
                # Shadows / midtones / highlights
                if slider == "shadows":
                    cur = current_3point_params(val, 0.0, 0.0)
                elif slider == "midtones":
                    cur = current_3point_params(0.0, val, 0.0)
                else:
                    cur = current_3point_params(0.0, 0.0, val)

                cur_preview = apply_3point(src_img, cur["black"], cur["gray"], cur["white"])
                cur_rmse = compute_rmse(cur_preview, ref_img)
                opt = optimize_3point(ref_img, src_img, cur)
                opt_params = {k: opt[k] for k in ("black", "gray", "white")}
                opt_rmse = opt["rmse"]

                improvement = ((cur_rmse - opt_rmse) / max(cur_rmse, 1e-9)) * 100
                print(f"  {slider}={val:>6.2f}  cur_rmse={cur_rmse:6.2f}  opt_rmse={opt_rmse:6.2f}  "
                      f"Δ={improvement:+5.1f}%")
                print(f"    current:  black={cur['black']:.4f} gray={cur['gray']:.4f} white={cur['white']:.4f}")
                print(f"    optimal:  black={opt_params['black']:.4f} gray={opt_params['gray']:.4f} white={opt_params['white']:.4f}")

                slider_results.append({"value": val,
                    "current_rmse": cur_rmse, "optimal_rmse": opt_rmse,
                    "current_params": cur, "optimal_params": opt_params})

            os.remove(ref_path)

        results[slider] = slider_results

    # Fit polynomial mappings
    print(f"\n{'='*70}")
    print("Fitting polynomial mappings...")
    print(f"{'='*70}")
    coefficients = fit_polynomials(results)
    results["_coefficients"] = coefficients

    # Save
    results_path = os.path.join(out_dir, "frei0r_calibration.json")
    with open(results_path, "w") as f:
        json.dump(results, f, indent=2)
    print(f"\nResults saved to: {results_path}")

    # Print Rust code
    print(f"\n{'='*70}")
    print("Suggested Rust implementation:")
    print(f"{'='*70}")
    print_rust_code(coefficients)

    return results


# ---------------------------------------------------------------------------
# Polynomial fitting
# ---------------------------------------------------------------------------
def fit_polynomials(results):
    from numpy.polynomial import polynomial as P
    coefficients = {}

    for slider, data in results.items():
        if slider.startswith("_"):
            continue

        is_coloradj = slider in ("temperature", "tint")
        vals = [d["value"] for d in data]
        vals = np.array(vals)

        if slider == "temperature":
            norm_vals = (vals - 6500.0) / 4500.0
        else:
            norm_vals = vals

        if is_coloradj:
            params_names = ["r", "g", "b"]
            defaults = {"r": 0.5, "g": 0.5, "b": 0.5}
        else:
            params_names = ["black", "gray", "white"]
            defaults = {"black": 0.0, "gray": 0.5, "white": 1.0}

        degree = min(4, len(vals) - 1)
        coeff = {}
        for pname in params_names:
            opt_vals = np.array([d["optimal_params"][pname] for d in data])
            c = P.polyfit(norm_vals, opt_vals, degree)
            fitted = P.polyval(norm_vals, c)
            residual = float(np.sqrt(np.mean((opt_vals - fitted) ** 2)))
            coeff[pname] = {"coefficients": c.tolist(), "residual": residual,
                            "default": defaults[pname]}

        coefficients[slider] = coeff
        print(f"\n  {slider}:")
        for pname in params_names:
            c = coeff[pname]
            print(f"    {pname}: coeffs=[{', '.join(f'{x:.6f}' for x in c['coefficients'])}]"
                  f"  residual={c['residual']:.6f}")

    return coefficients


# ---------------------------------------------------------------------------
# Rust code generation
# ---------------------------------------------------------------------------
def print_rust_code(coefficients):
    print()

    # Temperature / tint -> coloradj_RGB
    for slider in ("temperature", "tint"):
        if slider not in coefficients:
            continue
        coeff = coefficients[slider]
        print(f"    // ── {slider.capitalize()} ──")
        if slider == "temperature":
            print(f"    let t = (temperature - 6500.0) / 4500.0;")
        else:
            print(f"    let t = tint;")

        for pname in ("r", "g", "b"):
            c = coeff[pname]
            cs = c["coefficients"]
            default = c["default"]
            # Build polynomial expression using poly4-style const array
            # numpy.polynomial uses [c0, c1, c2, c3, c4] where val = c0 + c1*t + c2*t^2 + ...
            arr = ", ".join(f"{x:>12.6f}" for x in cs)
            print(f"    const {slider.upper()}_{pname.upper()}: [f64; {len(cs)}] = [{arr}];")

        print(f"    {{")
        for pname in ("r", "g", "b"):
            c = coeff[pname]
            default = c["default"]
            print(f"        let {pname}_val = Self::poly4(t, &{slider.upper()}_{pname.upper()});")
        print(f"    }}")
        print()

    # Shadows / midtones / highlights -> 3-point params
    for slider in ("shadows", "midtones", "highlights"):
        if slider not in coefficients:
            continue
        coeff = coefficients[slider]
        short = {"shadows": "SH", "midtones": "M", "highlights": "H"}[slider]
        print(f"    // ── {slider.capitalize()} ──")
        print(f"    let t = {slider};")

        for pname in ("black", "gray", "white"):
            c = coeff[pname]
            cs = c["coefficients"]
            arr = ", ".join(f"{x:>12.6f}" for x in cs)
            print(f"    const {short}_{pname.upper()}: [f64; {len(cs)}] = [{arr}];")

        print(f"    {{")
        for pname in ("black", "gray", "white"):
            print(f"        let {pname}_val = Self::poly4(t, &{short}_{pname.upper()});")
        print(f"    }}")
        print()


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------
if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Calibrate frei0r preview vs FFmpeg export")
    parser.add_argument("--frame", default=None, help="Path to test frame PNG")
    parser.add_argument("--out", default="/tmp/us_frei0r_calib", help="Output directory")
    args = parser.parse_args()

    frame_path = args.frame
    if frame_path is None:
        candidates = [
            Path(__file__).parent.parent / "Sample-Media" / "calibration_chart.png",
            Path("/tmp/us_calib/calibration_chart.png"),
        ]
        for c in candidates:
            if c.exists():
                frame_path = str(c)
                break
    if frame_path is None or not Path(frame_path).exists():
        print("Error: test frame not found. Run tools/generate_test_chart.sh first.")
        sys.exit(1)

    print(f"Test frame: {frame_path}")
    print(f"Output dir: {args.out}")
    run_calibration(frame_path, args.out)
