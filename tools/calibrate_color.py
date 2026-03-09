#!/usr/bin/env python3
"""
Calibrate GStreamer videobalance preview parameters to approximate FFmpeg export filters.

For each color slider, this script:
  1. Applies the ffmpeg export filter chain to a reference frame  → "ground truth"
  2. Applies the current GStreamer videobalance formula (simulated in numpy) → "current preview"
  3. Computes per-pixel RMSE between the two
  4. Uses scipy.optimize to find improved polynomial coefficients that minimise the RMSE
  5. Outputs a JSON file with the new coefficients and optional PNG comparison plots.

Usage:
    python3 tools/calibrate_color.py [--frame /tmp/us_calib/calibration_chart.png] [--out /tmp/us_calib_results]

Requirements: numpy, scipy, pillow   (pip3 install numpy scipy pillow)
Optional:     matplotlib              (for --plot)
"""

import argparse, json, os, subprocess, sys, tempfile
from pathlib import Path
import numpy as np
from scipy.optimize import minimize
from PIL import Image

# ---------------------------------------------------------------------------
# Slider definitions: (name, min, max, default, num_steps)
# ---------------------------------------------------------------------------
SLIDERS = {
    "brightness":  (-1.0,  1.0, 0.0, 11),
    "contrast":    ( 0.0,  2.0, 1.0, 11),
    "saturation":  ( 0.0,  2.0, 1.0, 11),
    "temperature": (2000, 10000, 6500, 11),
    "tint":        (-1.0,  1.0, 0.0, 11),
    "shadows":     (-1.0,  1.0, 0.0, 11),
    "midtones":    (-1.0,  1.0, 0.0, 11),
    "highlights":  (-1.0,  1.0, 0.0, 11),
}

# ---------------------------------------------------------------------------
# numpy simulation of GStreamer videobalance (RGB → HSV → adjust → RGB)
# ---------------------------------------------------------------------------
def rgb_to_hsv(img: np.ndarray) -> np.ndarray:
    """Convert float32 RGB [0,1] image to HSV [0,1]."""
    r, g, b = img[..., 0], img[..., 1], img[..., 2]
    maxc = np.maximum(np.maximum(r, g), b)
    minc = np.minimum(np.minimum(r, g), b)
    v = maxc
    s = np.where(maxc > 0, (maxc - minc) / maxc, 0.0)
    delta = maxc - minc
    delta_safe = np.where(delta == 0, 1.0, delta)
    rc = (maxc - r) / delta_safe
    gc = (maxc - g) / delta_safe
    bc = (maxc - b) / delta_safe
    h = np.where(r == maxc, bc - gc,
        np.where(g == maxc, 2.0 + rc - bc,
                 4.0 + gc - rc))
    h = (h / 6.0) % 1.0
    h = np.where(delta == 0, 0.0, h)
    return np.stack([h, s, v], axis=-1)


def hsv_to_rgb(img: np.ndarray) -> np.ndarray:
    """Convert float32 HSV [0,1] image to RGB [0,1]."""
    h, s, v = img[..., 0], img[..., 1], img[..., 2]
    i = (h * 6.0).astype(np.int32)
    f = (h * 6.0) - i
    p = v * (1.0 - s)
    q = v * (1.0 - s * f)
    t = v * (1.0 - s * (1.0 - f))
    i = i % 6
    conditions = [i == 0, i == 1, i == 2, i == 3, i == 4, i == 5]
    r = np.select(conditions, [v, q, p, p, t, v], default=v)
    g = np.select(conditions, [t, v, v, q, p, p], default=p)
    b = np.select(conditions, [p, p, t, v, v, q], default=p)
    return np.stack([r, g, b], axis=-1)


