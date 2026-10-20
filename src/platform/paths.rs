use std::path::PathBuf;

use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_Documents, FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
use windows::core::GUID;

/// `%APPDATA%\glint`, created on demand.
pub(crate) fn app_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join("glint");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as _));
        (!s.is_empty()).then(|| PathBuf::from(s))
    }
}

pub(crate) fn videos_dir() -> PathBuf {
    known_folder(&FOLDERID_Videos).unwrap_or_else(|| {
        std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Videos")).unwrap_or_else(std::env::temp_dir)
    })
}

pub(crate) fn documents_dir() -> Option<PathBuf> {
    known_folder(&FOLDERID_Documents)
}
