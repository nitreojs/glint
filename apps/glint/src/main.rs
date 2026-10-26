#![windows_subsystem = "windows"]

fn main() {
    glint_shell::log::install("glint");
    if std::env::args().any(|a| a == glint::settings::CAPTURE_ARG) {
        glint::settings::capture_keys();
        return;
    }
    if std::env::args().any(|a| a == "--settings") {
        glint::settings::run();
        return;
    }
    let args: Vec<_> = std::env::args_os().collect();
    if let Some(i) = args.iter().position(|a| a == glint::clip::CLIP_ARG) {
        glint::clip::run(args.get(i + 1).map(Into::into));
        return;
    }
    if let Some(i) = args.iter().position(|a| a == glint_ui::gallery::GALLERY_ARG) {
        glint::gallery::run(args.get(i + 1).map(Into::into));
        return;
    }
    glint::app::run();
}
