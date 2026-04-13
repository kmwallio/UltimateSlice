// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared AI execution-provider infrastructure for all ONNX-backed
//! caches (`bg_removal_cache`, `frame_interp_cache`, `music_gen`, and
//! — eventually — `sam_cache`).
//!
//! This module centralizes two concerns:
//!
//! 1. **Detection.** Report which ONNX Runtime execution providers
//!    were compiled into this binary *and* which ones are actually
//!    usable at runtime on the current machine. CPU is always both.
//!    CUDA / ROCm / OpenVINO are compile-time gated behind the
//!    `ai-cuda` / `ai-rocm` / `ai-openvino` Cargo features and also
//!    runtime-gated by whether the corresponding driver / runtime
//!    library is actually installed.
//!
//! 2. **Configuration.** Apply the user's preferred backend (or auto-
//!    select) to a fresh `SessionBuilder`. ort tries providers in the
//!    order they're registered and silently falls back to the CPU
//!    provider if none of the requested ones can load, so
//!    `configure_session_builder` never fails just because a GPU is
//!    unavailable — it produces a working builder on every machine.
//!
//! A process-wide atomic holds the currently-preferred backend so the
//! existing cache workers (which each create their own session on a
//! background thread) can read it without plumbing it through every
//! job struct.
//!
//! The whole module is gated behind the `ai-inference` feature. When
//! that feature is off, the crate doesn't pull in `ort` at all and
//! callers won't see this module's types either.

#![cfg(feature = "ai-inference")]

use std::sync::atomic::{AtomicU8, Ordering};

use ort::session::builder::SessionBuilder;

// ── Backend selection ──────────────────────────────────────────────────────

/// Which ONNX Runtime execution provider the user wants to use for
/// all AI inference across the app. `Auto` lets the runtime pick the
/// best compiled-in provider at session-creation time, falling back
/// to CPU if nothing else loads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiBackend {
    /// Try compiled-in GPU providers in priority order (native EPs
    /// first — CUDA → ROCm → OpenVINO — then the cross-vendor WebGPU
    /// path, finally CPU). Default.
    Auto,
    Cuda,
    Rocm,
    OpenVino,
    /// Cross-vendor GPU path via ONNX Runtime's WebGPU execution
    /// provider (Dawn → Vulkan / D3D12 / Metal). Works on Intel Arc,
    /// AMD, and NVIDIA without a vendor-specific SDK install. Lower
    /// peak throughput than the native EPs but zero friction.
    WebGpu,
    Cpu,
}

impl Default for AiBackend {
    fn default() -> Self {
        AiBackend::Auto
    }
}

impl AiBackend {
    /// Persistent string id used in `ui_state` / FCPXML / MCP. Keep
    /// stable — changing these breaks saved user preferences.
    pub fn as_id(self) -> &'static str {
        match self {
            AiBackend::Auto => "auto",
            AiBackend::Cuda => "cuda",
            AiBackend::Rocm => "rocm",
            AiBackend::OpenVino => "openvino",
            AiBackend::WebGpu => "webgpu",
            AiBackend::Cpu => "cpu",
        }
    }

    /// Inverse of `as_id`. Unknown ids map back to `Auto` rather than
    /// failing so that a preference file written by a newer build
    /// still loads on an older one.
    pub fn from_id(id: &str) -> AiBackend {
        match id {
            "cuda" => AiBackend::Cuda,
            "rocm" => AiBackend::Rocm,
            "openvino" => AiBackend::OpenVino,
            "webgpu" => AiBackend::WebGpu,
            "cpu" => AiBackend::Cpu,
            _ => AiBackend::Auto,
        }
    }

    /// User-facing label for the Preferences dropdown.
    pub fn label(self) -> &'static str {
        match self {
            AiBackend::Auto => "Auto (best available)",
            AiBackend::Cuda => "NVIDIA CUDA",
            AiBackend::Rocm => "AMD ROCm",
            AiBackend::OpenVino => "Intel OpenVINO",
            AiBackend::WebGpu => "WebGPU (cross-vendor)",
            AiBackend::Cpu => "CPU",
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            AiBackend::Auto => 0,
            AiBackend::Cuda => 1,
            AiBackend::Rocm => 2,
            AiBackend::OpenVino => 3,
            AiBackend::Cpu => 4,
            AiBackend::WebGpu => 5,
        }
    }

    fn from_u8(value: u8) -> AiBackend {
        match value {
            1 => AiBackend::Cuda,
            2 => AiBackend::Rocm,
            3 => AiBackend::OpenVino,
            4 => AiBackend::Cpu,
            5 => AiBackend::WebGpu,
            _ => AiBackend::Auto,
        }
    }
}

