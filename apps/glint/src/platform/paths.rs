use std::path::PathBuf;

use glint_core::config::Config;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_Documents, FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
use windows::core::GUID;

use super::format::date_time;

pub(crate) fn output_dir(cfg: &Config) -> PathBuf {
    cfg.output_dir_or(videos_dir)
}

/// A new file in the output folder named by the settings' pattern, never overwriting.
pub(crate) fn unique_path(cfg: &Config, ext: &str) -> PathBuf {
    let dir = output_dir(cfg);
    let (date, time) = date_time();
    let stem = cfg.file_stem(&date, &time);
    let mut path = dir.join(format!("{stem}.{ext}"));
    for n in 2.. {
        if !path.exists() {
            break;
        }
        path = dir.join(format!("{stem}_{n}.{ext}"));
    }
    path
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
