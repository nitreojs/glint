//! Explorer's ways to open some file types with this exe, for this user only: an entry in the
//! right-click menu (a verb under HKCU\Software\Classes\SystemFileAssociations\.<ext>\shell) and a
//! place under "open with" (HKCU\Software\Classes\Applications\<exe>).

use std::path::Path;

use glint_core::wide::wide;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegDeleteTreeW, RegSetKeyValueW};
use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
use windows::core::{HSTRING, PCWSTR};

/// Adds (`on`) or removes the verb `key` labelled `label` on files ending in `exts`; it runs this
/// exe with `arg` and the file. Adding rewrites the command, so a moved exe still opens.
pub fn sync(key: &str, label: &str, exts: &[&str], arg: &str, on: bool) -> Result<(), String> {
    let exe = exe()?;
    for ext in exts {
        let verb = format!(r"Software\Classes\SystemFileAssociations\.{ext}\shell\{key}");
        if on {
            set(&verb, "MUIVerb", label)?;
            set(&verb, "Icon", &format!("\"{exe}\",0"))?;
            set(&format!(r"{verb}\command"), "", &format!("\"{exe}\" {arg} \"%1\""))?;
        } else {
            remove(&verb)?;
        }
    }
    changed();
    Ok(())
}

/// Lists this exe as `name` under "open with" on files ending in `exts` (`on`), or takes it off;
/// it runs with `arg` and the file. Adding rewrites the command, so a moved exe still opens.
pub fn open_with(name: &str, exts: &[&str], arg: &str, on: bool) -> Result<(), String> {
    let exe = exe()?;
    let file = Path::new(&exe).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let app = format!(r"Software\Classes\Applications\{file}");
    if on {
        set(&app, "FriendlyAppName", name)?;
        set(&format!(r"{app}\DefaultIcon"), "", &format!("\"{exe}\",0"))?;
        set(&format!(r"{app}\shell\open\command"), "", &format!("\"{exe}\" {arg} \"%1\""))?;
        for ext in exts {
            set(&format!(r"{app}\SupportedTypes"), &format!(".{ext}"), "")?;
            set(&format!(r"Software\Classes\SystemFileAssociations\.{ext}\OpenWithList\{file}"), "", "")?;
        }
    } else {
        remove(&app)?;
        for ext in exts {
            remove(&format!(r"Software\Classes\SystemFileAssociations\.{ext}\OpenWithList\{file}"))?;
        }
    }
    changed();
    Ok(())
}

fn exe() -> Result<String, String> {
    std::env::current_exe().map(|e| e.display().to_string()).map_err(|e| format!("can't find this exe: {e}"))
}

/// Explorer picks the change up without a restart.
fn changed() {
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
}

fn remove(key: &str) -> Result<(), String> {
    let err = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key)) };
    if err == ERROR_FILE_NOT_FOUND { Ok(()) } else { check(err) }
}

fn set(key: &str, name: &str, value: &str) -> Result<(), String> {
    let data = wide(value);
    let bytes = (data.len() * 2) as u32; // UTF-16 including the terminating NUL
    // the key's default value has no name
    let name = HSTRING::from(name);
    let name = if name.is_empty() { PCWSTR::null() } else { PCWSTR(name.as_ptr()) };
    let err = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(key), name, REG_SZ.0, Some(data.as_ptr().cast()), bytes) };
    check(err)
}

fn check(err: WIN32_ERROR) -> Result<(), String> {
    if err == ERROR_SUCCESS { Ok(()) } else { Err(format!("can't update the explorer menu: {}", err.to_hresult().message())) }
}
