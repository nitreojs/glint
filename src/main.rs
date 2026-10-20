#![windows_subsystem = "windows"]

fn main() {
    if std::env::args().any(|a| a == glint::settings::CAPTURE_ARG) {
        glint::settings::capture_keys();
        return;
    }
    if std::env::args().any(|a| a == "--settings") {
        glint::settings::run();
        return;
    }
    glint::app::run();
}
