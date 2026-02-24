mod app;
mod model;
mod media;
mod fcpxml;
mod ui;

fn main() {
    env_logger::init();
    app::run();
}