def apply_videobalance(img_rgb: np.ndarray,
                       brightness: float = 0.0,
                       contrast: float = 1.0,
                       saturation: float = 1.0,
                       hue: float = 0.0) -> np.ndarray:
    """
    Simulate GStreamer videobalance in numpy.
    Input/output: float32 RGB [0,1].
    GStreamer videobalance behaviour (from source):
      - brightness:  clamp(pixel + brightness)          per-channel additive
      - contrast:    clamp((pixel - 0.5) * contrast + 0.5)   per-channel around mid-gray
      - saturation:  scale S channel in HSV
      - hue:         rotate H channel in HSV (range -1..1 maps to -pi..pi)
    """
    out = img_rgb.copy()
    # brightness (additive)
    out = out + brightness
    # contrast (around 0.5)
    out = (out - 0.5) * contrast + 0.5
    out = np.clip(out, 0.0, 1.0)
    # saturation & hue in HSV
    if saturation != 1.0 or hue != 0.0:
        hsv = rgb_to_hsv(out)
        if saturation != 1.0:
            hsv[..., 1] = np.clip(hsv[..., 1] * saturation, 0.0, 1.0)
        if hue != 0.0:
            hsv[..., 0] = (hsv[..., 0] + hue * 0.5) % 1.0  # hue -1..1 → -0.5..0.5 turns
        out = hsv_to_rgb(hsv)
        out = np.clip(out, 0.0, 1.0)
    return out


# ---------------------------------------------------------------------------
# Current GStreamer mapping formulas (from program_player.rs)
# ---------------------------------------------------------------------------
def current_gst_mapping(slider: str, value: float) -> dict:
    """Return videobalance params {brightness, contrast, saturation, hue} for a single slider."""
    b, c, s, h = 0.0, 1.0, 1.0, 0.0
    if slider == "brightness":
        b = np.clip(value, -1.0, 1.0)
    elif slider == "contrast":
        c = np.clip(value, 0.0, 2.0)
    elif slider == "saturation":
        s = np.clip(value, 0.0, 2.0)
    elif slider == "temperature":
        h = np.clip((value - 6500.0) / 6500.0 * -0.15, -0.25, 0.25)
    elif slider == "tint":
        h = np.clip(value * 0.08, -0.15, 0.15)
    elif slider == "shadows":
        b = np.clip(value * 0.3, -1.0, 1.0)
        c = np.clip(1.0 - value * 0.15, 0.0, 2.0)
    elif slider == "midtones":
        b = np.clip(value * 0.2, -1.0, 1.0)
    elif slider == "highlights":
        b = np.clip(value * 0.15, -1.0, 1.0)
        c = np.clip(1.0 + value * 0.15, 0.0, 2.0)
    return {"brightness": b, "contrast": c, "saturation": s, "hue": h}


# ---------------------------------------------------------------------------
# FFmpeg reference frame generation
# ---------------------------------------------------------------------------
def ffmpeg_export_filter(slider: str, value: float) -> str:
    """Build the ffmpeg filter string that the export pipeline would use."""
    if slider == "brightness":
        return f"eq=brightness={value:.4f}:contrast=1:saturation=1"
    elif slider == "contrast":
        return f"eq=brightness=0:contrast={value:.4f}:saturation=1"
    elif slider == "saturation":
        return f"eq=brightness=0:contrast=1:saturation={value:.4f}"
    elif slider == "temperature":
        return f"colortemperature=temperature={value:.0f}"
    elif slider == "tint":
        if abs(value) < 0.001:
            return "null"
        t = np.clip(value, -1.0, 1.0)
        gm = -t * 0.5
        rm = t * 0.25
        bm = t * 0.25
        return f"colorbalance=rm={rm:.4f}:gm={gm:.4f}:bm={bm:.4f}"
    elif slider == "shadows":
        s = np.clip(value, -1.0, 1.0)
        return f"colorbalance=rs={s:.4f}:gs={s:.4f}:bs={s:.4f}"
    elif slider == "midtones":
        m = np.clip(value, -1.0, 1.0)
        return f"colorbalance=rm={m:.4f}:gm={m:.4f}:bm={m:.4f}"
    elif slider == "highlights":
        h = np.clip(value, -1.0, 1.0)
        return f"colorbalance=rh={h:.4f}:gh={h:.4f}:bh={h:.4f}"
    return "null"