// ── Process-wide current backend ───────────────────────────────────────────

static CURRENT_BACKEND: AtomicU8 = AtomicU8::new(0); // Auto

/// Read the process-wide preferred backend. All cache workers call
/// this right before creating a `Session` so that changing the
/// preference in the UI takes effect on the next job without
/// restarting the app.
pub fn current_backend() -> AiBackend {
    AiBackend::from_u8(CURRENT_BACKEND.load(Ordering::Relaxed))
}

/// Update the process-wide preferred backend. Called from the
/// Preferences UI when the user changes the dropdown.
pub fn set_current_backend(backend: AiBackend) {
    CURRENT_BACKEND.store(backend.as_u8(), Ordering::Relaxed);
}

// ── Availability detection ─────────────────────────────────────────────────

/// Report of backend availability on this build + machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiBackendReport {
    /// Backends compiled into this binary (as gated by cargo
    /// features). Always includes `Cpu` — the ort CPU provider is
    /// built into the runtime unconditionally.
    pub compiled_in: Vec<AiBackend>,
    /// Backends that are both compiled in *and* report available at
    /// runtime (i.e. the required driver / runtime library could be
    /// loaded). Always includes `Cpu`.
    pub runtime_available: Vec<AiBackend>,
}

impl AiBackendReport {
    /// True if `backend` is compiled in AND runtime-available.
    pub fn can_use(&self, backend: AiBackend) -> bool {
        match backend {
            AiBackend::Auto => true, // Auto always works — it will pick CPU at worst.
            other => self.runtime_available.iter().any(|b| *b == other),
        }
    }

    /// Pretty one-line summary for logging / Preferences status row.
    pub fn describe(&self) -> String {
        if self.runtime_available.is_empty() {
            return "No AI execution providers available (including CPU)".to_string();
        }
        let parts: Vec<&'static str> = self.runtime_available.iter().map(|b| b.label()).collect();
        format!("Available: {}", parts.join(", "))
    }
}

