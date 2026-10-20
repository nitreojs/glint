//! Messages between the tray app and the settings process (`glint.exe --settings`).

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_APP};
use windows::core::{HSTRING, PCWSTR, w};

/// Window class of the tray app's hidden main window.
pub const MAIN_CLASS: PCWSTR = w!("glint_main");
pub const SETTINGS_TITLE: &str = "glint settings";

/// Settings were saved: re-read config.toml.
pub const WM_RELOAD: u32 = WM_APP + 20;
/// WPARAM 1: release the global hotkey (the settings window is recording a new one);
/// WPARAM 0: take it back.
pub const WM_HOTKEY_PAUSE: u32 = WM_APP + 21;

/// Posts `msg` to the running tray app, if any.
pub fn post_to_main(msg: u32, wparam: usize) -> bool {
    unsafe {
        match FindWindowW(MAIN_CLASS, None) {
            Ok(h) => PostMessageW(Some(h), msg, WPARAM(wparam), LPARAM(0)).is_ok(),
            Err(_) => false,
        }
    }
}

/// The open settings window, if any.
pub fn find_settings() -> Option<HWND> {
    unsafe { FindWindowW(None, &HSTRING::from(SETTINGS_TITLE)).ok() }
}
