use std::path::PathBuf;

/// `%APPDATA%\glint`, created on demand.
pub fn app_dir() -> PathBuf {
    app_dir_for("glint")
}

/// `%APPDATA%\<app>`, created on demand.
pub fn app_dir_for(app: &str) -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(app);
    let _ = std::fs::create_dir_all(&dir);
    dir
}