def generate_ffmpeg_reference(frame_path: str, slider: str, value: float, out_path: str):
    """Run ffmpeg to produce the export-equivalent frame."""
    filt = ffmpeg_export_filter(slider, value)
    cmd = [
        "ffmpeg", "-y", "-hide_banner", "-loglevel", "error",
        "-i", frame_path,
        "-vf", filt,
        "-frames:v", "1",
        out_path
    ]
    subprocess.run(cmd, check=True)


# ---------------------------------------------------------------------------
# RMSE computation
# ---------------------------------------------------------------------------
def compute_rmse(img_a: np.ndarray, img_b: np.ndarray) -> float:
    """Per-pixel RMSE in [0,255] space."""
    a = (img_a * 255.0).astype(np.float64)
    b = (img_b * 255.0).astype(np.float64)
    return float(np.sqrt(np.mean((a - b) ** 2)))


def compute_channel_rmse(img_a: np.ndarray, img_b: np.ndarray) -> dict:
    """Per-channel RMSE in [0,255] space."""
    a = (img_a * 255.0).astype(np.float64)
    b = (img_b * 255.0).astype(np.float64)
    return {
        "r": float(np.sqrt(np.mean((a[...,0] - b[...,0]) ** 2))),
        "g": float(np.sqrt(np.mean((a[...,1] - b[...,1]) ** 2))),
        "b": float(np.sqrt(np.mean((a[...,2] - b[...,2]) ** 2))),
        "total": float(np.sqrt(np.mean((a - b) ** 2))),
    }


# ---------------------------------------------------------------------------
# Calibration: find optimal videobalance params for each slider value
# ---------------------------------------------------------------------------
def find_optimal_vb_params(ref_img: np.ndarray, src_img: np.ndarray,
                           seed: dict = None) -> dict:
    """
    Given a reference (ffmpeg export) image and the source image,
    find the videobalance parameters that best approximate the reference.
    """
    def objective(params):
        b, c, s, h = params
        preview = apply_videobalance(src_img, brightness=b, contrast=c, saturation=s, hue=h)
        return compute_rmse(preview, ref_img)

    # Start from seed (current mapping) or neutral
    if seed:
        x0 = [seed["brightness"], seed["contrast"], seed["saturation"], seed["hue"]]
    else:
        x0 = [0.0, 1.0, 1.0, 0.0]
    bounds = [(-1.0, 1.0), (0.01, 3.0), (0.0, 3.0), (-1.0, 1.0)]
    result = minimize(objective, x0, method='L-BFGS-B', bounds=bounds,
                      options={'maxiter': 150, 'ftol': 1e-6})
    b, c, s, h = result.x
    return {"brightness": float(b), "contrast": float(c),
            "saturation": float(s), "hue": float(h), "rmse": float(result.fun)}


