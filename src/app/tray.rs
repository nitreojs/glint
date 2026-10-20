//! Notification-area icon (idle / recording) and balloon notifications.

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject, GetDC, ReleaseDC,
};
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, DestroyIcon, HICON, ICONINFO, SM_CXSMICON};

use crate::platform::wide::copy_wstr;

pub(crate) struct Tray {
    hwnd: HWND,
    callback: u32,
    idle: HICON,
    rec: HICON,
    recording: bool,
}

impl Tray {
    pub(crate) fn new(hwnd: HWND, callback: u32) -> Tray {
        // Drawn at the exact small-icon size (16 / 20 / 24 / 32 px), not scaled down.
        let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()) }.max(16);
        let tray = Tray {
            hwnd,
            callback,
            idle: make_icon(size, true),
            rec: make_icon(size, false),
            recording: false,
        };
        tray.add();
        tray
    }

    fn data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            ..Default::default()
        }
    }

    /// Adds the icon; also used after Explorer restarts (TaskbarCreated).
    pub(crate) fn add(&self) {
        let mut d = self.data();
        d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
        d.uCallbackMessage = self.callback;
        d.hIcon = if self.recording { self.rec } else { self.idle };
        copy_wstr(&mut d.szTip, if self.recording { "glint: recording" } else { "glint" });
        d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &d);
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &d);
        }
    }

    pub(crate) fn set_recording(&mut self, on: bool, tip: &str) {
        self.recording = on;
        let mut d = self.data();
        d.uFlags = NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        d.hIcon = if on { self.rec } else { self.idle };
        copy_wstr(&mut d.szTip, tip);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        }
    }

    pub(crate) fn notify(&self, title: &str, text: &str, error: bool) {
        let mut d = self.data();
        d.uFlags = NIF_INFO;
        copy_wstr(&mut d.szInfoTitle, title);
        copy_wstr(&mut d.szInfo, text);
        d.dwInfoFlags = if error { NIIF_ERROR } else { NIIF_INFO };
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        }
    }

    pub(crate) fn remove(&self) {
        let d = self.data();
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &d);
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
        unsafe {
            let _ = DestroyIcon(self.idle);
            let _ = DestroyIcon(self.rec);
        }
    }
}

/// The glint mark as straight-alpha ARGB pixels: a red disc with a white highlight in the
/// upper right ("a glint on a lens"); without the highlight it's the plain recording dot.
/// Geometry is snapped to whole pixels so it stays crisp at 16 px.
pub(crate) fn icon_pixels(size: i32, highlight: bool) -> Vec<u32> {
    const RED: [u8; 3] = [0xE5, 0x40, 0x2F];
    let s = size as f32;
    let unit = |n: f32| (n * s / 16.0).round();
    let (c, r) = (s / 2.0, s / 2.0 - unit(1.0).max(1.0)); // 14 px disc on a 16 px grid
    // 3 px highlight whose box ends 2 px inside the disc's top-right corner.
    let hd = unit(3.0);
    let (hx, hy) = (s - unit(3.0) - hd / 2.0, unit(3.0) + hd / 2.0);
    let cover = |d: f32, r: f32| (r - d + 0.5).clamp(0.0, 1.0);
    let mut px = vec![0u32; (size * size) as usize];
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let a = cover(((fx - c).powi(2) + (fy - c).powi(2)).sqrt(), r);
            if a <= 0.0 {
                continue;
            }
            let w = if highlight { cover(((fx - hx).powi(2) + (fy - hy).powi(2)).sqrt(), hd / 2.0) } else { 0.0 };
            let ch = |v: u8| (v as f32 + (255.0 - v as f32) * w).round() as u32;
            px[(y * size + x) as usize] =
                ((a * 255.0).round() as u32) << 24 | ch(RED[0]) << 16 | ch(RED[1]) << 8 | ch(RED[2]);
        }
    }
    px
}

fn make_icon(size: i32, highlight: bool) -> HICON {
    let px = icon_pixels(size, highlight);
    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let screen = GetDC(None);
        let mut bits = std::ptr::null_mut();
        let color = CreateDIBSection(Some(screen), &bmi, DIB_RGB_COLORS, &mut bits, None, 0);
        ReleaseDC(None, screen);
        let Ok(color) = color else { return HICON::default() };
        std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u32, px.len());
        let mask = CreateBitmap(size, size, 1, 1, None);
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info).unwrap_or_default();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}
