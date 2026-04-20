//! Pure-computation colour math shared by preview (`program_player`)
//! and export (`export`) paths.
//!
//! Every function in this module is a stateless `fn(params) → result`
//! with no GStreamer or GTK dependency, making it safe to call from
//! any thread and easy to test in isolation.

// ── Parameter structs ───────────────────────────────────────────────

/// Calibrated videobalance output parameters (brightness, contrast,
/// saturation, hue) computed from clip colour settings by
/// [`compute_videobalance_params`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct VBParams {
    pub(crate) brightness: f64,
    pub(crate) contrast: f64,
    pub(crate) saturation: f64,
    pub(crate) hue: f64,
}

/// Per-channel RGB gains for frei0r `coloradj_RGB` element.
/// Values are in frei0r's [0,1] range where 0.5 = neutral (gain 1.0).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ColorAdjRGBParams {
    pub(crate) r: f64,
    pub(crate) g: f64,
    pub(crate) b: f64,
}

/// Parameters for frei0r `3-point-color-balance` element.
/// Maps shadows/midtones/highlights to the black/gray/white reference
/// points of a piecewise-linear transfer curve.
#[derive(Clone)]
pub(crate) struct ThreePointParams {
    pub(crate) black_r: f64,
    pub(crate) black_g: f64,
    pub(crate) black_b: f64,
    pub(crate) gray_r: f64,
    pub(crate) gray_g: f64,
    pub(crate) gray_b: f64,
    pub(crate) white_r: f64,
    pub(crate) white_g: f64,
    pub(crate) white_b: f64,
}

/// Quadratic (parabola) coefficients for one channel of the frei0r
/// 3-point-color-balance transfer curve.  The plugin fits y = a·x² + b·x + c
/// through (black_c, 0), (gray_c, 0.5), (white_c, 1.0) where x is the
/// normalised input pixel value and y is the output (both 0–1).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ThreePointParabolaCoeffs {
    pub(crate) a: f64,
    pub(crate) b: f64,
    pub(crate) c: f64,
}

impl ThreePointParabolaCoeffs {
    /// Solve the quadratic passing through (x1, 0), (x2, 0.5), (x3, 1.0).
    pub(crate) fn from_control_points(x1: f64, x2: f64, x3: f64) -> Self {
        let d1 = x2 - x1;
        let d2 = x3 - x1;
        let d3 = x3 - x2;
        if d1.abs() < 1e-9 || d2.abs() < 1e-9 || d3.abs() < 1e-9 {
            return Self {
                a: 0.0,
                b: 1.0,
                c: 0.0,
            };
        }
        let a = (1.0 / d2 - 0.5 / d1) / d3;
        let b = 0.5 / d1 - a * (x2 + x1);
        let c = -(a * x1 * x1 + b * x1);
        Self { a, b, c }
    }
}

/// Per-channel parabola coefficients matching the frei0r 3-point plugin.
#[derive(Debug, Clone)]
pub(crate) struct ThreePointParabola {
    pub(crate) r: ThreePointParabolaCoeffs,
    pub(crate) g: ThreePointParabolaCoeffs,
    pub(crate) b: ThreePointParabolaCoeffs,
}

impl ThreePointParabola {
    pub(crate) fn from_params(p: &ThreePointParams) -> Self {
        Self {
            r: ThreePointParabolaCoeffs::from_control_points(p.black_r, p.gray_r, p.white_r),
            g: ThreePointParabolaCoeffs::from_control_points(p.black_g, p.gray_g, p.white_g),
            b: ThreePointParabolaCoeffs::from_control_points(p.black_b, p.gray_b, p.white_b),
        }
    }

    /// Format as an FFmpeg `lutrgb` filter expression.  `val` is 0–255 integer.
    /// output = clamp(a·(val/255)² + b·(val/255) + c, 0, 1) · 255
    ///        = clamp(A·val² + B·val + C, 0, 255)
    /// where A = a/255, B = b, C = c·255.
    pub(crate) fn to_lutrgb_filter(&self) -> String {
        fn chan_expr(tag: &str, c: &ThreePointParabolaCoeffs) -> String {
            let a = c.a / 255.0;
            let b = c.b;
            let cv = c.c * 255.0;
            format!("{tag}='clip({a:.6}*val*val+{b:.6}*val+{cv:.4},0,255)'")
        }
        format!(
            ",lutrgb={}:{}:{}",
            chan_expr("r", &self.r),
            chan_expr("g", &self.g),
            chan_expr("b", &self.b),
        )
    }
}

// ── Polynomial helper ───────────────────────────────────────────────

