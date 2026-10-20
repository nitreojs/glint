use windows::Win32::Foundation::{HWND, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetClassNameW, GetLayeredWindowAttributes, GetWindowLongW, GetWindowRect, IsIconic,
    IsWindowVisible, LAYERED_WINDOW_ATTRIBUTES_FLAGS, LWA_ALPHA, WS_EX_LAYERED, WS_EX_TRANSPARENT,
};
use windows::core::{BOOL, w};

use super::rect::Rect;

/// Visible top-level windows, topmost first, by their visible DWM frame (without the
/// invisible resize borders). Taken together with the screenshot, so hovering matches it.
pub(crate) fn visible_windows() -> Vec<Rect> {
    unsafe extern "system" fn add(hwnd: HWND, lp: LPARAM) -> BOOL {
        let list = unsafe { &mut *(lp.0 as *mut Vec<Rect>) };
        list.extend(unsafe { window_rect(hwnd) });
        TRUE
    }
    let mut list = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(add), LPARAM(&mut list as *mut Vec<Rect> as isize));
    }
    list
}

/// Windows thinner than this (physical px) in either direction are never picked.
const MIN_PICK: i32 = 24;

/// Where `hwnd` appears on screen, or None if the user can't see it as a window.
unsafe fn window_rect(hwnd: HWND) -> Option<Rect> {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        // Click-through windows are overlays (GPU, game, recording), not something to pick.
        if ex & WS_EX_TRANSPARENT.0 != 0 {
            return None;
        }
        // Fully transparent layered windows (e.g. the idle GeForce overlay) can't be seen.
        if ex & WS_EX_LAYERED.0 != 0 {
            let (mut alpha, mut flags) = (255u8, LAYERED_WINDOW_ATTRIBUTES_FLAGS(0));
            if GetLayeredWindowAttributes(hwnd, None, Some(&mut alpha), Some(&mut flags)).is_ok()
                && flags.contains(LWA_ALPHA)
                && alpha == 0
            {
                return None;
            }
        }
        // Cloaked: on another virtual desktop, a suspended UWP app, or a shell surface.
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as _, 4);
        if cloaked != 0 {
            return None;
        }
        // The desktop itself; hovering it highlights the whole monitor instead.
        let mut buf = [0u16; 16];
        let n = GetClassNameW(hwnd, &mut buf).max(0) as usize;
        let class = &buf[..n];
        if [w!("Progman"), w!("WorkerW")].iter().any(|c| c.as_wide() == class) {
            return None;
        }
        let mut rc = RECT::default();
        let size = size_of::<RECT>() as u32;
        if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut rc as *mut RECT as _, size).is_err() {
            GetWindowRect(hwnd, &mut rc).ok()?;
        }
        let r = Rect::from_win(&rc);
        // Slivers are drop shadows, borders and helper windows, not pick targets.
        (r.w >= MIN_PICK && r.h >= MIN_PICK).then_some(r)
    }
}
