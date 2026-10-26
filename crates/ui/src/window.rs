use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use eframe::egui::{Color32, Context, Modal, Rect, Stroke, StrokeKind, Ui};
use glint_core::wide::from_wstr;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromRect};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, GWLP_WNDPROC, GetWindowRect, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCLIENT, HTLEFT,
    HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, IsIconic, IsZoomed, SW_RESTORE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_NCCALCSIZE,
    WM_NCHITTEST, WM_PAINT, WNDPROC,
};
use windows::core::w;

pub use glint_shell::window::find;

mod shadow;
mod shot;

pub use shot::{dropped_file, shots};

const EDGE: f32 = 5.0;
const CORNER: f32 = 14.0;
/// The window corners' radius, Windows 11's own.
const RADIUS: u8 = 8;

static WINIT_PROC: AtomicIsize = AtomicIsize::new(0);
static RESIZABLE: AtomicBool = AtomicBool::new(false);
static ROUNDED: AtomicBool = AtomicBool::new(false);
/// Windows 11 rounds windows itself; Windows 10 leaves them square.
static WINDOWS_11: LazyLock<bool> = LazyLock::new(|| windows_build() >= 22000);

/// Whether a frameless window is built transparent (`ViewportBuilder::with_transparent`): on
/// Windows 10, which keeps windows square, it paints its own rounded corners.
pub fn transparent() -> bool {
    !*WINDOWS_11
}

/// Makes an undecorated egui window's client area cover the whole window, gives it a shadow, lets
/// a resizable one be resized from its edges, and rounds it: Windows 11 does it, on Windows 10 the
/// window, built [`transparent`], is painted rounded (see [`radius`]) and its shadow is ours.
pub fn frameless(hwnd: HWND, resizable: bool) {
    RESIZABLE.store(resizable, Ordering::Relaxed);
    unsafe {
        let winit = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, frame_proc as *const () as isize);
        WINIT_PROC.store(winit, Ordering::Relaxed);
        if *WINDOWS_11 {
            // dwm draws no shadow around a window without a frame unless some frame reaches into the
            // client area; on windows 10 that frame shows as a square 1 px outline past round corners
            let margins = MARGINS { cxLeftWidth: 1, cxRightWidth: 1, cyTopHeight: 1, cyBottomHeight: 1 };
            let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        }
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED;
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, flags);
    }
    if *WINDOWS_11 {
        ROUNDED.store(round(hwnd), Ordering::Relaxed);
    } else {
        shadow::attach(hwnd);
        ROUNDED.store(true, Ordering::Relaxed);
    }
}

/// Call from `eframe::App::on_exit`: the window is going, and so is the shadow drawn around it.
pub fn closing() {
    if !*WINDOWS_11 {
        shadow::end();
    }
}

/// The radius of the window's own corners, to paint whatever reaches them with: 0 while it's
/// square or maximized. On Windows 10 anything square there shows past the corners.
pub fn radius(ctx: &Context) -> u8 {
    let maximized = ctx.input(|i| i.viewport().maximized).unwrap_or(false);
    if ROUNDED.load(Ordering::Relaxed) && !maximized { RADIUS } else { 0 }
}

/// The 1 px outline of the window `full`, painted last; Windows 11 draws its own.
pub fn border(ui: &Ui, full: Rect, color: Color32) {
    let radius = radius(ui.ctx());
    if !*WINDOWS_11 && radius > 0 {
        ui.painter().rect_stroke(full, radius, Stroke::new(1.0, color), StrokeKind::Inside);
    }
}

/// `modal` with a `color` backdrop that keeps to the window's corners (egui's own is square);
/// call it right before showing the modal.
pub fn backdrop(ctx: &Context, modal: Modal, color: Color32) -> Modal {
    ctx.layer_painter(modal.area.layer()).rect_filled(ctx.content_rect(), radius(ctx), color);
    modal.backdrop_color(Color32::TRANSPARENT)
}

unsafe extern "system" fn frame_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let winit: WNDPROC = unsafe { std::mem::transmute(WINIT_PROC.load(Ordering::Relaxed)) };
    let zoomed = unsafe { IsZoomed(hwnd).as_bool() };
    if !*WINDOWS_11 {
        shadow::message(msg, wp, lp);
    }
    if shot::message(msg, wp, lp) {
        return LRESULT(0);
    }
    match msg {
        // winit 0.30 keeps a 1 px non-client strip at the top for the shadow, which win10 paints as a light line
        WM_NCCALCSIZE if wp.0 != 0 && !zoomed => return LRESULT(0),
        WM_NCHITTEST if RESIZABLE.load(Ordering::Relaxed) && !zoomed => {
            let mut r = RECT::default();
            if unsafe { GetWindowRect(hwnd, &mut r) }.is_ok() {
                let (x, y) = ((lp.0 & 0xFFFF) as i16 as i32, (lp.0 >> 16 & 0xFFFF) as i16 as i32);
                let scale = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32 / 96.0;
                let (band, corner) = ((EDGE * scale).round() as i32, (CORNER * scale).round() as i32);
                let hit = edge_hit(x - r.left, y - r.top, r.right - r.left, r.bottom - r.top, band, corner);
                if hit != HTCLIENT {
                    return LRESULT(hit as isize);
                }
            }
        }
        _ => {}
    }
    let result = unsafe { CallWindowProcW(winit, hwnd, msg, wp, lp) };
    if msg == WM_PAINT && !*WINDOWS_11 {
        // winit draws the frame right in WM_PAINT
        shadow::painted();
    }
    result
}