/// Evaluate degree-4 polynomial: c₀ + c₁t + c₂t² + c₃t³ + c₄t⁴
#[inline]
pub(crate) fn poly4(t: f64, c: &[f64; 5]) -> f64 {
    c[0] + t * (c[1] + t * (c[2] + t * (c[3] + t * c[4])))
}

// ── Videobalance ────────────────────────────────────────────────────
//
// GStreamer `videobalance` operates in RGB/HSV with 4 knobs (brightness,
// contrast, saturation, hue) while the FFmpeg export pipeline uses
// dedicated per-domain filters (eq, colortemperature, colorbalance,
// hqdn3d, unsharp).  These polynomial coefficients were derived from
// empirical calibration (tools/calibrate_color.py) by sweeping each
// slider across its range, generating FFmpeg reference frames, and using
// L-BFGS-B optimisation to find the videobalance params that minimise
// per-pixel RMSE against the FFmpeg output.

/// Compute calibrated videobalance parameters that best approximate the
/// FFmpeg export filter chain.  Each slider contributes a *delta* from
/// neutral (0, 1, 1, 0) based on fitted polynomials.  When multiple
/// sliders are active, deltas are summed (linear superposition — an
/// approximation that works well for moderate adjustments).
///
/// When `has_coloradj` is true, temperature/tint are handled by the
/// frei0r `coloradj_RGB` element and excluded from videobalance.
/// When `has_3point` is true, shadows/midtones/highlights are handled
/// by the frei0r `3-point-color-balance` element.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_videobalance_params(
    brightness: f64,
    contrast: f64,
    saturation: f64,
    temperature: f64,
    tint: f64,
    shadows: f64,
    midtones: f64,
    highlights: f64,
    exposure: f64,
    black_point: f64,
    highlights_warmth: f64,
    highlights_tint: f64,
    midtones_warmth: f64,
    midtones_tint: f64,
    shadows_warmth: f64,
    shadows_tint: f64,
    has_coloradj: bool,
    has_3point: bool,
) -> VBParams {
    let mut b = 0.0_f64;
    let mut c = 1.0_f64;
    let mut s = 1.0_f64;
    let mut h = 0.0_f64;

    // ── Brightness (default 0.0) ────────────────────────────────────
    {
        const B_B: [f64; 5] = [0.005156, 1.260891, 0.002010, -0.217226, -0.009772];
        const B_C: [f64; 5] = [1.025037, 0.023633, -0.205914, -0.132415, 0.120400];
        let t = brightness;
        b += poly4(t, &B_B) - B_B[0];
        c += poly4(t, &B_C) - B_C[0];
    }

    // ── Exposure (default 0.0, range −1..1) ─────────────────────────
    {
        let e = exposure.clamp(-1.0, 1.0);
        b += e * 0.55;
        c += e * 0.12;
    }

    // ── Contrast (default 1.0) ──────────────────────────────────────
    {
        const C_C: [f64; 5] = [0.294574, 0.666291, 0.242201, -0.290659, 0.071964];
        const C_S: [f64; 5] = [2.423564, -3.689165, 3.643393, -1.624246, 0.272677];
        let t = contrast;
        let t0 = 1.0;
        c += poly4(t, &C_C) - poly4(t0, &C_C);
        s += poly4(t, &C_S) - poly4(t0, &C_S);
    }

    // ── Saturation (default 1.0) ────────────────────────────────────
    {
        const S_S: [f64; 5] = [0.364002, 1.024258, -0.843067, 0.625649, -0.135496];
        let t = saturation;
        let t0 = 1.0;
        s += poly4(t, &S_S) - poly4(t0, &S_S);
    }

    // ── Temperature (default 6500K, normalised to [-1, 0.78]) ───────
    if !has_coloradj {
        let t_norm = (temperature - 6500.0) / 6500.0;
        h += (t_norm * -0.15).clamp(-0.25, 0.25);
    }

    // ── Tint (default 0.0) ──────────────────────────────────────────
    if !has_coloradj {
        h += (tint * 0.08).clamp(-0.15, 0.15);
    }

    // ── Shadows (default 0.0) ───────────────────────────────────────
    if !has_3point {
        const SH_B: [f64; 5] = [-0.004654, 0.217915, 0.503318, 0.095557, -0.208157];
        const SH_C: [f64; 5] = [0.967806, -0.569223, -0.773125, 0.208067, 0.503863];
        let t = shadows;
        b += poly4(t, &SH_B) - SH_B[0];
        c += poly4(t, &SH_C) - SH_C[0];
    }

    // ── Midtones (default 0.0) ──────────────────────────────────────
    if !has_3point {
        const M_B: [f64; 5] = [-0.009292, 0.130927, -0.050035, -0.039406, 0.044492];
        const M_C: [f64; 5] = [1.049000, -0.302024, 0.407251, 0.214717, -0.319686];
        let t = midtones;
        b += poly4(t, &M_B) - M_B[0];
        c += poly4(t, &M_C) - M_C[0];
    }

    // ── Highlights (default 0.0) ────────────────────────────────────
    if !has_3point {
        const H_B: [f64; 5] = [-0.002545, 0.500927, -0.437295, -0.060255, 0.152945];
        const H_C: [f64; 5] = [0.918940, 1.420073, 1.848961, -0.254895, -0.846390];
        const H_S: [f64; 5] = [1.031708, -1.145010, 0.783173, 0.699344, -0.956279];
        let t = highlights;
        b += poly4(t, &H_B) - H_B[0];
        c += poly4(t, &H_C) - H_C[0];
        s += poly4(t, &H_S) - H_S[0];
    }

    if !has_3point {
        let bp = black_point.clamp(-1.0, 1.0);
        b += bp * 0.18;
        c += bp * 0.10;

        let warmth_avg =
            (highlights_warmth + midtones_warmth + shadows_warmth).clamp(-3.0, 3.0) / 3.0;
        let tint_avg = (highlights_tint + midtones_tint + shadows_tint).clamp(-3.0, 3.0) / 3.0;
        h += (tint_avg * 0.08 - warmth_avg * 0.06).clamp(-0.20, 0.20);
    }

    VBParams {
        brightness: b.clamp(-1.0, 1.0),
        contrast: c.clamp(0.0, 2.0),
        saturation: s.clamp(0.0, 2.0),
        hue: h.clamp(-1.0, 1.0),
    }
}

