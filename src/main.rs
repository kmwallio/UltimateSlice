mod app;
mod model;
mod media;
mod fcpxml;
mod ui;
mod undo;
mod mcp;
mod recent;
mod ui_state;

/// PID file used to track a running `--mcp` instance so a new invocation
/// can terminate the old one before taking over.
const MCP_PID_FILE: &str = "/tmp/ultimateslice-mcp.pid";

fn main() {
    env_logger::init();
    let mcp_enabled = std::env::args().any(|a| a == "--mcp");
    if mcp_enabled {
        ensure_single_mcp_instance();
    }
    app::run(mcp_enabled);
    if mcp_enabled {
        // Clean up the PID file when the app exits normally.
        let _ = std::fs::remove_file(MCP_PID_FILE);
    }
}

/// If another `--mcp` instance is already running (tracked via `MCP_PID_FILE`),
/// send it SIGTERM and wait up to 3 s for it to exit (SIGKILL after that).
/// Then write this process's PID to the file so the next invocation can find us.
fn ensure_single_mcp_instance() {
    if let Ok(content) = std::fs::read_to_string(MCP_PID_FILE) {
        if let Ok(pid) = content.trim().parse::<u32>() {
            if process_exists(pid) {
                eprintln!("[MCP] Terminating existing instance (PID {pid})…");
                let _ = std::process::Command::new("kill")
                    .args(["-15", &pid.to_string()])
                    .status();
                // Wait up to 3 s for graceful exit
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                while std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    if !process_exists(pid) { break; }
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