/// The resize border under a window-relative point of a `w` x `h` window, or HTCLIENT.
fn edge_hit(x: i32, y: i32, w: i32, h: i32, band: i32, corner: i32) -> u32 {
    let (left, right, top, bottom) = (x < band, x >= w - band, y < band, y >= h - band);
    let (near_left, near_right, near_top, near_bottom) = (x < corner, x >= w - corner, y < corner, y >= h - corner);
    if top && near_left || left && near_top {
        HTTOPLEFT
    } else if top && near_right || right && near_top {
        HTTOPRIGHT
    } else if bottom && near_left || left && near_bottom {
        HTBOTTOMLEFT
    } else if bottom && near_right || right && near_bottom {
        HTBOTTOMRIGHT
    } else if left {
        HTLEFT
    } else if right {
        HTRIGHT
    } else if top {
        HTTOP
    } else if bottom {
        HTBOTTOM
    } else {
        HTCLIENT
    }
}

fn windows_build() -> u32 {
    let mut buf = [0u16; 16];
    let mut bytes = size_of_val(&buf) as u32;
    let key = w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    unsafe {
        let _ = RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key,
            w!("CurrentBuildNumber"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut bytes),
        );
    }
    from_wstr(&buf).parse().unwrap_or(0)
}

/// Asks Windows 11 to round the window; returns whether it did.
fn round(hwnd: HWND) -> bool {
    let pref = DWMWCP_ROUND;
    let set = unsafe {
        DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &pref as *const _ as _, size_of_val(&pref) as u32)
    };
    set.is_ok()
}

/// Where an app window sits as `[x, y, w, h]`, to open there next time; `None` while it's minimized
/// or maximized.
pub fn placement(hwnd: HWND) -> Option<[i32; 4]> {
    unsafe {
        if IsIconic(hwnd).as_bool() || IsZoomed(hwnd).as_bool() {
            return None;
        }
        let mut r = RECT::default();
        GetWindowRect(hwnd, &mut r).ok()?;
        Some([r.left, r.top, r.right - r.left, r.bottom - r.top])
    }
}

/// Moves a window back to a [`placement`], unless no monitor shows that place any more.
pub fn place(hwnd: HWND, [x, y, w, h]: [i32; 4]) {
    let r = RECT { left: x, top: y, right: x + w, bottom: y + h };
    unsafe {
        if !MonitorFromRect(&r, MONITOR_DEFAULTTONULL).is_invalid() {
            let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
}

/// Brings a window to the front, restored if it was minimized; not in [`quiet`] mode.
pub fn bring_forward(hwnd: HWND) {
    if quiet() {
        return;
    }
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Test runs (`GLINT_QUIET=1`): our windows open without taking the focus and nothing is brought
/// to the front, so tests never steal it from the user.
pub fn quiet() -> bool {
    std::env::var_os("GLINT_QUIET").is_some_and(|v| v == "1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_and_corners_resize() {
        let hit = |x, y| edge_hit(x, y, 680, 600, 5, 14);
        assert_eq!(hit(300, 300), HTCLIENT);
        assert_eq!(hit(0, 300), HTLEFT);
        assert_eq!(hit(4, 300), HTLEFT);
        assert_eq!(hit(5, 300), HTCLIENT);
        assert_eq!(hit(679, 300), HTRIGHT);
        assert_eq!(hit(300, 0), HTTOP);
        assert_eq!(hit(300, 599), HTBOTTOM);
        assert_eq!(hit(0, 0), HTTOPLEFT);
        assert_eq!(hit(10, 2), HTTOPLEFT);
        assert_eq!(hit(2, 10), HTTOPLEFT);
        assert_eq!(hit(670, 3), HTTOPRIGHT);
        assert_eq!(hit(3, 590), HTBOTTOMLEFT);
        assert_eq!(hit(679, 599), HTBOTTOMRIGHT);
        assert_eq!(hit(20, 2), HTTOP);
    }
}