# ---------------------------------------------------------------------------
# Main calibration loop
# ---------------------------------------------------------------------------
def run_calibration(frame_path: str, out_dir: str, do_plot: bool = False):
    src_full = np.array(Image.open(frame_path).convert("RGB")).astype(np.float32) / 255.0
    # Downsample to 480x270 for fast optimization (color relationships are resolution-independent)
    src_pil = Image.open(frame_path).convert("RGB").resize((480, 270), Image.LANCZOS)
    src_img = np.array(src_pil).astype(np.float32) / 255.0
    os.makedirs(out_dir, exist_ok=True)

    # Also save a small reference frame for ffmpeg processing
    small_frame = os.path.join(out_dir, "_small_ref.png")
    src_pil.save(small_frame)

    results = {}
    for slider, (smin, smax, sdefault, nsteps) in SLIDERS.items():
        print(f"\n{'='*60}")
        print(f"Calibrating: {slider}  [{smin} .. {smax}]  (default={sdefault})")
        print(f"{'='*60}")

        values = np.linspace(smin, smax, nsteps).tolist()
        slider_results = []

        for val in values:
            # Skip default values (no change expected)
            if slider in ("brightness", "tint", "shadows", "midtones", "highlights") and abs(val) < 1e-6:
                slider_results.append({
                    "value": val,
                    "current_rmse": 0.0, "optimal_rmse": 0.0,
                    "current_params": {"brightness": 0.0, "contrast": 1.0, "saturation": 1.0, "hue": 0.0},
                    "optimal_params": {"brightness": 0.0, "contrast": 1.0, "saturation": 1.0, "hue": 0.0},
                })
                continue
            if slider in ("contrast", "saturation") and abs(val - 1.0) < 1e-6:
                slider_results.append({
                    "value": val,
                    "current_rmse": 0.0, "optimal_rmse": 0.0,
                    "current_params": {"brightness": 0.0, "contrast": 1.0, "saturation": 1.0, "hue": 0.0},
                    "optimal_params": {"brightness": 0.0, "contrast": 1.0, "saturation": 1.0, "hue": 0.0},
                })
                continue
            if slider == "temperature" and abs(val - 6500.0) < 1.0:
                slider_results.append({
                    "value": val,
                    "current_rmse": 0.0, "optimal_rmse": 0.0,
                    "current_params": {"brightness": 0.0, "contrast": 1.0, "saturation": 1.0, "hue": 0.0},
                    "optimal_params": {"brightness": 0.0, "contrast": 1.0, "saturation": 1.0, "hue": 0.0},
                })
                continue

            # Generate ffmpeg reference (at small resolution for speed)
            ref_path = os.path.join(out_dir, f"ref_{slider}_{val:.4f}.png")
            generate_ffmpeg_reference(small_frame, slider, val, ref_path)
            ref_img = np.array(Image.open(ref_path).convert("RGB")).astype(np.float32) / 255.0

            # Current GStreamer mapping
            cur_params = current_gst_mapping(slider, val)
            cur_preview = apply_videobalance(src_img, **cur_params)
            cur_rmse = compute_rmse(cur_preview, ref_img)

            # Find optimal videobalance params (seeded from current mapping)
            opt = find_optimal_vb_params(ref_img, src_img, seed=cur_params)
            opt_params = {k: opt[k] for k in ("brightness", "contrast", "saturation", "hue")}
            opt_rmse = opt["rmse"]

            improvement = ((cur_rmse - opt_rmse) / max(cur_rmse, 1e-9)) * 100
            print(f"  {slider}={val:>8.2f}  current_rmse={cur_rmse:6.2f}  optimal_rmse={opt_rmse:6.2f}  "
                  f"improvement={improvement:5.1f}%")
            print(f"    current:  b={cur_params['brightness']:+.4f} c={cur_params['contrast']:.4f} "
                  f"s={cur_params['saturation']:.4f} h={cur_params['hue']:+.4f}")
            print(f"    optimal:  b={opt_params['brightness']:+.4f} c={opt_params['contrast']:.4f} "
                  f"s={opt_params['saturation']:.4f} h={opt_params['hue']:+.4f}")

            slider_results.append({
                "value": val,
                "current_rmse": cur_rmse,
                "optimal_rmse": opt_rmse,
                "current_params": cur_params,
                "optimal_params": opt_params,
            })

            # Clean up reference frame
            os.remove(ref_path)

        results[slider] = slider_results

    # Fit polynomial mappings for each slider
    print(f"\n{'='*60}")
    print("Fitting polynomial mappings...")
    print(f"{'='*60}")
    coefficients = fit_polynomials(results)
    results["_coefficients"] = coefficients

    # Save results
    results_path = os.path.join(out_dir, "calibration_results.json")
    with open(results_path, "w") as f:
        json.dump(results, f, indent=2)
    print(f"\nResults saved to: {results_path}")

    # Print Rust code
    print(f"\n{'='*60}")
    print("Suggested Rust implementation:")
    print(f"{'='*60}")
    print_rust_code(coefficients)

    if do_plot:
        try:
            generate_plots(results, out_dir)
        except ImportError:
            print("matplotlib not available, skipping plots")

    return results


