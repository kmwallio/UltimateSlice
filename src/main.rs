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
