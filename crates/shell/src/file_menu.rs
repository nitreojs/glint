//! An entry in Explorer's right-click menu for some file types, starting this exe with the file:
//! a verb under HKCU\Software\Classes\SystemFileAssociations\.<ext>\shell, for this user only.

use glint_core::wide::wide;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegDeleteTreeW, RegSetKeyValueW};
use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
use windows::core::{HSTRING, PCWSTR};

/// Adds (`on`) or removes the verb `key` labelled `label` on files ending in `exts`; it runs this
/// exe with `arg` and the file. Adding rewrites the command, so a moved exe still opens.
pub fn sync(key: &str, label: &str, exts: &[&str], arg: &str, on: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("can't find this exe: {e}"))?;
    let exe = exe.display().to_string();
    for ext in exts {
        let verb = format!(r"Software\Classes\SystemFileAssociations\.{ext}\shell\{key}");
        if on {
            set(&verb, "MUIVerb", label)?;
            set(&verb, "Icon", &format!("\"{exe}\",0"))?;
            set(&format!(r"{verb}\command"), "", &format!("\"{exe}\" {arg} \"%1\""))?;
        } else {
            let err = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(verb.as_str())) };
            if err != ERROR_FILE_NOT_FOUND {
                check(err)?;
            }
        }
    }
    // Explorer picks the change up without a restart
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    Ok(())
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
