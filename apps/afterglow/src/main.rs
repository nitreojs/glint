#![windows_subsystem = "windows"]

mod app;
mod buffer;
mod config;
mod events;
mod gallery;
mod glint;
mod note;
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
    let args: Vec<_> = std::env::args_os().collect();
    if let Some(i) = args.iter().position(|a| a == glint_ui::gallery::GALLERY_ARG) {
        gallery::run(args.get(i + 1).map(Into::into));
        return;
    }
    app::run();
}
