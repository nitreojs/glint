#![windows_subsystem = "windows"]

mod app;
mod buffer;
mod config;

fn main() {
    glint_shell::log::install(config::APP);
    app::run();
}
