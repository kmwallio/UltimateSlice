mod app;
mod model;
mod media;
mod fcpxml;
mod ui;
mod undo;

fn main() {
    env_logger::init();
    app::run();
}
