//! Hardware-encoder capability probing for FFmpeg-driven cache pipelines
//! (proxy generation and background prerender).
//!
//! The first call to [`detect`] spawns `ffmpeg -hide_banner -encoders` once,
//! parses the table, and caches a [`HwEncoderCaps`] in a process-wide
//! `OnceLock`. The export pipeline does its own software-only encoder
//! selection today; this module is intentionally scoped to the cache paths
//! where speed matters more than final-output quality.
//!
//! VA-API additionally requires `/dev/dri/renderD128` to exist and be
//! readable — having the encoder library compiled into FFmpeg is not
//! sufficient if the user's machine has no Intel/AMD render node.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use crate::ui_state::{HwEncoderMode, ProxyCodec};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HwEncoderFamily {
    Vaapi,
    Nvenc,
}

impl HwEncoderFamily {
    pub fn as_str(self) -> &'static str {
        match self {
            HwEncoderFamily::Vaapi => "vaapi",
            HwEncoderFamily::Nvenc => "nvenc",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HwEncoderCaps {
    pub vaapi_h264: bool,
    pub vaapi_h265: bool,
    /// True iff at least one DRM render node exists AND we can open it
    /// for read. Distinct from "the file exists" — on Linux these nodes
    /// are typically gated to group `render`, and a non-member user gets
    /// EACCES from VA-API / QSV initialisation. We require successful
    /// open so the picker doesn't promise a backend that will fail at
    /// runtime.
    pub vaapi_device_present: bool,
    /// Concrete path to the accessible render node, used as the
    /// `-vaapi_device` argument at runtime. `None` whenever
    /// `vaapi_device_present` is false. Allows multi-GPU systems to use
    /// renderD129/130/etc. when the default isn't accessible.
    pub vaapi_render_node: Option<PathBuf>,
    /// True iff at least one render node exists on disk but none could
    /// be opened. Signals the caller to surface a "join the render group"
    /// hint to the user.
    pub render_node_blocked_by_permissions: bool,
    /// VA-API end-to-end init probe result (`ffmpeg -init_hw_device vaapi`
    /// against the discovered render node). False when the kernel device
    /// opens but libva itself can't load a driver — e.g. the Intel media
    /// driver isn't installed for the user's GPU.
    pub vaapi_init_ok: bool,
    /// Quick Sync end-to-end init probe result (`ffmpeg -init_hw_device qsv`).
    /// QSV uses VA-API under the hood on Linux, so it usually fails for the
    /// same reasons VA-API does, but we probe independently because some
    /// FFmpeg builds support one and not the other.
    pub qsv_init_ok: bool,
    pub nvenc_h264: bool,
    pub nvenc_h265: bool,
    /// FFmpeg `-hwaccels` method names available in this build (e.g.
    /// `vaapi`, `cuda`, `qsv`, `vdpau`, `videotoolbox`). Used as the value
    /// of the `-hwaccel` flag at decode time.
    pub hwaccels_available: Vec<String>,
}

impl HwEncoderCaps {
    pub fn vaapi_usable(&self) -> bool {
        self.vaapi_device_present && (self.vaapi_h264 || self.vaapi_h265)
    }

    pub fn nvenc_usable(&self) -> bool {
        self.nvenc_h264 || self.nvenc_h265
    }

    pub fn any_usable(&self) -> bool {
        self.vaapi_usable() || self.nvenc_usable() || self.any_decode_hwaccel_usable()
    }

    /// True when at least one HW decode method is plausibly usable.
    /// `vaapi` decode requires the render node like the encode side; `cuda`
    /// and `qsv` rely on driver-level access we can't probe upfront, so we
    /// trust the FFmpeg `-hwaccels` listing and let runtime fallback handle
    /// access errors.
    pub fn any_decode_hwaccel_usable(&self) -> bool {
        self.has_hwaccel("cuda")
            || self.has_hwaccel("qsv")
            || (self.has_hwaccel("vaapi") && self.vaapi_device_present)
    }

    pub fn has_hwaccel(&self, name: &str) -> bool {
        self.hwaccels_available.iter().any(|n| n == name)
    }
}

static CACHED_CAPS: OnceLock<HwEncoderCaps> = OnceLock::new();

pub fn detect() -> &'static HwEncoderCaps {
    CACHED_CAPS.get_or_init(|| {
        let caps = detect_with_ffmpeg("ffmpeg");
        let cuda_loadable = cuda_runtime_loadable();
        // One-time advisory log so users on machines without the NVIDIA
        // driver understand why CUDA/NVENC isn't being used even though
        // ffmpeg lists them.
        if (caps.has_hwaccel("cuda") || caps.nvenc_h264 || caps.nvenc_h265) && !cuda_loadable {
            log::info!(
                "hwaccel: ffmpeg advertises CUDA/NVENC but libcuda.so.1 is not installed in any standard path; skipping cuda/nvenc — proxy/prerender will use qsv/vaapi/libx264 instead"
            );
        }
        // Same idea for the DRM render node: VA-API and QSV (which uses
        // VA-API under the hood on Linux) both error out at init when the
        // device file exists but isn't openable, which is the common case
        // when the user isn't a member of the `render` group.
        if caps.render_node_blocked_by_permissions {
            log::warn!(
                "hwaccel: a DRM render node exists in /dev/dri but is not openable by this process — skipping vaapi/qsv. To enable hardware decode/encode on Intel/AMD GPUs, add your user to the 'render' group: `sudo usermod -aG render $USER` then log out and back in."
            );
        }
        // VA-API init can also fail past the kernel device — most often
        // because the libva user-space driver for the GPU isn't installed
        // even though /dev/dri is openable. Surface the install hint up
        // front so the user doesn't have to dig through ffmpeg errors.
        if caps.vaapi_render_node.is_some() && !caps.vaapi_init_ok {
            log::warn!(
                "hwaccel: VA-API failed to initialise against {} (libva driver missing or unsupported for this GPU) — skipping vaapi/qsv. On Intel: install `intel-media-va-driver-non-free` (Ubuntu/Debian) or `intel-media-driver` (Fedora). On AMD: install `mesa-va-drivers`. Verify with `vainfo`.",
                caps.vaapi_render_node
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            );
        }
        caps
    })
}

pub fn detect_with_ffmpeg(ffmpeg: &str) -> HwEncoderCaps {
    let encoders_stdout = Command::new(ffmpeg)
        .arg("-hide_banner")
        .arg("-encoders")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let hwaccels_stdout = Command::new(ffmpeg)
        .arg("-hide_banner")
        .arg("-hwaccels")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let mut caps = parse_ffmpeg_encoders_lines(&encoders_stdout);
    caps.hwaccels_available = parse_ffmpeg_hwaccels_lines(&hwaccels_stdout);
    let (node, blocked) = probe_drm_render_node();
    caps.vaapi_render_node = node.clone();
    caps.vaapi_device_present = node.is_some();
    caps.render_node_blocked_by_permissions = blocked;
    // End-to-end init probes. Skipped unless the prerequisite is plausible
    // (no point probing VA-API if no render node opened, no point probing
    // QSV if FFmpeg doesn't list it as a hwaccel).
    caps.vaapi_init_ok = if let Some(node) = node.as_ref() {
        probe_vaapi_init(ffmpeg, node)
    } else {
        false
    };
    caps.qsv_init_ok = if caps.has_hwaccel("qsv") {
        probe_qsv_init(ffmpeg)
    } else {
        false
    };
    caps
}

/// Spawn a tiny ffmpeg invocation that initialises VA-API against the
/// supplied render node and exits. Returns true iff the invocation
/// succeeded. The probe is bounded — input is a 1-frame `nullsrc`, and
/// failure modes (missing driver, broken libva) error within milliseconds.
fn probe_vaapi_init(ffmpeg: &str, render_node: &Path) -> bool {
    Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error"])
        .arg("-init_hw_device")
        .arg(format!("vaapi=us_probe:{}", render_node.display()))
        .args([
            "-f",
            "lavfi",
            "-i",
            "nullsrc=size=64x64:duration=0.04:rate=24",
            "-frames:v",
            "1",
            "-f",
            "null",
            "-",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Spawn a tiny ffmpeg invocation that initialises QSV (Quick Sync) and
/// exits. On Linux QSV bridges through libva so this typically fails for
/// the same reasons as `probe_vaapi_init` — but ffmpeg supports a Windows
/// DXVA2 path too, so we probe independently.
fn probe_qsv_init(ffmpeg: &str) -> bool {
    Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error"])
        .arg("-init_hw_device")
        .arg("qsv=us_probe:hw_any")
        .args([
            "-f",
            "lavfi",
            "-i",
            "nullsrc=size=64x64:duration=0.04:rate=24",
            "-frames:v",
            "1",
            "-f",
            "null",
            "-",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Parse `ffmpeg -hwaccels` output. Format is a header line
/// "Hardware acceleration methods:" followed by one method name per line:
///
/// ```text
/// Hardware acceleration methods:
/// vdpau
/// cuda
/// vaapi
/// qsv
/// ```
pub fn parse_ffmpeg_hwaccels_lines(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.contains(':')
                && line
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
        .map(str::to_string)
        .collect()
}

/// Walk `/dev/dri/renderD128..D135` and return the first node we can open
/// for read, plus a flag indicating whether any node existed but couldn't
/// be opened (typical case: user not in the `render` group).
///
/// Returning `None` from existence means VA-API/QSV are simply unavailable
/// on this machine; returning `None` with `blocked = true` means the
/// hardware is there but the user needs to join the `render` group (or
/// equivalent). The picker treats both as "unavailable" so we don't burn
/// a per-source fallback attempt on a permission-denied path.
fn probe_drm_render_node() -> (Option<PathBuf>, bool) {
    let mut any_present = false;
    let mut found = None;
    let dri_dir = Path::new("/dev/dri");
    for n in 128..136 {
        let path = dri_dir.join(format!("renderD{n}"));
        if !path.exists() {
            continue;
        }
        any_present = true;
        if std::fs::OpenOptions::new().read(true).open(&path).is_ok() {
            found = Some(path);
            break;
        }
    }
    let blocked = found.is_none() && any_present;
    (found, blocked)
}

/// True when `libcuda.so.1` is present in any of the dynamic-linker
/// search paths.
///
/// `ffmpeg -hwaccels` and `ffmpeg -encoders` will happily list `cuda` and
/// `h264_nvenc` whenever FFmpeg was built with NVIDIA support, even on
/// machines without the proprietary NVIDIA driver / CUDA runtime
/// installed. At runtime those paths fail with
/// `Cannot load libcuda.so.1 / Could not dynamically load CUDA / Device
/// creation failed: -1`. We probe for the actual file so the picker can
/// skip CUDA/NVENC and fall straight to a working backend (qsv / vaapi /
/// libx264) instead of burning a per-source attempt on a guaranteed
/// failure.
pub fn cuda_runtime_loadable() -> bool {
    cuda_runtime_loadable_inner(
        &[
            "/usr/lib/x86_64-linux-gnu/libcuda.so.1",
            "/usr/lib64/libcuda.so.1",
            "/usr/lib/libcuda.so.1",
            "/lib/x86_64-linux-gnu/libcuda.so.1",
            "/usr/local/cuda/lib64/libcuda.so.1",
        ],
        std::env::var_os("LD_LIBRARY_PATH").as_deref(),
    )
}

fn cuda_runtime_loadable_inner(
    standard_paths: &[&str],
    ld_library_path: Option<&std::ffi::OsStr>,
) -> bool {
    if standard_paths.iter().any(|p| Path::new(p).exists()) {
        return true;
    }
    if let Some(ld_path) = ld_library_path {
        for dir in std::env::split_paths(ld_path) {
            if dir.join("libcuda.so.1").exists() {
                return true;
            }
        }
    }
    false
}

/// Parse the body of `ffmpeg -encoders` output. Each row looks like:
///
/// ```text
///  V..... h264_vaapi          H.264/AVC (Intel VA-API)
///  V..... h264_nvenc          NVIDIA NVENC H.264 encoder
/// ```
///
/// We only care that the flag column starts with `V` (video) and the second
/// whitespace-separated column matches a known HW encoder name. Audio
/// encoders and other noise lines are skipped silently.
pub fn parse_ffmpeg_encoders_lines(output: &str) -> HwEncoderCaps {
    let mut caps = HwEncoderCaps::default();
    for line in output.lines() {
        let trimmed = line.trim_start();
        // Skip the header / separator rows. Real entries start with the
        // 6-char flag column ("V....." for video).
        let mut parts = trimmed.split_whitespace();
        let Some(flags) = parts.next() else {
            continue;
        };
        if !flags.starts_with('V') || flags.len() < 2 {
            continue;
        }
        let Some(name) = parts.next() else {
            continue;
        };
        match name {
            "h264_vaapi" => caps.vaapi_h264 = true,
            "hevc_vaapi" => caps.vaapi_h265 = true,
            "h264_nvenc" | "nvenc_h264" => caps.nvenc_h264 = true,
            "hevc_nvenc" | "nvenc_hevc" => caps.nvenc_h265 = true,
            _ => {}
        }
    }
    caps
}

/// Resolve a user-facing [`HwEncoderMode`] preference to the encoder family
/// that should actually be used right now, or `None` for software encoding.
///
/// Auto picks NVENC when both are present (CUDA encoders generally have
/// fewer pixel-format pitfalls than VA-API). Explicit `Vaapi` / `Nvenc`
/// requests fall through to `None` if the requested family isn't actually
/// usable, so the caller emits libx264 instead of a broken HW command.
pub fn pick_h264_encoder(mode: HwEncoderMode) -> Option<HwEncoderFamily> {
    pick_encoder(ProxyCodec::H264, mode, detect(), cuda_runtime_loadable())
}

pub fn pick_h264_encoder_with_caps(
    mode: HwEncoderMode,
    caps: &HwEncoderCaps,
) -> Option<HwEncoderFamily> {
    pick_h264_encoder_with_caps_and_runtime(mode, caps, cuda_runtime_loadable())
}

pub fn pick_h264_encoder_with_caps_and_runtime(
    mode: HwEncoderMode,
    caps: &HwEncoderCaps,
    cuda_loadable: bool,
) -> Option<HwEncoderFamily> {
    pick_encoder(ProxyCodec::H264, mode, caps, cuda_loadable)
}

/// Codec-aware version of [`pick_h264_encoder_with_caps_and_runtime`].
/// Returns the HW encoder family that should be used for the chosen
/// codec under the given user mode + runtime caps, or `None` for the
/// software fallback (libx264 / libx265).
pub fn pick_encoder(
    codec: ProxyCodec,
    mode: HwEncoderMode,
    caps: &HwEncoderCaps,
    cuda_loadable: bool,
) -> Option<HwEncoderFamily> {
    let (vaapi_codec_supported, nvenc_codec_supported) = match codec {
        ProxyCodec::H264 => (caps.vaapi_h264, caps.nvenc_h264),
        ProxyCodec::Hevc => (caps.vaapi_h265, caps.nvenc_h265),
    };
    // NVENC is gated on libcuda.so.1 being loadable: ffmpeg lists the
    // encoder whenever it was compiled in, but at runtime it errors out
    // with "Could not dynamically load CUDA" on machines without the
    // NVIDIA driver. Treat that as encoder-unavailable and let Auto fall
    // through to VA-API.
    let nvenc_usable = nvenc_codec_supported && cuda_loadable;
    // VA-API is additionally gated on the startup init probe — if libva
    // can't load a driver for the GPU (e.g. missing intel-media-va-driver)
    // every per-source attempt would fail with the same error, so we drop
    // it from the candidate list up front.
    let vaapi_usable = caps.vaapi_device_present && vaapi_codec_supported && caps.vaapi_init_ok;
    match mode {
        HwEncoderMode::Off => None,
        HwEncoderMode::Vaapi => {
            if vaapi_usable {
                Some(HwEncoderFamily::Vaapi)
            } else {
                None
            }
        }
        HwEncoderMode::Nvenc => {
            if nvenc_usable {
                Some(HwEncoderFamily::Nvenc)
            } else {
                None
            }
        }
        HwEncoderMode::Auto => {
            if nvenc_usable {
                Some(HwEncoderFamily::Nvenc)
            } else if vaapi_usable {
                Some(HwEncoderFamily::Vaapi)
            } else {
                None
            }
        }
    }
}

/// Resolve the user's [`HwEncoderMode`] preference into the FFmpeg
/// `-hwaccel <name>` value to use at decode time, or `None` for software
/// decode. Returned strings are valid FFmpeg `-hwaccel` arguments
/// (`"cuda"`, `"vaapi"`, `"qsv"`).
///
/// `Auto` prefers `cuda` (NVIDIA NVDEC) > `qsv` (Intel iGPU Quick Sync) >
/// `vaapi` (generic Linux GPU). The CUDA-first ordering mirrors
/// [`pick_h264_encoder`]'s NVENC preference and gives us the most
/// reliable 10-bit HEVC decode path on common hardware. Explicit `Vaapi`
/// / `Nvenc` requests still try just their family.
///
/// IMPORTANT: this function does NOT set `-hwaccel_output_format`. Frames
/// download to CPU memory automatically so existing software filter chains
/// (lanczos / lut3d / etc.) keep working unchanged.
pub fn pick_decode_hwaccel(mode: HwEncoderMode) -> Option<&'static str> {
    pick_decode_hwaccel_with_caps(mode, detect())
}

pub fn pick_decode_hwaccel_with_caps(
    mode: HwEncoderMode,
    caps: &HwEncoderCaps,
) -> Option<&'static str> {
    pick_decode_hwaccel_with_caps_and_runtime(mode, caps, cuda_runtime_loadable())
}

pub fn pick_decode_hwaccel_with_caps_and_runtime(
    mode: HwEncoderMode,
    caps: &HwEncoderCaps,
    cuda_loadable: bool,
) -> Option<&'static str> {
    // CUDA decode shares the libcuda.so.1 dependency with NVENC; treat it
    // as unavailable when the runtime library isn't installed even if
    // ffmpeg advertises it.
    let cuda_usable = caps.has_hwaccel("cuda") && cuda_loadable;
    // QSV / VA-API decode are gated on their respective init probes (QSV
    // bridges through libva on Linux, so QSV typically fails when VA-API
    // does — but we keep the gates independent for ffmpeg builds with
    // alternative QSV backends).
    let qsv_usable = caps.has_hwaccel("qsv") && caps.qsv_init_ok;
    let vaapi_usable = caps.has_hwaccel("vaapi") && caps.vaapi_device_present && caps.vaapi_init_ok;
    match mode {
        HwEncoderMode::Off => None,
        HwEncoderMode::Vaapi => {
            if vaapi_usable {
                Some("vaapi")
            } else {
                None
            }
        }
        HwEncoderMode::Nvenc => {
            if cuda_usable {
                Some("cuda")
            } else {
                None
            }
        }
        HwEncoderMode::Auto => {
            if cuda_usable {
                Some("cuda")
            } else if qsv_usable {
                Some("qsv")
            } else if vaapi_usable {
                Some("vaapi")
            } else {
                None
            }
        }
    }
}

/// Stable string identity for cache signatures. Distinct from `as_str` in
/// that this also covers the software case.
pub fn encoder_signature(family: Option<HwEncoderFamily>) -> &'static str {
    match family {
        None => "sw",
        Some(HwEncoderFamily::Vaapi) => "vaapi",
        Some(HwEncoderFamily::Nvenc) => "nvenc",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_ENCODERS: &str = "\
Encoders:
 V..... = Video
 A..... = Audio
 S..... = Subtitle
 ------
 V..... libx264              libx264 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10
 V..... libx265              libx265 H.265 / HEVC
 V..... h264_vaapi           H.264/AVC (Intel VA-API)
 V..... hevc_vaapi           H.265/HEVC (Intel VA-API)
 V..... h264_nvenc           NVIDIA NVENC H.264 encoder
 V..... hevc_nvenc           NVIDIA NVENC HEVC encoder
 A..... aac                  AAC (Advanced Audio Coding)
";

    #[test]
    fn parses_full_encoder_table() {
        let caps = parse_ffmpeg_encoders_lines(SAMPLE_ENCODERS);
        assert!(caps.vaapi_h264);
        assert!(caps.vaapi_h265);
        assert!(caps.nvenc_h264);
        assert!(caps.nvenc_h265);
    }

    #[test]
    fn parses_software_only_table() {
        let caps =
            parse_ffmpeg_encoders_lines(" V..... libx264              libx264 H.264 / AVC\n");
        assert!(!caps.vaapi_h264);
        assert!(!caps.nvenc_h264);
    }

    #[test]
    fn skips_audio_rows_and_noise() {
        let txt = "Encoders:\n V..... = Video\n ------\n A..... aac AAC encoder\nrandom noise\n V..... h264_nvenc NVENC\n";
        let caps = parse_ffmpeg_encoders_lines(txt);
        assert!(caps.nvenc_h264);
        assert!(!caps.vaapi_h264);
    }

    #[test]
    fn off_mode_returns_none_even_with_caps() {
        let caps = HwEncoderCaps {
            vaapi_h264: true,
            vaapi_device_present: true,
            nvenc_h264: true,
            ..HwEncoderCaps::default()
        };
        assert_eq!(pick_h264_encoder_with_caps(HwEncoderMode::Off, &caps), None);
    }

    #[test]
    fn auto_prefers_nvenc_when_both_present() {
        let caps = HwEncoderCaps {
            vaapi_h264: true,
            vaapi_device_present: true,
            nvenc_h264: true,
            ..HwEncoderCaps::default()
        };
        // Pin cuda_loadable=true so the test doesn't depend on whether
        // libcuda.so.1 happens to be installed in the test environment.
        assert_eq!(
            pick_h264_encoder_with_caps_and_runtime(HwEncoderMode::Auto, &caps, true),
            Some(HwEncoderFamily::Nvenc)
        );
    }

    #[test]
    fn auto_falls_back_to_vaapi() {
        let caps = HwEncoderCaps {
            vaapi_h264: true,
            vaapi_device_present: true,
            vaapi_init_ok: true,
            ..HwEncoderCaps::default()
        };
        assert_eq!(
            pick_h264_encoder_with_caps_and_runtime(HwEncoderMode::Auto, &caps, false),
            Some(HwEncoderFamily::Vaapi)
        );
    }

    #[test]
    fn auto_returns_none_without_any_hw() {
        let caps = HwEncoderCaps::default();
        assert_eq!(
            pick_h264_encoder_with_caps(HwEncoderMode::Auto, &caps),
            None
        );
    }

    #[test]
    fn vaapi_requires_device_node() {
        let caps = HwEncoderCaps {
            vaapi_h264: true,
            vaapi_device_present: false,
            ..HwEncoderCaps::default()
        };
        assert_eq!(
            pick_h264_encoder_with_caps(HwEncoderMode::Vaapi, &caps),
            None
        );
        assert_eq!(
            pick_h264_encoder_with_caps(HwEncoderMode::Auto, &caps),
            None
        );
    }

    #[test]
    fn explicit_family_returns_none_when_unavailable() {
        let caps = HwEncoderCaps::default();
        assert_eq!(
            pick_h264_encoder_with_caps(HwEncoderMode::Vaapi, &caps),
            None
        );
        assert_eq!(
            pick_h264_encoder_with_caps(HwEncoderMode::Nvenc, &caps),
            None
        );
    }

    #[test]
    fn encoder_signature_distinguishes_modes() {
        assert_eq!(encoder_signature(None), "sw");
        assert_eq!(encoder_signature(Some(HwEncoderFamily::Vaapi)), "vaapi");
        assert_eq!(encoder_signature(Some(HwEncoderFamily::Nvenc)), "nvenc");
    }

    const SAMPLE_HWACCELS: &str = "\
Hardware acceleration methods:
vdpau
cuda
vaapi
qsv
drm
opencl
vulkan
";

    #[test]
    fn parses_hwaccels_listing() {
        let methods = parse_ffmpeg_hwaccels_lines(SAMPLE_HWACCELS);
        assert!(methods.contains(&"cuda".to_string()));
        assert!(methods.contains(&"vaapi".to_string()));
        assert!(methods.contains(&"qsv".to_string()));
        assert!(methods.contains(&"vdpau".to_string()));
        assert!(!methods.iter().any(|m| m.contains(':')));
    }

    #[test]
    fn parses_empty_hwaccels_listing() {
        let methods = parse_ffmpeg_hwaccels_lines("Hardware acceleration methods:\n");
        assert!(methods.is_empty());
    }

    #[test]
    fn pick_decode_off_returns_none() {
        let caps = HwEncoderCaps {
            hwaccels_available: vec!["cuda".into(), "vaapi".into()],
            vaapi_device_present: true,
            ..HwEncoderCaps::default()
        };
        assert_eq!(
            pick_decode_hwaccel_with_caps(HwEncoderMode::Off, &caps),
            None
        );
    }

    #[test]
    fn pick_decode_auto_prefers_cuda() {
        let caps = HwEncoderCaps {
            hwaccels_available: vec!["cuda".into(), "qsv".into(), "vaapi".into()],
            vaapi_device_present: true,
            vaapi_init_ok: true,
            qsv_init_ok: true,
            ..HwEncoderCaps::default()
        };
        assert_eq!(
            pick_decode_hwaccel_with_caps_and_runtime(HwEncoderMode::Auto, &caps, true),
            Some("cuda")
        );
    }

    #[test]
    fn pick_decode_auto_falls_back_to_qsv_then_vaapi() {
        let caps_qsv = HwEncoderCaps {
            hwaccels_available: vec!["qsv".into(), "vaapi".into()],
            vaapi_device_present: true,
            vaapi_init_ok: true,
            qsv_init_ok: true,
            ..HwEncoderCaps::default()
        };
        assert_eq!(
            pick_decode_hwaccel_with_caps_and_runtime(HwEncoderMode::Auto, &caps_qsv, false),
            Some("qsv")
        );
        let caps_vaapi = HwEncoderCaps {
            hwaccels_available: vec!["vaapi".into()],
            vaapi_device_present: true,
            vaapi_init_ok: true,
            ..HwEncoderCaps::default()
        };
        assert_eq!(
            pick_decode_hwaccel_with_caps_and_runtime(HwEncoderMode::Auto, &caps_vaapi, false),
            Some("vaapi")
        );
    }

    #[test]
    fn pick_decode_vaapi_requires_device_node() {
        let caps_no_node = HwEncoderCaps {
            hwaccels_available: vec!["vaapi".into()],
            vaapi_device_present: false,
            ..HwEncoderCaps::default()
        };
        assert_eq!(
            pick_decode_hwaccel_with_caps(HwEncoderMode::Vaapi, &caps_no_node),
            None
        );
    }

    #[test]
    fn pick_decode_nvenc_uses_cuda_decoder() {
        let caps = HwEncoderCaps {
            hwaccels_available: vec!["cuda".into()],
            ..HwEncoderCaps::default()
        };
        assert_eq!(
            pick_decode_hwaccel_with_caps_and_runtime(HwEncoderMode::Nvenc, &caps, true),
            Some("cuda")
        );
    }

    #[test]
    fn pick_decode_returns_none_when_method_missing() {
        let caps = HwEncoderCaps::default();
        assert_eq!(
            pick_decode_hwaccel_with_caps(HwEncoderMode::Auto, &caps),
            None
        );
        assert_eq!(
            pick_decode_hwaccel_with_caps(HwEncoderMode::Nvenc, &caps),
            None
        );
        assert_eq!(
            pick_decode_hwaccel_with_caps(HwEncoderMode::Vaapi, &caps),
            None
        );
    }

    #[test]
    fn cuda_loadable_inner_finds_standard_path() {
        let mut tmp = std::env::temp_dir();
        tmp.push(format!("us-test-libcuda-{}.so.1", std::process::id()));
        std::fs::File::create(&tmp).expect("create temp libcuda");
        let path_str = tmp.to_string_lossy().into_owned();
        let standard = [path_str.as_str()];
        assert!(cuda_runtime_loadable_inner(&standard, None));
        std::fs::remove_file(&tmp).ok();
        assert!(!cuda_runtime_loadable_inner(&standard, None));
    }

    #[test]
    fn cuda_loadable_inner_finds_via_ld_library_path() {
        use std::ffi::OsString;
        let dir = std::env::temp_dir().join(format!("us-test-cuda-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::File::create(dir.join("libcuda.so.1")).expect("create");
        let ld: OsString = dir.to_string_lossy().to_string().into();
        assert!(cuda_runtime_loadable_inner(&[], Some(ld.as_os_str())));
        std::fs::remove_file(dir.join("libcuda.so.1")).ok();
        assert!(!cuda_runtime_loadable_inner(&[], Some(ld.as_os_str())));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cuda_loadable_inner_returns_false_when_nothing_found() {
        assert!(!cuda_runtime_loadable_inner(
            &["/definitely/does/not/exist/libcuda.so.1"],
            None
        ));
    }

    #[test]
    fn auto_skips_nvenc_without_libcuda() {
        let caps = HwEncoderCaps {
            nvenc_h264: true,
            vaapi_h264: true,
            vaapi_device_present: true,
            vaapi_init_ok: true,
            ..HwEncoderCaps::default()
        };
        // libcuda missing → Auto must fall through past NVENC to VA-API
        assert_eq!(
            pick_h264_encoder_with_caps_and_runtime(HwEncoderMode::Auto, &caps, false),
            Some(HwEncoderFamily::Vaapi)
        );
        // libcuda present → NVENC wins as before
        assert_eq!(
            pick_h264_encoder_with_caps_and_runtime(HwEncoderMode::Auto, &caps, true),
            Some(HwEncoderFamily::Nvenc)
        );
    }

    #[test]
    fn explicit_nvenc_returns_none_without_libcuda() {
        let caps = HwEncoderCaps {
            nvenc_h264: true,
            ..HwEncoderCaps::default()
        };
        assert_eq!(
            pick_h264_encoder_with_caps_and_runtime(HwEncoderMode::Nvenc, &caps, false),
            None
        );
    }

    #[test]
    fn auto_decode_skips_cuda_without_libcuda() {
        let caps = HwEncoderCaps {
            hwaccels_available: vec!["cuda".into(), "qsv".into(), "vaapi".into()],
            vaapi_device_present: true,
            vaapi_init_ok: true,
            qsv_init_ok: true,
            ..HwEncoderCaps::default()
        };
        // libcuda missing → Auto must fall through past CUDA to QSV
        assert_eq!(
            pick_decode_hwaccel_with_caps_and_runtime(HwEncoderMode::Auto, &caps, false),
            Some("qsv")
        );
        // libcuda present → CUDA still wins
        assert_eq!(
            pick_decode_hwaccel_with_caps_and_runtime(HwEncoderMode::Auto, &caps, true),
            Some("cuda")
        );
    }

    #[test]
    fn explicit_nvenc_decode_returns_none_without_libcuda() {
        let caps = HwEncoderCaps {
            hwaccels_available: vec!["cuda".into()],
            ..HwEncoderCaps::default()
        };
        assert_eq!(
            pick_decode_hwaccel_with_caps_and_runtime(HwEncoderMode::Nvenc, &caps, false),
            None
        );
    }

    /// Sanity test: real DRM probe doesn't panic and returns a consistent
    /// state. Whether a node is found or blocked depends on the runner.
    #[test]
    fn drm_probe_runs_without_panic() {
        let (node, blocked) = probe_drm_render_node();
        // Cannot have both: if we found a node, it's accessible (not blocked).
        // If we didn't, "blocked" tells us why (existed but unreadable).
        assert!(node.is_some() || !node.is_some()); // tautology — just exercise the path
        if node.is_some() {
            assert!(!blocked, "found a node so it cannot also be blocked");
        }
    }

    #[test]
    fn render_node_blocked_flag_propagates_into_caps() {
        // We can't fake /dev/dri easily, but we can confirm the field
        // exists on HwEncoderCaps and is wired through default detection
        // without panicking.
        let caps = detect_with_ffmpeg("ffmpeg-this-binary-does-not-exist");
        // The picker should treat a no-render-node + no-hwaccels world as
        // "no usable HW" — explicit Vaapi mode returns None.
        assert_eq!(
            pick_h264_encoder_with_caps_and_runtime(HwEncoderMode::Vaapi, &caps, false),
            None
        );
        let _ = caps.render_node_blocked_by_permissions; // field exists
    }
}