// ── coloradj_RGB ────────────────────────────────────────────────────

/// Compute frei0r `coloradj_RGB` parameters for temperature and tint.
///
/// Uses the Tanner Helland algorithm (same as FFmpeg's `colortemperature`
/// filter) to convert a Kelvin value into per-channel RGB gains, then
/// maps those gains into frei0r's [0,1] parameter space where 0.5 is
/// neutral (gain 1.0).
///
/// Tint is applied as a green-channel shift with complementary R/B
/// boost, matching FFmpeg's `colorbalance` midtones approach.
pub(crate) fn compute_coloradj_params(temperature: f64, tint: f64) -> ColorAdjRGBParams {
    const TEMP_R: [f64; 5] = [0.484425, -0.096376, -0.113860, 0.040888, 0.074672];
    const TEMP_G: [f64; 5] = [0.477346, 0.003212, -0.205499, 0.074971, 0.075272];
    const TEMP_B: [f64; 5] = [0.481896, 0.123688, -0.212512, 0.107891, -0.001538];

    const TINT_R: [f64; 5] = [0.501907, 0.059414, 0.031477, 0.020843, -0.014643];
    const TINT_G: [f64; 5] = [0.493519, -0.073231, 0.227645, -0.178444, -0.126107];
    const TINT_B: [f64; 5] = [0.505304, 0.055597, -0.003110, 0.025954, -0.016120];

    let t_temp = ((temperature.clamp(1000.0, 15000.0) - 6500.0) / 4500.0).clamp(-1.0, 1.0);
    let t_tint = tint.clamp(-1.0, 1.0);

    let mut r = poly4(t_temp, &TEMP_R);
    let mut g = poly4(t_temp, &TEMP_G);
    let mut b = poly4(t_temp, &TEMP_B);

    r += poly4(t_tint, &TINT_R) - TINT_R[0];
    g += poly4(t_tint, &TINT_G) - TINT_G[0];
    b += poly4(t_tint, &TINT_B) - TINT_B[0];

    ColorAdjRGBParams {
        r: r.clamp(0.0, 1.0),
        g: g.clamp(0.0, 1.0),
        b: b.clamp(0.0, 1.0),
    }
}

/// Convert a frei0r coloradj parameter (0–1) to a linear gain (0–2).
#[inline]
pub(crate) fn coloradj_param_to_gain(param: f64) -> f64 {
    (param.clamp(0.0, 1.0) * 2.0).clamp(0.0, 2.0)
}

