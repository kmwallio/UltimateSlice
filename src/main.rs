#![recursion_limit = "256"]
mod app;
mod fcpxml;
mod mcp;
mod media;
mod model;
mod recent;
mod ui;
mod ui_state;
mod undo;

/// PID file used to track a running `--mcp` instance so a new invocation
/// can terminate the old one before taking over.
const MCP_PID_FILE: &str = "/tmp/ultimateslice-mcp.pid";

fn main() {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    let mcp_enabled = args.iter().any(|a| a == "--mcp");
    let mcp_attach = args.iter().any(|a| a == "--mcp-attach");
    let startup_project_path = args.iter().skip(1).find(|a| !a.starts_with("--")).cloned();

    // --mcp-attach: bridge stdio to the running instance's Unix domain socket
    // and exit. No GUI is started.
    if mcp_attach {
        std::process::exit(run_mcp_attach());
    }

    // On macOS, GSettings schemas may become stale after Homebrew upgrades
    // (e.g. GTK4 adds new schema keys but glib-compile-schemas wasn't rerun).
    // A missing key causes a fatal g_error() in GTK4's FileChooserNative —
    // recompile proactively before GTK initialization to prevent this crash.
    #[cfg(target_os = "macos")]
    ensure_gsettings_schemas();

    // Apply GSK renderer preference before GTK initializes.
    let prefs = ui_state::load_preferences_state();
    if let Some(renderer) = prefs.gsk_renderer.env_value() {
        std::env::set_var("GSK_RENDERER", renderer);
    }

    if mcp_enabled {
        ensure_single_mcp_instance();
    }
    app::run(mcp_enabled, startup_project_path);
    if mcp_enabled {
        // Clean up the PID file when the app exits normally.
        let _ = std::fs::remove_file(MCP_PID_FILE);
    }
}

