//! What glint and afterglow say to each other. afterglow's on-screen note is kept out of capture
//! so its own clips never show it; glint asks for it to be let in while it takes a screenshot.

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_APP};
use windows::core::HSTRING;

/// Window class of afterglow's hidden main window.
pub const MAIN_CLASS: &str = "afterglow_main";
/// WPARAM 1: let capture see the note (answers 1 when one is up); 0: keep it out again.
pub const WM_NOTE_CAPTURABLE: u32 = WM_APP + 23;

/// Runs `grab` with afterglow's note, if one is up, visible to screen capture.
pub fn with_note_visible<T>(grab: impl FnOnce() -> T) -> T {
    let Ok(main) = (unsafe { FindWindowW(&HSTRING::from(crate::instance::scoped(MAIN_CLASS)), None) }) else {
        return grab();
    };
    let mut up = 0usize;
    let asked = unsafe {
        SendMessageTimeoutW(main, WM_NOTE_CAPTURABLE, WPARAM(1), LPARAM(0), SMTO_ABORTIFHUNG, 200, Some(&mut up))
    };
    if asked.0 == 0 || up == 0 {
        return grab();
    }
    // the change shows from the next composed frame on
    unsafe {
        let _ = DwmFlush();
    }
    let out = grab();
    unsafe {
        let _ = PostMessageW(Some(main), WM_NOTE_CAPTURABLE, WPARAM(0), LPARAM(0));
    }
    out
}
