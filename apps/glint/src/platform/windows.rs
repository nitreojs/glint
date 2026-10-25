use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use glint_core::geom::fit::MIN_PICK;
use glint_core::geom::rect::Rect;
use glint_media::rect::WinRect;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, TRUE};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{ClientToScreen, MONITOR_DEFAULTTONULL, MonitorFromRect};
use windows::Win32::UI::Shell::{
    FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW, SHFileOperationW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetClassNameW, GetClientRect, GetLayeredWindowAttributes, GetWindowLongW, GetWindowRect,
    IsIconic, IsWindowVisible, IsZoomed, LAYERED_WINDOW_ATTRIBUTES_FLAGS, LWA_ALPHA, SWP_NOACTIVATE, SWP_NOZORDER,
    SetWindowPos, WS_EX_LAYERED, WS_EX_TRANSPARENT,
};
use windows::core::{BOOL, PCWSTR, w};

/// Where an app window sits as `[x, y, w, h]`, to open there next time; `None` while it's minimized
/// or maximized.
pub(crate) fn window_placement(hwnd: HWND) -> Option<[i32; 4]> {
    unsafe {
        if IsIconic(hwnd).as_bool() || IsZoomed(hwnd).as_bool() {
            return None;
        }
        let mut r = RECT::default();
        GetWindowRect(hwnd, &mut r).ok()?;
        Some([r.left, r.top, r.right - r.left, r.bottom - r.top])
    }
}

/// Moves a window back to a [`window_placement`], unless no monitor shows that place any more.
pub(crate) fn place(hwnd: HWND, [x, y, w, h]: [i32; 4]) {
    let r = RECT { left: x, top: y, right: x + w, bottom: y + h };
    unsafe {
        if !MonitorFromRect(&r, MONITOR_DEFAULTTONULL).is_invalid() {
            let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
}

/// Moves `path` to the Recycle Bin.
pub(crate) fn recycle(owner: HWND, path: &Path) -> Result<(), String> {
    let from: Vec<u16> = path.as_os_str().encode_wide().chain([0, 0]).collect();
    let mut op = SHFILEOPSTRUCTW {
        hwnd: owner,
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT).0 as u16,
        ..Default::default()
    };
    match unsafe { SHFileOperationW(&mut op) } {
        0 if !op.fAnyOperationsAborted.as_bool() => Ok(()),
        0 => Err("the deletion was cancelled".into()),
        code => Err(format!("windows couldn't move it to the recycle bin (error {code:#x})")),
    }
}

/// Visible top-level windows, topmost first, by their visible DWM frame (without the
/// invisible resize borders). Taken together with the screenshot, so hovering matches it.
pub(crate) fn visible_windows() -> Vec<(HWND, Rect)> {
    unsafe extern "system" fn add(hwnd: HWND, lp: LPARAM) -> BOOL {
        let list = unsafe { &mut *(lp.0 as *mut Vec<(HWND, Rect)>) };
        list.extend(unsafe { window_rect(hwnd) }.map(|r| (hwnd, r)));
        TRUE
    }
    let mut list = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(add), LPARAM(&mut list as *mut Vec<(HWND, Rect)> as isize));
    }
    list
}

/// The client area of `hwnd` in screen coordinates, or None if it's empty.
pub(crate) fn client_rect(hwnd: HWND) -> Option<Rect> {
    let mut rc = RECT::default();
    let mut origin = POINT::default();
    unsafe {
        GetClientRect(hwnd, &mut rc).ok()?;
        ClientToScreen(hwnd, &mut origin).ok().ok()?;
    }
    let r = Rect::from_win(&rc).offset(origin.x, origin.y);
    (r.w > 0 && r.h > 0).then_some(r)
}

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
