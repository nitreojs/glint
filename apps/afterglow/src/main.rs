#![windows_subsystem = "windows"]

mod app;
mod buffer;
mod config;
mod settings;

fn main() {
    glint_shell::log::install(config::APP);
    if std::env::args().any(|a| a == glint_ui::capture::CAPTURE_ARG) {
        glint_ui::capture::capture_keys();
        return;
    }
    if std::env::args().any(|a| a == settings::SETTINGS_ARG) {
        settings::run();
        return;
    }
    app::run();
}
