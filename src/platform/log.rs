use std::path::PathBuf;

use super::format::timestamp;
use super::paths::app_dir;

/// Appends a line to `%APPDATA%\glint\glint.log`.
pub(crate) fn log(msg: &str) {
    use std::io::Write;
    if cfg!(test) {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_path()) {
        let _ = writeln!(f, "[{}] {}", timestamp(), msg);
    }
}

pub(crate) fn log_path() -> PathBuf {
    app_dir().join("glint.log")
}
