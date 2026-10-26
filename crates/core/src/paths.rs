use std::path::PathBuf;

/// `%APPDATA%\glint`, created on demand.
pub fn app_dir() -> PathBuf {
    app_dir_for("glint")
}

/// `%APPDATA%\<app>`, created on demand.
pub fn app_dir_for(app: &str) -> PathBuf {
    let dir = appdata().join(app);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// `%APPDATA%`, where each app keeps its folder.
pub fn appdata() -> PathBuf {
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}