# ---------------------------------------------------------------------------
# Polynomial fitting
# ---------------------------------------------------------------------------
def fit_polynomials(results: dict) -> dict:
    """
    For each slider, fit polynomial curves mapping slider_value → optimal (b, c, s, h).
    """
    from numpy.polynomial import polynomial as P
    coefficients = {}

    for slider, data in results.items():
        if slider.startswith("_"):
            continue
        vals = []
        opt_b, opt_c, opt_s, opt_h = [], [], [], []
        for entry in data:
            vals.append(entry["value"])
            opt_b.append(entry["optimal_params"]["brightness"])
            opt_c.append(entry["optimal_params"]["contrast"])
            opt_s.append(entry["optimal_params"]["saturation"])
            opt_h.append(entry["optimal_params"]["hue"])

        vals = np.array(vals)
        # Normalise slider values for temperature (Kelvin → normalised)
        if slider == "temperature":
            norm_vals = (vals - 6500.0) / 4500.0  # maps 2000..10000 → ~-1..0.78
        else:
            norm_vals = vals

        degree = min(4, len(vals) - 1)  # up to quartic
        coeff = {}
        for prop_name, prop_vals in [("brightness", opt_b), ("contrast", opt_c),
                                      ("saturation", opt_s), ("hue", opt_h)]:
            prop_arr = np.array(prop_vals)
            # Fit polynomial; use lower degree if data is near-constant
            c = P.polyfit(norm_vals, prop_arr, degree)
            # Evaluate fit quality
            fitted = P.polyval(norm_vals, c)
            residual = float(np.sqrt(np.mean((prop_arr - fitted) ** 2)))
            coeff[prop_name] = {
                "coefficients": c.tolist(),
                "residual": residual,
            }

        coefficients[slider] = coeff
        print(f"\n  {slider}:")
        for prop_name in ("brightness", "contrast", "saturation", "hue"):
            c = coeff[prop_name]
            print(f"    {prop_name}: coeffs={[f'{x:.6f}' for x in c['coefficients']]}  residual={c['residual']:.6f}")

    return coefficients


# ---------------------------------------------------------------------------
# Rust code generation
# ---------------------------------------------------------------------------
def print_rust_code(coefficients: dict):
    """Print suggested Rust implementation."""

    print("""
/// Calibrated videobalance parameters for preview color correction.
#[derive(Debug, Clone, Copy)]
pub struct VideoBalanceParams {
    pub brightness: f64,
    pub contrast: f64,
    pub saturation: f64,
    pub hue: f64,
}

impl Default for VideoBalanceParams {
    fn default() -> Self {
        Self { brightness: 0.0, contrast: 1.0, saturation: 1.0, hue: 0.0 }
    }
}

/// Compute calibrated videobalance parameters from clip color settings.
/// Coefficients derived from empirical calibration against ffmpeg export filters.
pub fn compute_videobalance_params(clip: &ProgramClip) -> VideoBalanceParams {
    let mut p = VideoBalanceParams::default();
""")

    for slider in ("brightness", "contrast", "saturation", "temperature", "tint",
                    "shadows", "midtones", "highlights"):
        if slider not in coefficients:
            continue
        coeff = coefficients[slider]
        print(f"    // --- {slider} ---")
        if slider == "temperature":
            print(f"    let t = (clip.temperature - 6500.0) / 4500.0;")
        elif slider == "brightness":
            print(f"    let t = clip.brightness;")
        elif slider == "contrast":
            print(f"    let t = clip.contrast;")
        elif slider == "saturation":
            print(f"    let t = clip.saturation;")
        elif slider == "tint":
            print(f"    let t = clip.tint;")
        elif slider == "shadows":
            print(f"    let t = clip.shadows;")
        elif slider == "midtones":
            print(f"    let t = clip.midtones;")
        elif slider == "highlights":
            print(f"    let t = clip.highlights;")

        for prop_name in ("brightness", "contrast", "saturation", "hue"):
            c = coeff[prop_name]["coefficients"]
            if all(abs(x) < 1e-6 for x in c):
                continue
            # Check if this is a "delta" property (add) or "absolute" (set)
            if prop_name == "contrast":
                default = 1.0
            elif prop_name == "saturation":
                default = 1.0
            else:
                default = 0.0

            # Build polynomial expression
            terms = []
            for i, ci in enumerate(c):
                if abs(ci) < 1e-7:
                    continue
                if i == 0:
                    # Only include constant if it differs from default
                    if abs(ci - default) > 1e-6:
                        terms.append(f"{ci:.6f}")
                elif i == 1:
                    terms.append(f"{ci:.6f} * t")
                else:
                    terms.append(f"{ci:.6f} * t.powi({i})")

            if terms:
                expr = " + ".join(terms)
                # For additive properties, accumulate
                if slider in ("brightness", "contrast", "saturation"):
                    print(f"    p.{prop_name} = {expr};")
                else:
                    print(f"    p.{prop_name} += {expr};")

        print()

    print("""    // Clamp final values
    p.brightness = p.brightness.clamp(-1.0, 1.0);
    p.contrast = p.contrast.clamp(0.0, 2.0);
    p.saturation = p.saturation.clamp(0.0, 2.0);
    p.hue = p.hue.clamp(-1.0, 1.0);
    p
}""")