/// Apply videobalance-style colour grading (brightness/contrast/saturation/hue)
/// to a linear RGB triplet.  Used by the preview prerender path to emulate
/// the GStreamer `videobalance` element in software.
#[inline]
pub(crate) fn apply_videobalance_rgb(r: f64, g: f64, b: f64, params: VBParams) -> (f64, f64, f64) {
    let y = 0.299 * r + 0.587 * g + 0.114 * b;
    let mut u = 0.492 * (b - y);
    let mut v = 0.877 * (r - y);
    let y = ((y - 0.5) * params.contrast + 0.5 + params.brightness).clamp(0.0, 1.0);
    let hue_rad = params.hue * std::f64::consts::PI;
    let (sin_h, cos_h) = hue_rad.sin_cos();
    let rot_u = u * cos_h - v * sin_h;
    let rot_v = u * sin_h + v * cos_h;
    u = rot_u * params.saturation;
    v = rot_v * params.saturation;
    let out_r = (y + v / 0.877).clamp(0.0, 1.0);
    let out_b = (y + u / 0.492).clamp(0.0, 1.0);
    let out_g = ((y - 0.299 * out_r - 0.114 * out_b) / 0.587).clamp(0.0, 1.0);
    (out_r, out_g, out_b)
}

// ── Tonal axis response ─────────────────────────────────────────────

/// Non-linear response used by tonal warmth/tint controls.
///
/// Keeps precision near 0.0 while boosting creative range near slider ends.
pub(crate) fn compute_tonal_axis_response(value: f64) -> f64 {
    let v = value.clamp(-1.0, 1.0);
    v * (1.0 + 0.35 * v * v)
}

// ── 3-point colour balance ──────────────────────────────────────────

/// Compute frei0r `3-point-color-balance` parameters from
/// shadows/midtones/highlights sliders.
///
/// The 3-point element fits a parabola through three control points
/// (black, 0), (gray, 0.5), (white, 1.0) per channel and applies it
/// as the transfer curve.  Coefficients derived from empirical
/// calibration against FFmpeg's `colorbalance` export filter
/// (tools/calibrate_frei0r.py).
///
/// Each slider contributes a delta from neutral via fitted polynomials.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_3point_params(
    shadows: f64,
    midtones: f64,
    highlights: f64,
    black_point: f64,
    highlights_warmth: f64,
    highlights_tint: f64,
    midtones_warmth: f64,
    midtones_tint: f64,
    shadows_warmth: f64,
    shadows_tint: f64,
) -> ThreePointParams {
    // ── Shadows ─────────────────────────────────────────────────────
    const SH_BLACK: [f64; 5] = [0.065817, 0.157626, 0.002710, -0.194674, -0.055835];
    const SH_GRAY: [f64; 5] = [0.513596, 0.081446, -0.041960, 0.017848, 0.117598];
    const SH_WHITE: [f64; 5] = [0.999118, -0.037826, -0.087347, -0.046390, -0.004455];

    // ── Midtones ────────────────────────────────────────────────────
    const M_BLACK: [f64; 5] = [0.006185, -0.024554, -0.070816, 0.093034, 0.159694];
    const M_GRAY: [f64; 5] = [0.499145, -0.111600, -0.219452, 0.010107, 0.285960];
    const M_WHITE: [f64; 5] = [0.988180, -0.000025, 0.125044, 0.017842, -0.230026];

    // ── Highlights ──────────────────────────────────────────────────
    const H_BLACK: [f64; 5] = [0.039231, 0.056431, -0.027366, 0.018788, 0.094133];
    const H_GRAY: [f64; 5] = [0.519445, -0.494215, 0.275317, 0.483889, -0.277072];
    const H_WHITE: [f64; 5] = [0.957496, -0.356602, -0.543984, 0.462982, 0.266763];

    let sh = shadows.clamp(-1.0, 1.0);
    let mid = midtones.clamp(-1.0, 1.0);
    let hi = highlights.clamp(-1.0, 1.0);

    let mut black = 0.0_f64;
    let mut gray = 0.5_f64;
    let mut white = 1.0_f64;

    black += poly4(sh, &SH_BLACK) - SH_BLACK[0];
    gray += poly4(sh, &SH_GRAY) - SH_GRAY[0];
    white += poly4(sh, &SH_WHITE) - SH_WHITE[0];

    black += poly4(mid, &M_BLACK) - M_BLACK[0];
    gray += poly4(mid, &M_GRAY) - M_GRAY[0];
    white += poly4(mid, &M_WHITE) - M_WHITE[0];

    black += poly4(hi, &H_BLACK) - H_BLACK[0];
    gray += poly4(hi, &H_GRAY) - H_GRAY[0];
    white += poly4(hi, &H_WHITE) - H_WHITE[0];

    let black = black.clamp(0.0, 0.95);
    let white = white.clamp(0.05, 1.0);
    let gray = gray.clamp(black + 0.01, white - 0.01);

    // ── Black point (default 0.0, range −1..1) ─────────────────────
    let bp = black_point.clamp(-1.0, 1.0) * 0.15;

    // ── Per-tone warmth/tint → per-channel RGB offsets ──────────────
    let warmth_scale = 0.35;
    let tint_scale = 0.28;
    let shadows_endpoint_boost = 1.30;

    let sw = compute_tonal_axis_response(shadows_warmth) * warmth_scale * shadows_endpoint_boost;
    let st = -compute_tonal_axis_response(shadows_tint) * tint_scale * shadows_endpoint_boost;
    let mw = compute_tonal_axis_response(midtones_warmth) * warmth_scale;
    let mt = -compute_tonal_axis_response(midtones_tint) * tint_scale;
    let hw = compute_tonal_axis_response(highlights_warmth) * warmth_scale;
    let ht = -compute_tonal_axis_response(highlights_tint) * tint_scale;

    let black_r = (black + bp - sw + st * 0.5).clamp(0.0, 0.95);
    let black_g = (black + bp - st).clamp(0.0, 0.95);
    let black_b = (black + bp + sw + st * 0.5).clamp(0.0, 0.95);

    let gray_r = (gray - mw + mt * 0.5).clamp(0.01, 0.99);
    let gray_g = (gray - mt).clamp(0.01, 0.99);
    let gray_b = (gray + mw + mt * 0.5).clamp(0.01, 0.99);

    let white_r = (white - hw + ht * 0.5).clamp(0.05, 1.0);
    let white_g = (white - ht).clamp(0.05, 1.0);
    let white_b = (white + hw + ht * 0.5).clamp(0.05, 1.0);

    ThreePointParams {
        black_r,
        black_g,
        black_b,
        gray_r,
        gray_g,
        gray_b,
        white_r,
        white_g,
        white_b,
    }
}

