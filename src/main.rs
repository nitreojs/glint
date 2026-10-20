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
    let args: Vec<_> = std::env::args_os().collect();
    if let Some(i) = args.iter().position(|a| a == glint::clip::CLIP_ARG)
        && let Some(path) = args.get(i + 1)
    {
        glint::clip::run(path.into());
        return;
    }
    glint::app::run();
}