/// Detect which providers this binary was compiled with and which
/// ones are actually usable on the current machine. Cheap — just
/// instantiates provider builder structs and asks them whether their
/// shared library could be loaded. Safe to call at startup from the
/// main thread.
pub fn detect_backends() -> AiBackendReport {
    let mut compiled_in: Vec<AiBackend> = Vec::new();
    let mut runtime_available: Vec<AiBackend> = Vec::new();

    // CUDA ───────────────────────────────────────────────────────────
    #[cfg(feature = "ai-cuda")]
    {
        use ort::execution_providers::{CUDAExecutionProvider, ExecutionProvider};
        compiled_in.push(AiBackend::Cuda);
        let ep = CUDAExecutionProvider::default();
        if ep.is_available().unwrap_or(false) {
            runtime_available.push(AiBackend::Cuda);
        }
    }

    // ROCm ───────────────────────────────────────────────────────────
    #[cfg(feature = "ai-rocm")]
    {
        use ort::execution_providers::{ExecutionProvider, ROCmExecutionProvider};
        compiled_in.push(AiBackend::Rocm);
        let ep = ROCmExecutionProvider::default();
        if ep.is_available().unwrap_or(false) {
            runtime_available.push(AiBackend::Rocm);
        }
    }

    // OpenVINO (Intel Arc / iGPU / CPU) ─────────────────────────────
    #[cfg(feature = "ai-openvino")]
    {
        use ort::execution_providers::{ExecutionProvider, OpenVINOExecutionProvider};
        compiled_in.push(AiBackend::OpenVino);
        let ep = OpenVINOExecutionProvider::default();
        if ep.is_available().unwrap_or(false) {
            runtime_available.push(AiBackend::OpenVino);
        }
    }

    // WebGPU (cross-vendor via Dawn → Vulkan / D3D12 / Metal) ────────
    #[cfg(feature = "ai-webgpu")]
    {
        use ort::execution_providers::{ExecutionProvider, WebGPUExecutionProvider};
        compiled_in.push(AiBackend::WebGpu);
        let ep = WebGPUExecutionProvider::default();
        if ep.is_available().unwrap_or(false) {
            runtime_available.push(AiBackend::WebGpu);
        }
    }

    // CPU ────────────────────────────────────────────────────────────
    // The CPU provider is always compiled into ONNX Runtime, so we
    // report it unconditionally — without even asking `is_available`,
    // which on some prebuilt ort variants returns `Ok(false)` for CPU
    // because the builtin CPU EP isn't tracked like the optional GPU
    // ones.
    compiled_in.push(AiBackend::Cpu);
    runtime_available.push(AiBackend::Cpu);

    AiBackendReport {
        compiled_in,
        runtime_available,
    }
}

// ── Session builder configuration ──────────────────────────────────────────

/// Apply the currently-preferred backend to a fresh `SessionBuilder`,
/// registering execution providers in the appropriate order. The
/// returned builder can then be `commit_from_file`'d to produce a
/// `Session`. If the preferred backend isn't compiled in or fails to
/// load at runtime, ort silently falls back to the next registered
/// provider — CPU is always registered last as a guaranteed fallback.
///
/// This function is deliberately infallible in terms of "backend
/// availability": if nothing else works, CPU does. It only returns
/// `Err` for genuine ort errors (e.g. builder construction failed for
/// reasons unrelated to providers).
pub fn configure_session_builder(
    builder: SessionBuilder,
    preferred: AiBackend,
) -> ort::Result<SessionBuilder> {
    let effective = if preferred == AiBackend::Auto {
        // Auto mode: try every compiled-in GPU provider in priority
        // order, then CPU. ort will pick whichever first succeeds at
        // runtime. We don't call `is_available` here because `ort`
        // itself already performs that check during registration and
        // skips providers that can't load — so registering a CUDA
        // provider on a CPU-only machine is harmless.
        None
    } else {
        Some(preferred)
    };

    // Providers are registered on the builder itself.
    // ort 2.0 uses `SessionBuilder::with_execution_providers` which
    // takes an iterable of `ExecutionProviderDispatch`.
    use ort::execution_providers::ExecutionProviderDispatch;
    let mut providers: Vec<ExecutionProviderDispatch> = Vec::new();

    match effective {
        // Explicit pick → register just that one (ort will fall back
        // to CPU on its own if the load fails).
        Some(AiBackend::Cuda) => {
            push_cuda(&mut providers);
        }
        Some(AiBackend::Rocm) => {
            push_rocm(&mut providers);
        }
        Some(AiBackend::OpenVino) => {
            push_openvino(&mut providers);
        }
        Some(AiBackend::WebGpu) => {
            push_webgpu(&mut providers);
        }
        Some(AiBackend::Cpu) => {
            push_cpu(&mut providers);
        }
        Some(AiBackend::Auto) | None => {
            // Auto ordering: native EPs first (NVIDIA → AMD → Intel),
            // then cross-vendor WebGPU, then CPU. Rationale: if a
            // user has both a native EP and WebGPU compiled in, the
            // native EP wins because it has vendor-specific
            // optimizations (cuDNN, MIOpen, OpenVINO graph
            // optimizations); WebGPU is the generic fallback above
            // CPU. ort quietly skips any provider that isn't compiled
            // in or can't load at runtime.
            push_cuda(&mut providers);
            push_rocm(&mut providers);
            push_openvino(&mut providers);
            push_webgpu(&mut providers);
            push_cpu(&mut providers);
        }
    }

    if providers.is_empty() {
        // No compiled-in providers matched. Fall through to the ort
        // default (CPU) by not calling `with_execution_providers` at
        // all — this keeps working on a bare `cargo build` with no
        // GPU features enabled.
        return Ok(builder);
    }

    // `with_execution_providers` in ort 2.0 returns
    // `Result<SessionBuilder, ort::Error<SessionBuilder>>` — the
    // error type carries the original builder back on failure for
    // recovery. We don't need that context at our level, so drop it
    // via `?` + `Ok(_)` which runs the `From` coercion to the plain
    // `ort::Error` alias returned by `ort::Result`.
    Ok(builder.with_execution_providers(providers)?)
}

