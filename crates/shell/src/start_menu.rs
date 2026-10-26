//! Shortcuts to this exe in the user's Start menu, so the app (and what it starts with arguments,
//! like glint's editor) can be found by name there.

use std::path::{Path, PathBuf};

use glint_core::log::log;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, STGM_READ,
};
use windows::Win32::UI::Shell::{FOLDERID_Programs, IShellLinkW, KF_FLAG_DEFAULT, SHGetKnownFolderPath, ShellLink};
use windows::core::{HSTRING, Interface};

/// One Start menu entry: `<Start menu>\Programs\<name>.lnk` starting this exe with `args`.
#[derive(Clone, Copy, Debug)]
pub struct Shortcut {
    pub name: &'static str,
    /// What the exe is started with; empty for none. A file dropped on the shortcut follows it.
    pub args: &'static str,
    pub description: &'static str,
}

/// Writes the `shortcuts` that are missing or point somewhere else (the exe moved, the arguments
/// changed), on a thread of its own. A failure is only logged.
pub fn sync(shortcuts: &'static [Shortcut]) {
    std::thread::spawn(move || {
        let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        let written = std::env::current_exe()
            .map_err(|e| format!("can't find this exe: {e}"))
            .and_then(|exe| Ok((exe, programs()?)))
            .and_then(|(exe, dir)| shortcuts.iter().try_for_each(|s| write(&dir, &exe, s)));
        if let Err(e) = written {
            log(&format!("start menu shortcut: {e}"));
        }
        if com {
            unsafe { CoUninitialize() };
        }
    });
}

/// Makes `<dir>\<name>.lnk` start `exe` with the shortcut's arguments, unless it already does.
/// The calling thread needs COM.
fn write(dir: &Path, exe: &Path, s: &Shortcut) -> Result<(), String> {
    let lnk = dir.join(format!("{}.lnk", s.name));
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        let file: IPersistFile = link.cast().map_err(|e| e.to_string())?;
        if lnk.exists() && file.Load(&HSTRING::from(lnk.as_path()), STGM_READ).is_ok() && read(&link) == Some((exe.to_path_buf(), s.args.to_string())) {
            return Ok(());
        }
        link.SetPath(&HSTRING::from(exe)).map_err(|e| e.to_string())?;
        link.SetArguments(&HSTRING::from(s.args)).map_err(|e| e.to_string())?;
        if let Some(dir) = exe.parent() {
            link.SetWorkingDirectory(&HSTRING::from(dir)).map_err(|e| e.to_string())?;
        }
        link.SetDescription(&HSTRING::from(s.description)).map_err(|e| e.to_string())?;
        file.Save(&HSTRING::from(lnk.as_path()), true).map_err(|e| format!("can't write {}: {e}", lnk.display()))
    }
}

/// The link's target and arguments.
fn read(link: &IShellLinkW) -> Option<(PathBuf, String)> {
    let mut path = [0u16; 1024];
    unsafe { link.GetPath(&mut path, std::ptr::null_mut(), 0).ok()? };
    let mut args = [0u16; 1024];
    unsafe { link.GetArguments(&mut args).ok()? };
    Some((PathBuf::from(utf16(&path)), utf16(&args))).filter(|(p, _)| !p.as_os_str().is_empty())
}

fn utf16(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

fn programs() -> Result<PathBuf, String> {
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Programs, KF_FLAG_DEFAULT, None).map_err(|e| format!("no start menu folder: {e}"))?;
        let path = PathBuf::from(p.to_string().unwrap_or_default());
        windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as _));
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: Shortcut = Shortcut { name: "glint", args: "", description: "screen recorder" };
    const EDITOR: Shortcut = Shortcut { name: "glint editor", args: "--clip", description: "trim videos" };

    /// The shortcut at `lnk`: its target and arguments.
    fn load(lnk: &Path) -> (PathBuf, String) {
        unsafe {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            link.cast::<IPersistFile>().unwrap().Load(&HSTRING::from(lnk), STGM_READ).unwrap();
            read(&link).unwrap()
        }
    }

    #[test]
    fn shortcuts_start_the_exe_with_their_arguments_and_follow_it_when_it_moves() {
        let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        let dir = std::env::temp_dir().join(format!("glint-start-menu-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("old").join("glint.exe");
        let moved = dir.join("new").join("glint.exe");
        for e in [&exe, &moved] {
            std::fs::create_dir_all(e.parent().unwrap()).unwrap();
            std::fs::write(e, b"").unwrap();
        }
        for s in [APP, EDITOR] {
            write(&dir, &exe, &s).unwrap();
        }
        assert_eq!(load(&dir.join("glint.lnk")), (exe.clone(), String::new()));
        assert_eq!(load(&dir.join("glint editor.lnk")), (exe, "--clip".to_string()));

        write(&dir, &moved, &EDITOR).unwrap();
        assert_eq!(load(&dir.join("glint editor.lnk")), (moved, "--clip".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
        if com {
            unsafe { CoUninitialize() };
        }
    }
}
