//! Global hotkeys on a window: `WM_HOTKEY` arrives with the id.

use glint_core::config::hotkeys::parse_hotkey;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey};

/// Takes `combo` (like `"Alt+F10"`) as hotkey `id` on `hwnd`; false when it doesn't parse or
/// another app holds it.
pub fn register(hwnd: HWND, id: i32, combo: &str) -> bool {
    let Ok((mods, vk)) = parse_hotkey(combo) else { return false };
    unsafe { RegisterHotKey(Some(hwnd), id, HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT, vk).is_ok() }
}

/// Registers a bare key (no combo text), like Esc while a countdown runs.
pub fn register_key(hwnd: HWND, id: i32, vk: u32) -> bool {
    unsafe { RegisterHotKey(Some(hwnd), id, MOD_NOREPEAT, vk).is_ok() }
}

pub fn unregister(hwnd: HWND, id: i32) {
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), id);
    }
}
