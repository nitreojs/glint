use std::path::{Path, PathBuf};

use glint_core::config::{Config, pinned_output_dir};
use glint_core::history::Kind;
use glint_core::log::log;
use glint_core::naming::{self, Naming, Subject};
use glint_core::paths::app_dir;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_Documents, FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
use windows::core::GUID;

use glint_shell::time::date_time;

/// Migrations already done, one name per line.
const MIGRATED: &str = "migrated";
const DOCUMENTS_DEFAULT: &str = "documents-default-folder";
/// The output folder in use before the last change in settings.
const PREVIOUS_DIR: &str = "previous-output-dir";

pub(crate) fn output_dir(cfg: &Config) -> PathBuf {
    cfg.output_dir_or(|| {
        documents_dir().unwrap_or_else(|| user_folder("Documents"))
    })
}

/// Where captures went by default up to 0.1.
pub(crate) fn old_default_dir() -> PathBuf {
    known_folder(&FOLDERID_Videos).unwrap_or_else(|| user_folder("Videos")).join("glint")
}

fn user_folder(name: &str) -> PathBuf {
    std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join(name)).unwrap_or_else(std::env::temp_dir)
}

/// The capture's file name for the settings' pattern, without its folders.
pub(crate) fn file_name(cfg: &Config, kind: Kind, subject: Subject, ext: &str) -> String {
    let (date, time) = date_time();
    let path = naming::expand(&cfg.file_name, &Naming { date: &date, time: &time, kind, subject });
    format!("{}.{ext}", path.file_name().unwrap_or_default().to_string_lossy())
}

/// A new file in the output folder named by the settings' pattern, never overwriting; the
/// folders the pattern makes are created.
pub(crate) fn unique_path(cfg: &Config, kind: Kind, subject: Subject, ext: &str) -> PathBuf {
    let (date, time) = date_time();
    let named = naming::expand(&cfg.file_name, &Naming { date: &date, time: &time, kind, subject });
    let path = output_dir(cfg).join(format!("{}.{ext}", named.display()));
    if let Some(dir) = path.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        log(&format!("can't create {}: {e}", dir.display()));
    }
    unused(path)
}

/// `path`, or `name_2.ext`, `name_3.ext`… next to it if it's taken.
pub(crate) fn unused(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    (2..).map(|n| dir.join(format!("{stem}_{n}{ext}"))).find(|p| !p.exists()).expect("some number is free")
}

/// Since 0.2 the default folder is `<Documents>\glint`; who already has captures in
/// `<Videos>\glint` keeps it, once, on the first start of a version that knows.
pub(crate) fn migrate_output_dir(cfg: &mut Config) {
    let marker = app_dir().join(MIGRATED);
    let done = std::fs::read_to_string(&marker).unwrap_or_default();
    if done.lines().any(|l| l.trim() == DOCUMENTS_DEFAULT) {
        return;
    }
    let old = old_default_dir();
    let files: Vec<String> = std::fs::read_dir(&old)
        .map(|d| {
            d.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    let names: Vec<&str> = files.iter().map(String::as_str).collect();
    if let Some(dir) = pinned_output_dir(&cfg.output_dir, &old, &names) {
        let before = std::mem::replace(&mut cfg.output_dir, dir.clone());
        match cfg.save() {
            Ok(()) => log(&format!("kept the old default folder {dir}: it has {} files", names.len())),
            Err(e) => {
                cfg.output_dir = before;
                return log(&format!("can't keep the old default folder: {e}"));
            }
        }
    }
    let text = format!("{done}{DOCUMENTS_DEFAULT}\n");
    if let Err(e) = std::fs::write(&marker, text) {
        log(&format!("can't write {}: {e}", marker.display()));
    }
}

pub(crate) fn previous_output_dir() -> Option<PathBuf> {
    let text = std::fs::read_to_string(app_dir().join(PREVIOUS_DIR)).ok()?;
    Some(PathBuf::from(text.trim())).filter(|p| !p.as_os_str().is_empty())
}

pub(crate) fn remember_previous_output_dir(dir: &Path) {
    let _ = std::fs::write(app_dir().join(PREVIOUS_DIR), dir.display().to_string());
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as _));
        (!s.is_empty()).then(|| PathBuf::from(s))
    }
}

pub(crate) fn documents_dir() -> Option<PathBuf> {
    known_folder(&FOLDERID_Documents)
}