// Per-backend helpers. Each is a no-op at compile time when the
// corresponding feature isn't enabled, so the auto-order block above
// doesn't need its own cfg tangle.

#[inline]
#[allow(unused_variables)]
fn push_cuda(providers: &mut Vec<ort::execution_providers::ExecutionProviderDispatch>) {
    #[cfg(feature = "ai-cuda")]
    {
        use ort::execution_providers::CUDAExecutionProvider;
        providers.push(CUDAExecutionProvider::default().build());
    }
}

#[inline]
#[allow(unused_variables)]
fn push_rocm(providers: &mut Vec<ort::execution_providers::ExecutionProviderDispatch>) {
    #[cfg(feature = "ai-rocm")]
    {
        use ort::execution_providers::ROCmExecutionProvider;
        providers.push(ROCmExecutionProvider::default().build());
    }
}

#[inline]
#[allow(unused_variables)]
fn push_openvino(providers: &mut Vec<ort::execution_providers::ExecutionProviderDispatch>) {
    #[cfg(feature = "ai-openvino")]
    {
        use ort::execution_providers::OpenVINOExecutionProvider;
        providers.push(OpenVINOExecutionProvider::default().build());
    }
}

#[inline]
#[allow(unused_variables)]
fn push_webgpu(providers: &mut Vec<ort::execution_providers::ExecutionProviderDispatch>) {
    #[cfg(feature = "ai-webgpu")]
    {
        use ort::execution_providers::WebGPUExecutionProvider;
        providers.push(WebGPUExecutionProvider::default().build());
    }
}

#[inline]
fn push_cpu(providers: &mut Vec<ort::execution_providers::ExecutionProviderDispatch>) {
    use ort::execution_providers::CPUExecutionProvider;
    providers.push(CPUExecutionProvider::default().build());
}

// ── WebGPU pre-warm (suppress Dawn's startup warnings) ─────────────────────
//
// ORT's WebGPU EP asks Dawn for a device with
// `max*PerPipelineLayout = 500000` (a sentinel for "as many as
// possible"). Dawn's Vulkan backend can only supply 16 per layout —
// both the WebGPU spec minimum and the actual hardware limit on
// every driver tested — so it silently clamps the request and
// prints two warnings to stderr directly from C++:
//
//   Warning: maxDynamicUniformBuffersPerPipelineLayout artificially
//     reduced from 500000 to 16 to fit dynamic offset allocation limit.
//   Warning: maxDynamicStorageBuffersPerPipelineLayout artificially
//     reduced from 500000 to 16 to fit dynamic offset allocation limit.
//
// These are cosmetic (MusicGen, SAM, MODNet, RIFE all work fine
// with 16 dynamic buffers — none of their graphs need more) and
// fire exactly once at first Dawn device creation; ORT caches the
// device in its environment singleton across sessions.
//
// Dawn has no documented env var, toggle, or log callback to
// suppress these — we checked the Dawn debugging docs + toggle
// list. The messages are printed directly from
// `native/Device.cpp::AdjustLimitsForRequiredLimits` and bypass
// every log/tracing framework we can configure from Rust.
//
// So we do the next best thing: pre-trigger Dawn device creation
// at app startup with stderr temporarily redirected to /dev/null,
// so the warnings get emitted during the silent splash instead of
// interleaving with user output during the first real inference.
// Subsequent real session creation inherits the already-warmed
// device and prints nothing.