/// Compute export-focused 3-point parameters with small luma harmonization
/// terms tuned to reduce known preview/export endpoint drift.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_export_3point_params(
    shadows: f64,
    midtones: f64,
    highlights: f64,
    black_point: f64,
    highlights_warmth: f64,
    highlights_tint: f64,
    midtones_warmth: f64,
    midtones_tint: f64,
    shadows_warmth: f64,
    shadows_tint: f64,
) -> ThreePointParams {
    let (shadows, midtones, highlights) = export_tonal_parity_inputs(shadows, midtones, highlights);
    compute_3point_params(
        shadows,
        midtones,
        highlights,
        black_point,
        highlights_warmth,
        highlights_tint,
        midtones_warmth,
        midtones_tint,
        shadows_warmth,
        shadows_tint,
    )
}

/// Per-zone additive corrections for the export frei0r 3-point-color-balance
/// path.  Reserved for future per-zone compensation once a reliable model
/// is found.  Previous polynomial offsets regressed midtones parity
/// (3-point curve reshaping has undesirable cross-zone side effects),
/// so the body is intentionally empty.
pub(crate) fn apply_export_3point_parity_offsets(
    _p: &mut ThreePointParams,
    _shadows: f64,
    _midtones: f64,
    _highlights: f64,
) {
    // Intentionally empty — tonal 3-point offsets reverted after
    // MCP cross-runtime validation showed midtones regression.
}

// ── Export temperature parity ────────────────────────────────────────

/// Cool-side export temperature gain (env-overridable for parity fitting).
/// Uses a piecewise curve in cool range with unity at/above 6500K.
pub(crate) fn export_temperature_parity_gain(temperature: f64) -> f64 {
    let legacy_gain = export_gain_env("US_EXPORT_COOL_TEMP_GAIN", 1.0);
    let far_gain = export_gain_env("US_EXPORT_COOL_TEMP_GAIN_FAR", legacy_gain);
    let near_gain = export_gain_env("US_EXPORT_COOL_TEMP_GAIN_NEAR", legacy_gain);
    piecewise_cool_temperature_gain(temperature, far_gain, near_gain)
}

pub(crate) fn piecewise_cool_temperature_gain(
    temperature: f64,
    far_gain: f64,
    near_gain: f64,
) -> f64 {
    const FAR_K: f64 = 2000.0;
    const NEAR_K: f64 = 5000.0;
    const NEUTRAL_K: f64 = 6500.0;

    if temperature >= NEUTRAL_K {
        return 1.0;
    }
    if temperature <= FAR_K {
        return far_gain;
    }
    if temperature <= NEAR_K {
        let t = (temperature - FAR_K) / (NEAR_K - FAR_K);
        return far_gain + (near_gain - far_gain) * t;
    }
    let t = (temperature - NEAR_K) / (NEUTRAL_K - NEAR_K);
    near_gain + (1.0 - near_gain) * t
}

pub(crate) fn export_gain_env(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .filter(|v| *v >= 0.80 && *v <= 1.20)
        .unwrap_or(default)
}

