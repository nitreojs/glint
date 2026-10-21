use std::path::PathBuf;

use glint_core::paths::app_dir;

use super::format::timestamp;

pub fn install() {
    glint_core::log::set_sink(write);
}

/// Appends a line to `%APPDATA%\glint\glint.log`.
fn write(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_path()) {
        let _ = writeln!(f, "[{}] {}", timestamp(), msg);
    }
}

pub(crate) fn log_path() -> PathBuf {
    app_dir().join("glint.log")
}