/// RAII guard that temporarily redirects stderr (fd 2) to
/// `/dev/null` for the lifetime of the guard, then restores the
/// original fd on drop. Used to swallow Dawn's device-creation
/// warnings without affecting any other stderr output.
///
/// Unix-only. On non-Unix platforms the guard is a no-op and the
/// pre-warm proceeds without redirection.
#[cfg(all(feature = "ai-webgpu", unix))]
struct StderrSilencer {
    saved_fd: libc::c_int,
}

#[cfg(all(feature = "ai-webgpu", unix))]
impl StderrSilencer {
    fn new() -> Option<Self> {
        unsafe {
            // 1. Save the current stderr fd so we can restore it on drop.
            let saved_fd = libc::dup(libc::STDERR_FILENO);
            if saved_fd < 0 {
                return None;
            }
            // 2. Open /dev/null for writing.
            let devnull_path = b"/dev/null\0".as_ptr() as *const libc::c_char;
            let devnull = libc::open(devnull_path, libc::O_WRONLY);
            if devnull < 0 {
                libc::close(saved_fd);
                return None;
            }
            // 3. Point fd 2 at /dev/null. `dup2` closes the existing
            //    fd 2 for us before reassigning.
            let r = libc::dup2(devnull, libc::STDERR_FILENO);
            libc::close(devnull);
            if r < 0 {
                libc::close(saved_fd);
                return None;
            }
            Some(StderrSilencer { saved_fd })
        }
    }
}

#[cfg(all(feature = "ai-webgpu", unix))]
impl Drop for StderrSilencer {
    fn drop(&mut self) {
        unsafe {
            // Restore original stderr. If this fails there's no
            // meaningful recovery from a Drop impl — just don't
            // panic.
            libc::dup2(self.saved_fd, libc::STDERR_FILENO);
            libc::close(self.saved_fd);
        }
    }
}