/// Per-channel additive offsets for the export coloradj temperature path.
/// Compensates for FFmpeg/GStreamer frei0r bridge differences in color
/// temperature rendering.  All offsets taper to zero at neutral (6500K).
pub(crate) fn export_temperature_channel_offsets(temperature: f64) -> (f64, f64, f64) {
    let deviation = (temperature - 6500.0) / 4500.0;
    if deviation.abs() < 0.01 {
        return (0.0, 0.0, 0.0);
    }
    let abs_dev = deviation.abs().min(1.0);

    if deviation < 0.0 {
        (0.0, 0.0, 0.0)
    } else {
        (-abs_dev * 0.012, -abs_dev * 0.008, -abs_dev * 0.022)
    }
}

// ── Export tonal parity ─────────────────────────────────────────────

pub(crate) fn export_tonal_parity_inputs(
    shadows: f64,
    midtones: f64,
    highlights: f64,
) -> (f64, f64, f64) {
    let shadows_pos_gain = export_gain_env("US_EXPORT_SHADOWS_POS_GAIN", 1.0);
    let midtones_neg_gain = export_gain_env("US_EXPORT_MIDTONES_NEG_GAIN", 1.0);
    let highlights_neg_gain = export_gain_env("US_EXPORT_HIGHLIGHTS_NEG_GAIN", 1.0);
    apply_export_tonal_parity_gains(
        shadows,
        midtones,
        highlights,
        shadows_pos_gain,
        midtones_neg_gain,
        highlights_neg_gain,
    )
}

pub(crate) fn apply_export_tonal_parity_gains(
    shadows: f64,
    midtones: f64,
    highlights: f64,
    shadows_pos_gain: f64,
    midtones_neg_gain: f64,
    highlights_neg_gain: f64,
) -> (f64, f64, f64) {
    let sh = shadows.clamp(-1.0, 1.0);
    let mid = midtones.clamp(-1.0, 1.0);
    let hi = highlights.clamp(-1.0, 1.0);

    let sh = if sh > 0.0 {
        (sh * shadows_pos_gain).clamp(-1.0, 1.0)
    } else {
        sh
    };
    let mid = if mid < 0.0 {
        (mid * midtones_neg_gain).clamp(-1.0, 1.0)
    } else {
        mid
    };
    let hi = if hi < 0.0 {
        (hi * highlights_neg_gain).clamp(-1.0, 1.0)
    } else {
        hi
    };
    (sh, mid, hi)
}

// ── Audio math helpers ──────────────────────────────────────────────

/// Compute EQ bandwidth from centre frequency and Q factor.
/// Shared by GStreamer equalizer (preview) and FFmpeg equalizer (export).
#[inline]
pub(crate) fn eq_bandwidth(freq: f64, q: f64) -> f64 {
    freq / q.max(0.1)
}

