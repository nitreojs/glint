//! Messages between the tray app and the settings process (`glint.exe --settings`).

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, WM_APP};
use windows::core::HSTRING;

/// Window class of the tray app's hidden main window.
pub(crate) const MAIN_CLASS: &str = "glint_main";
pub(crate) const SETTINGS_TITLE: &str = "glint settings";
/// Starts this exe as the settings window.
pub const SETTINGS_ARG: &str = "--settings";

/// Settings were saved: re-read config.toml.
pub(crate) const WM_RELOAD: u32 = WM_APP + 20;
/// WPARAM 1: release the global hotkey (the settings window is recording a new one);
/// WPARAM 0: take it back.
pub(crate) const WM_HOTKEY_PAUSE: u32 = WM_APP + 21;

/// Posts `msg` to the running tray app, if any.
pub(crate) fn post_to_main(msg: u32, wparam: usize) -> bool {
    glint_shell::window::post(MAIN_CLASS, msg, wparam)
}

/// The open settings window, if any.
pub(crate) fn find_settings() -> Option<HWND> {
    unsafe { FindWindowW(None, &HSTRING::from(SETTINGS_TITLE)).ok() }
}