/// Pre-trigger Dawn device creation with stderr silenced, so the
/// two "limits artificially reduced" warnings fire during app
/// startup (where we can swallow them) instead of interleaving
/// with user output during the first user-triggered inference job.
///
/// Verified empirically that registering the WebGPU EP via
/// `SessionBuilder::with_execution_providers` — even without ever
/// committing a model — is enough to trigger Dawn device creation.
/// See `configure_session_builder_auto_succeeds` (the test is
/// `#[cfg_attr(feature = "ai-webgpu", ignore)]` because rapid
/// test-process exit right after Dawn init races with Dawn's
/// destructors and segfaults; long-lived GTK sessions don't hit
/// this because Dawn settles during normal use).
///
/// No-op on:
/// * builds without the `ai-webgpu` feature
/// * non-Unix platforms (the fd-redirect trick is Unix-specific)
/// * sessions where the current backend isn't `WebGpu` or `Auto`
///   (if the user picked CPU / CUDA / ROCm / OpenVINO, they won't
///   trigger Dawn and there's nothing to warm)
///
/// Safe to call from any thread; should be called once at app
/// startup after `set_current_backend` has loaded the user's
/// preference.
pub fn prewarm_webgpu_if_needed() {
    #[cfg(all(feature = "ai-webgpu", unix))]
    {
        let backend = current_backend();
        if !matches!(backend, AiBackend::WebGpu | AiBackend::Auto) {
            return;
        }

        // Redirect stderr *before* touching ort so Dawn's C++
        // warnings go to /dev/null. Scope the silencer tightly so
        // we don't accidentally swallow unrelated stderr output.
        let _silencer = match StderrSilencer::new() {
            Some(s) => s,
            // If the fd juggling failed (e.g. sandbox restricts
            // dup2), fall back to emitting the warnings normally
            // later — the feature is cosmetic, not critical.
            None => return,
        };

        let builder = match ort::session::Session::builder() {
            Ok(b) => b,
            Err(_) => return,
        };
        // Register the WebGPU EP. This triggers Dawn device
        // creation. We don't need a model — just the EP
        // registration is enough to initialize the device, and
        // dropping the builder afterwards leaves the device alive
        // in ort's environment singleton for later sessions.
        let _ = configure_session_builder(builder, AiBackend::WebGpu);
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_id_round_trip() {
        for b in [
            AiBackend::Auto,
            AiBackend::Cuda,
            AiBackend::Rocm,
            AiBackend::OpenVino,
            AiBackend::WebGpu,
            AiBackend::Cpu,
        ] {
            assert_eq!(AiBackend::from_id(b.as_id()), b);
        }
        // Unknown id → Auto.
        assert_eq!(AiBackend::from_id("nonsense"), AiBackend::Auto);
    }

    #[test]
    fn detect_always_reports_cpu() {
        let report = detect_backends();
        assert!(
            report.runtime_available.contains(&AiBackend::Cpu),
            "CPU must always be runtime-available: {report:?}"
        );
        assert!(
            report.compiled_in.contains(&AiBackend::Cpu),
            "CPU must always be compiled-in: {report:?}"
        );
        assert!(report.can_use(AiBackend::Cpu));
        assert!(report.can_use(AiBackend::Auto));
        // `describe` never panics and always mentions CPU when it's
        // the only thing available.
        let desc = report.describe();
        assert!(desc.contains("CPU") || desc.contains("cpu"));
    }

    #[test]
    fn current_backend_atomic_round_trip() {
        let original = current_backend();
        set_current_backend(AiBackend::Cpu);
        assert_eq!(current_backend(), AiBackend::Cpu);
        set_current_backend(AiBackend::Auto);
        assert_eq!(current_backend(), AiBackend::Auto);
        // Restore so other tests aren't affected by ordering.
        set_current_backend(original);
    }

    #[test]
    fn configure_session_builder_cpu_succeeds() {
        // CPU backend should always succeed; we don't actually load a
        // model (no test fixture), just verify the builder
        // construction path.
        let builder = ort::session::Session::builder()
            .expect("SessionBuilder::new should succeed on any machine");
        let configured = configure_session_builder(builder, AiBackend::Cpu);
        assert!(
            configured.is_ok(),
            "CPU backend must always configure cleanly: {:?}",
            configured.err()
        );
    }

    #[test]
    // Registering the WebGPU EP without actually running a session
    // initializes Dawn and leaves a live GPU device on the global
    // ORT state; when the test binary then exits, Dawn's C++
    // destructors race with the ORT environment teardown and the
    // process segfaults *after* the test has already passed. This
    // is a test-harness-only issue (long-lived GTK sessions don't
    // see it). Skip the Auto path for ai-webgpu builds — the CPU-
    // explicit test below still exercises the builder construction,
    // and the explicit WebGpu path is smoke-tested in the
    // `segment_with_box_smoke` integration test under `--ignored`.
    #[cfg_attr(
        feature = "ai-webgpu",
        ignore = "skips Dawn init — see ai_providers tests for rationale"
    )]
    fn configure_session_builder_auto_succeeds() {
        let builder = ort::session::Session::builder().expect("SessionBuilder::new");
        let configured = configure_session_builder(builder, AiBackend::Auto);
        assert!(configured.is_ok());
    }
}