/// Compute the voice-isolation ducking floor as a volume multiplier.
///
/// When `voice_isolation > 0`, non-speech regions are ducked towards
/// `floor` instead of silence.  Returns a value in `[floor, 1.0]`.
///
/// Preview uses `1.0 − duck_range` where `duck_range = iso * (1 − floor)`,
/// export uses `(1 − iso) * (1 − floor) + floor` — they are algebraically
/// identical.
#[inline]
pub(crate) fn voice_ducking_floor(voice_isolation: f64, floor: f64) -> f64 {
    (1.0 - voice_isolation) * (1.0 - floor) + floor
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poly4_linear() {
        let c = [1.0, 2.0, 0.0, 0.0, 0.0];
        assert!((poly4(3.0, &c) - 7.0).abs() < 1e-9);
    }

    #[test]
    fn eq_bandwidth_clamps_q() {
        assert!((eq_bandwidth(1000.0, 1.0) - 1000.0).abs() < 1e-9);
        assert!((eq_bandwidth(1000.0, 0.01) - 10000.0).abs() < 1e-9);
    }

    #[test]
    fn voice_ducking_floor_identity_at_zero_isolation() {
        assert!((voice_ducking_floor(0.0, 0.3) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn voice_ducking_floor_full_isolation_returns_floor() {
        assert!((voice_ducking_floor(1.0, 0.3) - 0.3).abs() < 1e-9);
    }

    #[test]
    fn coloradj_neutral_at_6500k() {
        let cp = compute_coloradj_params(6500.0, 0.0);
        assert!((cp.r - 0.5).abs() < 0.02, "neutral r={}", cp.r);
        assert!((cp.g - 0.5).abs() < 0.03, "neutral g={}", cp.g);
        assert!((cp.b - 0.5).abs() < 0.02, "neutral b={}", cp.b);
    }

    #[test]
    fn coloradj_warm_boosts_red_cuts_blue() {
        let cp = compute_coloradj_params(3000.0, 0.0);
        assert!(
            cp.r > cp.b,
            "warm should boost R over B: r={} b={}",
            cp.r,
            cp.b
        );
        assert!(cp.b < 0.4, "warm should cut blue: b={}", cp.b);
    }

    #[test]
    fn coloradj_cool_boosts_blue_cuts_red() {
        let cp = compute_coloradj_params(10000.0, 0.0);
        assert!(
            cp.b > cp.r,
            "cool should boost B over R: r={} b={}",
            cp.r,
            cp.b
        );
        assert!(cp.r < 0.45, "cool should cut red: r={}", cp.r);
    }

    #[test]
    fn coloradj_tint_shifts_green() {
        let pos = compute_coloradj_params(6500.0, 0.5);
        let neg = compute_coloradj_params(6500.0, -0.5);
        assert!(
            pos.g < neg.g,
            "positive tint cuts green: pos_g={} neg_g={}",
            pos.g,
            neg.g
        );
        assert!(
            pos.r > neg.r,
            "positive tint boosts red: pos_r={} neg_r={}",
            pos.r,
            neg.r
        );
    }

    #[test]
    fn coloradj_params_clamped() {
        let cp = compute_coloradj_params(1000.0, 1.0);
        assert!(cp.r >= 0.0 && cp.r <= 1.0, "r clamped: {}", cp.r);
        assert!(cp.g >= 0.0 && cp.g <= 1.0, "g clamped: {}", cp.g);
        assert!(cp.b >= 0.0 && cp.b <= 1.0, "b clamped: {}", cp.b);
    }

    #[test]
    fn threepoint_positive_shadows_raises_gray() {
        let neutral = compute_3point_params(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let p = compute_3point_params(0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            p.gray_r > neutral.gray_r,
            "positive shadows should raise gray: gray_r={}",
            p.gray_r
        );
    }

    #[test]
    fn threepoint_all_channels_equal() {
        let p = compute_3point_params(0.5, -0.3, 0.7, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!((p.black_r - p.black_g).abs() < 1e-10, "R==G for black");
        assert!((p.black_r - p.black_b).abs() < 1e-10, "R==B for black");
        assert!((p.gray_r - p.gray_g).abs() < 1e-10, "R==G for gray");
        assert!((p.white_r - p.white_g).abs() < 1e-10, "R==G for white");
    }

    #[test]
    fn threepoint_params_clamped() {
        let p = compute_3point_params(1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            p.black_r >= 0.0 && p.black_r <= 0.8,
            "black clamped: {}",
            p.black_r
        );
        assert!(
            p.gray_r >= 0.05 && p.gray_r <= 0.95,
            "gray clamped: {}",
            p.gray_r
        );
        assert!(
            p.white_r >= 0.2 && p.white_r <= 1.0,
            "white clamped: {}",
            p.white_r
        );
    }

    #[test]
    fn apply_export_tonal_parity_gains_with_unity_preserves_inputs() {
        let (sh, mid, hi) = apply_export_tonal_parity_gains(0.7, -0.4, -0.6, 1.0, 1.0, 1.0);
        assert!((sh - 0.7).abs() < 1e-9);
        assert!((mid + 0.4).abs() < 1e-9);
        assert!((hi + 0.6).abs() < 1e-9);
    }

    #[test]
    fn apply_export_tonal_parity_gains_is_side_selective() {
        let (sh, mid, hi) = apply_export_tonal_parity_gains(0.8, -0.5, -0.4, 0.9, 0.92, 0.95);
        assert!((sh - 0.72).abs() < 1e-9);
        assert!((mid + 0.46).abs() < 1e-9);
        assert!((hi + 0.38).abs() < 1e-9);

        let (sh_pos, mid_pos, hi_pos) =
            apply_export_tonal_parity_gains(-0.8, 0.5, 0.4, 0.9, 0.92, 0.95);
        assert!(
            (sh_pos + 0.8).abs() < 1e-9,
            "negative shadows must be unchanged"
        );
        assert!(
            (mid_pos - 0.5).abs() < 1e-9,
            "positive midtones must be unchanged"
        );
        assert!(
            (hi_pos - 0.4).abs() < 1e-9,
            "positive highlights must be unchanged"
        );
    }

    #[test]
    fn export_temperature_parity_gain_is_cool_side_only() {
        assert!((export_temperature_parity_gain(6500.0) - 1.0).abs() < 1e-9);
        assert!((export_temperature_parity_gain(10000.0) - 1.0).abs() < 1e-9);
        assert!((export_temperature_parity_gain(2000.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn piecewise_cool_temperature_gain_interpolates_segments() {
        let far = 0.94;
        let near = 0.98;
        assert!((piecewise_cool_temperature_gain(2000.0, far, near) - far).abs() < 1e-9);
        assert!((piecewise_cool_temperature_gain(3500.0, far, near) - 0.96).abs() < 1e-9);
        assert!((piecewise_cool_temperature_gain(5000.0, far, near) - near).abs() < 1e-9);
        assert!((piecewise_cool_temperature_gain(5750.0, far, near) - 0.99).abs() < 1e-9);
        assert!((piecewise_cool_temperature_gain(6500.0, far, near) - 1.0).abs() < 1e-9);
        assert!((piecewise_cool_temperature_gain(10000.0, far, near) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn export_temperature_channel_offsets_zero_at_neutral() {
        let (r, g, b) = export_temperature_channel_offsets(6500.0);
        assert!(r.abs() < 1e-9, "r should be zero at neutral: {r}");
        assert!(g.abs() < 1e-9, "g should be zero at neutral: {g}");
        assert!(b.abs() < 1e-9, "b should be zero at neutral: {b}");
    }

    #[test]
    fn export_temperature_channel_offsets_at_extremes() {
        let (r2k, g2k, b2k) = export_temperature_channel_offsets(2000.0);
        assert!((r2k).abs() < 1e-9, "2000K: R should be zero: {r2k}");
        assert!((g2k).abs() < 1e-9, "2000K: G should be zero: {g2k}");
        assert!((b2k).abs() < 1e-9, "2000K: B should be zero: {b2k}");

        let (r10k, g10k, b10k) = export_temperature_channel_offsets(10000.0);
        assert!(
            b10k < -0.01,
            "10000K: B should be strongly negative: {b10k}"
        );
        assert!(r10k < 0.0, "10000K: R should be negative: {r10k}");
        assert!(
            b10k < r10k,
            "10000K: B offset should be larger than R: b={b10k} r={r10k}"
        );
    }

    #[test]
    fn export_3point_parity_offsets_is_noop() {
        let base = compute_3point_params(0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let mut adjusted = base.clone();
        apply_export_3point_parity_offsets(&mut adjusted, 0.5, 0.0, 0.0);
        assert!((adjusted.black_r - base.black_r).abs() < 1e-9);
        assert!((adjusted.gray_r - base.gray_r).abs() < 1e-9);
        assert!((adjusted.white_r - base.white_r).abs() < 1e-9);
    }

    #[test]
    fn tonal_axis_response_boosts_slider_ends_without_destabilizing_center() {
        let center = compute_tonal_axis_response(0.2);
        let end = compute_tonal_axis_response(1.0);
        assert!(
            (center - 0.2).abs() < 0.01,
            "center should remain precise: {}",
            center
        );
        assert!(
            end > 1.3,
            "ends should be substantially stronger than linear: {}",
            end
        );
    }

    #[test]
    fn threepoint_positive_shadows_warmth_produces_warm_red() {
        let p = compute_3point_params(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0);
        assert!(
            p.black_r < p.black_b,
            "positive shadows warmth: red control should be lower than blue: black_r={}, black_b={}",
            p.black_r, p.black_b
        );
    }

    #[test]
    fn threepoint_parabola_identity_at_neutral() {
        let p = compute_3point_params(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let para = ThreePointParabola::from_params(&p);
        for &x in &[0.0, 0.25, 0.5, 0.75, 1.0] {
            let yr = para.r.a * x * x + para.r.b * x + para.r.c;
            assert!(
                (yr - x).abs() < 0.05,
                "neutral red parabola at x={}: expected ~{}, got {}",
                x,
                x,
                yr
            );
        }
    }

    #[test]
    fn videobalance_rgb_identity_at_neutral() {
        let params = VBParams {
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            hue: 0.0,
        };
        let (r, g, b) = apply_videobalance_rgb(0.5, 0.3, 0.7, params);
        assert!((r - 0.5).abs() < 0.01, "r should be ~0.5: {r}");
        assert!((g - 0.3).abs() < 0.01, "g should be ~0.3: {g}");
        assert!((b - 0.7).abs() < 0.01, "b should be ~0.7: {b}");
    }

    #[test]
    fn coloradj_gain_conversion() {
        assert!((coloradj_param_to_gain(0.5) - 1.0).abs() < 1e-9);
        assert!((coloradj_param_to_gain(0.0) - 0.0).abs() < 1e-9);
        assert!((coloradj_param_to_gain(1.0) - 2.0).abs() < 1e-9);
    }
}
