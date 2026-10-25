//! A shortcut to this exe in the user's Start menu, so the app can be found by name there.

use std::path::{Path, PathBuf};

use glint_core::log::log;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, STGM_READ,
};
use windows::Win32::UI::Shell::{FOLDERID_Programs, IShellLinkW, KF_FLAG_DEFAULT, SHGetKnownFolderPath, ShellLink};
use windows::core::{HSTRING, Interface};

/// Makes `<Start menu>\Programs\<app>.lnk` start this exe, on a thread of its own: written when
/// it's missing or points somewhere else (the exe moved). A failure is only logged.
pub fn sync(app: &'static str, description: &'static str) {
    std::thread::spawn(move || {
        let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        if let Err(e) = write(app, description) {
            log(&format!("start menu shortcut: {e}"));
        }
        if com {
            unsafe { CoUninitialize() };
        }
    });
}

fn write(app: &str, description: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("can't find {app}.exe: {e}"))?;
    let lnk = programs()?.join(format!("{app}.lnk"));
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        let file: IPersistFile = link.cast().map_err(|e| e.to_string())?;
        if lnk.exists() && file.Load(&HSTRING::from(lnk.as_path()), STGM_READ).is_ok() && target(&link).as_deref() == Some(&exe) {
            return Ok(());
        }
        link.SetPath(&HSTRING::from(exe.as_path())).map_err(|e| e.to_string())?;
        if let Some(dir) = exe.parent() {
            link.SetWorkingDirectory(&HSTRING::from(dir)).map_err(|e| e.to_string())?;
        }
        link.SetDescription(&HSTRING::from(description)).map_err(|e| e.to_string())?;
        file.Save(&HSTRING::from(lnk.as_path()), true).map_err(|e| format!("can't write {}: {e}", lnk.display()))
    }
}

fn target(link: &IShellLinkW) -> Option<PathBuf> {
    let mut buf = [0u16; 1024];
    unsafe { link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()? };
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(Path::new(&String::from_utf16_lossy(&buf[..n])).to_path_buf()).filter(|p| !p.as_os_str().is_empty())
}

fn programs() -> Result<PathBuf, String> {
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Programs, KF_FLAG_DEFAULT, None).map_err(|e| format!("no start menu folder: {e}"))?;
        let path = PathBuf::from(p.to_string().unwrap_or_default());
        windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as _));
        Ok(path)
    }
}
