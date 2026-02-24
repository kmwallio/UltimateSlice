mod app;
mod model;
mod media;
mod fcpxml;
mod ui;
mod undo;
mod mcp;

fn main() {
    env_logger::init();
    let mcp_enabled = std::env::args().any(|a| a == "--mcp");
    app::run(mcp_enabled);
}
