//! "Launch at startup": a value under HKCU\…\Run that starts this exe at sign-in.

use glint_core::wide::wide;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW};
use windows::core::w;

/// Passed by the Run entry, so a sign-in launch skips the "glint is running" balloon.
pub(crate) const ARG: &str = "--autostart";

/// Adds or removes the Run entry. Adding rewrites the command, so a moved exe still starts.
pub(crate) fn sync(enabled: bool) -> Result<(), String> {
    let key = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
    let err = unsafe {
        if enabled {
            let exe = std::env::current_exe().map_err(|e| format!("can't find glint.exe: {e}"))?;
            let cmd = wide(&format!("\"{}\" {ARG}", exe.display()));
            let bytes = (cmd.len() * 2) as u32; // UTF-16 including the terminating NUL
            RegSetKeyValueW(HKEY_CURRENT_USER, key, w!("glint"), REG_SZ.0, Some(cmd.as_ptr().cast()), bytes)
        } else {
            match RegDeleteKeyValueW(HKEY_CURRENT_USER, key, w!("glint")) {
                ERROR_FILE_NOT_FOUND => ERROR_SUCCESS,
                e => e,
            }
        }
    };
    err.ok().map_err(|e| format!("can't update the startup entry: {}", e.message()))
}