/// Bridge stdin/stdout to the running UltimateSlice instance's MCP Unix socket.
/// Returns 0 on clean shutdown, 1 on error.
fn run_mcp_attach() -> i32 {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::net::UnixStream;

    let path = mcp::server::socket_path();
    let stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "Failed to connect to MCP socket at {}: {e}\n\
                 Make sure UltimateSlice is running with the MCP socket server enabled \
                 (Preferences → Integration → Enable MCP socket server).",
                path.display()
            );
            return 1;
        }
    };

    let reader_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to clone socket: {e}");
            return 1;
        }
    };
    let mut writer_stream = stream;

    // Thread 1: stdin → socket
    let writer_handle = std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            let n = match stdin.lock().read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if writer_stream.write_all(&buf[..n]).is_err() {
                break;
            }
            if writer_stream.flush().is_err() {
                break;
            }
        }
        // Shut down write half so the server sees EOF
        let _ = writer_stream.shutdown(std::net::Shutdown::Write);
    });

    // Thread 2 (main): socket → stdout
    let reader = BufReader::new(reader_stream);
    let mut stdout = std::io::stdout();
    for line in reader.lines() {
        match line {
            Ok(l) => {
                if writeln!(stdout, "{l}").is_err() {
                    break;
                }
                if stdout.flush().is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    let _ = writer_handle.join();
    0
}

/// If another `--mcp` instance is already running (tracked via `MCP_PID_FILE`),
/// send it SIGTERM and wait up to 3 s for it to exit (SIGKILL after that).
/// Then write this process's PID to the file so the next invocation can find us.
fn ensure_single_mcp_instance() {
    if let Ok(content) = std::fs::read_to_string(MCP_PID_FILE) {
        if let Ok(pid) = content.trim().parse::<u32>() {
            if process_exists(pid) {
                eprintln!("[MCP] Terminating existing instance (PID {pid})…");
                let killed = std::process::Command::new("kill")
                    .args(["-15", &pid.to_string()])
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                if killed {
                    // Wait up to 3 s for graceful exit
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                    while std::time::Instant::now() < deadline {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        if !process_exists(pid) {
                            break;
                        }
                    }
                    // Force-kill if still alive
                    if process_exists(pid) {
                        eprintln!("[MCP] Sending SIGKILL to PID {pid}");
                        let _ = std::process::Command::new("kill")
                            .args(["-9", &pid.to_string()])
                            .status();
                        std::thread::sleep(std::time::Duration::from_millis(300));
                    }
                }
                // If kill failed (e.g., stale PID belonging to a system process),
                // just remove the stale file and continue.
            }
        }
        let _ = std::fs::remove_file(MCP_PID_FILE);
    }
    // Register this instance's PID
    let _ = std::fs::write(MCP_PID_FILE, std::process::id().to_string());
}

/// Check whether a process with the given PID is alive via `/proc/{pid}`.
fn process_exists(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

/// On macOS with Homebrew, GLib resolves GSettings schemas via
/// `XDG_DATA_DIRS`.  On Apple Silicon Macs Homebrew installs to
/// `/opt/homebrew` which is typically *not* in `XDG_DATA_DIRS`, so GLib may
/// find a stale `gschemas.compiled` under `/usr/local/share` (left over from
/// an old Intel Homebrew or system install) instead of the current one.  A
/// missing or outdated key in the compiled schema causes a fatal `g_error()`
/// (SIGTRAP) inside GTK4's `FileChooserNative` when it calls
/// `g_settings_get_enum`.
///
/// This function:
/// 1. Finds the Homebrew schemas directory that matches the running GTK4.
/// 2. Recompiles it if any source XML is newer than `gschemas.compiled`.
/// 3. **Always** sets `GSETTINGS_SCHEMA_DIR` so GLib uses the right one,
///    regardless of what `XDG_DATA_DIRS` contains.
#[cfg(target_os = "macos")]
fn ensure_gsettings_schemas() {
    // Prefer Apple Silicon path, fall back to Intel Homebrew.
    let candidates = [
        "/opt/homebrew/share/glib-2.0/schemas",
        "/usr/local/share/glib-2.0/schemas",
    ];

    let Some(schema_dir) = candidates.iter().find(|d| {
        // Only consider directories that actually contain schema XML sources
        // (not leftover dirs with only a compiled file).
        let dir = std::path::Path::new(d);
        dir.exists()
            && std::fs::read_dir(dir)
                .map(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .any(|e| e.file_name().to_string_lossy().ends_with(".gschema.xml"))
                })
                .unwrap_or(false)
    }) else {
        return;
    };

    // Recompile if any source XML is newer than the compiled file.
    if needs_schema_recompile(schema_dir) {
        if let Some(ref compiler) = find_glib_compile_schemas() {
            // Try in-place first (single-user Homebrew).
            let ok = std::process::Command::new(compiler)
                .arg(schema_dir)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);

            if !ok {
                // Fallback: compile into a per-user cache directory.
                let cache_dir = std::env::temp_dir().join("ultimateslice-gsettings");
                if compile_schemas_to_cache(compiler, schema_dir, &cache_dir) {
                    std::env::set_var("GSETTINGS_SCHEMA_DIR", &cache_dir);
                    return;
                }
            }
        }
    }

    // Point GLib at the Homebrew schemas directory so it is not shadowed by
    // a stale compiled file elsewhere on XDG_DATA_DIRS.
    std::env::set_var("GSETTINGS_SCHEMA_DIR", schema_dir);
}

/// Copy schema XMLs to `cache_dir`, compile them, and return whether the
/// resulting `gschemas.compiled` exists.
#[cfg(target_os = "macos")]
fn compile_schemas_to_cache(compiler: &str, schema_dir: &str, cache_dir: &std::path::Path) -> bool {
    if std::fs::create_dir_all(cache_dir).is_err() {
        return false;
    }
    if let Ok(entries) = std::fs::read_dir(schema_dir) {
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.ends_with(".gschema.xml") || name_str.ends_with(".gschema.override") {
                let _ = std::fs::copy(entry.path(), cache_dir.join(&name));
            }
        }
    }
    let ok = std::process::Command::new(compiler)
        .arg(cache_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ok && cache_dir.join("gschemas.compiled").exists()
}

#[cfg(target_os = "macos")]
fn needs_schema_recompile(schema_dir: &str) -> bool {
    let dir = std::path::Path::new(schema_dir);
    let compiled = dir.join("gschemas.compiled");
    let compiled_mtime = match compiled.metadata().and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return true, // no compiled file
    };
    std::fs::read_dir(dir)
        .map(|entries| {
            entries.filter_map(Result::ok).any(|e| {
                let name = e.file_name();
                let n = name.to_string_lossy();
                (n.ends_with(".gschema.xml") || n.ends_with(".gschema.override"))
                    && e.metadata()
                        .and_then(|m| m.modified())
                        .map(|t| t > compiled_mtime)
                        .unwrap_or(false)
            })
        })
        .unwrap_or(true)
}

#[cfg(target_os = "macos")]
fn find_glib_compile_schemas() -> Option<String> {
    let known = [
        "/opt/homebrew/bin/glib-compile-schemas",
        "/usr/local/bin/glib-compile-schemas",
    ];
    for path in &known {
        if std::path::Path::new(path).exists() {
            return Some(path.to_string());
        }
    }
    // Fall back to PATH lookup
    if std::process::Command::new("glib-compile-schemas")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return Some("glib-compile-schemas".to_string());
    }
    None
}