# ---------------------------------------------------------------------------
# Plotting (optional)
# ---------------------------------------------------------------------------
def generate_plots(results: dict, out_dir: str):
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt

    for slider, data in results.items():
        if slider.startswith("_"):
            continue

        vals = [d["value"] for d in data]
        cur_rmse = [d["current_rmse"] for d in data]
        opt_rmse = [d["optimal_rmse"] for d in data]

        fig, axes = plt.subplots(1, 2, figsize=(14, 5))
        fig.suptitle(f"Calibration: {slider}", fontsize=14)

        # RMSE comparison
        ax = axes[0]
        ax.plot(vals, cur_rmse, 'r-o', label='Current mapping', markersize=4)
        ax.plot(vals, opt_rmse, 'g-o', label='Optimal mapping', markersize=4)
        ax.set_xlabel(f'{slider} value')
        ax.set_ylabel('RMSE (0-255)')
        ax.set_title('RMSE: Preview vs Export')
        ax.legend()
        ax.grid(True, alpha=0.3)

        # Optimal params
        ax = axes[1]
        for prop in ("brightness", "contrast", "saturation", "hue"):
            cur_vals = [d["current_params"][prop] for d in data]
            opt_vals = [d["optimal_params"][prop] for d in data]
            ax.plot(vals, opt_vals, '-o', label=f'opt_{prop}', markersize=3)
            ax.plot(vals, cur_vals, '--', label=f'cur_{prop}', alpha=0.5)
        ax.set_xlabel(f'{slider} value')
        ax.set_ylabel('videobalance param')
        ax.set_title('Optimal vs Current Params')
        ax.legend(fontsize=8)
        ax.grid(True, alpha=0.3)

        plt.tight_layout()
        plot_path = os.path.join(out_dir, f"calibration_{slider}.png")
        plt.savefig(plot_path, dpi=100)
        plt.close()
        print(f"Plot saved: {plot_path}")


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------
if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Calibrate GStreamer preview vs FFmpeg export")
    parser.add_argument("--frame", default=None, help="Path to test frame PNG")
    parser.add_argument("--out", default="/tmp/us_calib_results", help="Output directory")
    parser.add_argument("--plot", action="store_true", help="Generate comparison plots")
    args = parser.parse_args()

    # Auto-find test frame
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

    print(f"Using test frame: {frame_path}")
    print(f"Output directory: {args.out}")
    run_calibration(frame_path, args.out, do_plot=args.plot)
