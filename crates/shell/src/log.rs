//! The log file: `%APPDATA%\<app>\<app>.log`, where [`glint_core::log::log`] writes.

use std::path::PathBuf;
use std::sync::OnceLock;

use glint_core::paths::app_dir_for;

use super::time::timestamp;

static APP: OnceLock<&'static str> = OnceLock::new();

/// Sends the log to `app`'s file from now on.
pub fn install(app: &'static str) {
    let _ = APP.set(app);
    glint_core::log::set_sink(write);
}

fn write(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path()) {
        let _ = writeln!(f, "[{}] {}", timestamp(), msg);
    }
}

/// Starts this run's log empty; the last run's is kept beside it as `<app>.prev.log`, so what led
/// up to a crash or a restart is still there.
pub fn start_fresh() {
    let path = path();
    let _ = std::fs::rename(&path, path.with_extension("prev.log"));
}

/// The log file of the app that called [`install`].
pub fn path() -> PathBuf {
    let app = APP.get().copied().unwrap_or("glint");
    app_dir_for(app).join(format!("{app}.log"))
}
