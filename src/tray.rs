//! Notification-area icon (idle / recording) and balloon notifications.

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject, GetDC, ReleaseDC,
};
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, DestroyIcon, HICON, ICONINFO, SM_CXSMICON};

use crate::util::copy_wstr;

pub struct Tray {
    hwnd: HWND,
    callback: u32,
    idle: HICON,
    rec: HICON,
    recording: bool,
}

impl Tray {
    pub fn new(hwnd: HWND, callback: u32) -> Tray {
        let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()) }.max(16) * 2;
        let tray = Tray {
            hwnd,
            callback,
            idle: make_icon(size, [235, 235, 235]),
            rec: make_icon(size, [235, 45, 45]),
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
    pub fn add(&self) {
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

    pub fn set_recording(&mut self, on: bool, tip: &str) {
        self.recording = on;
        let mut d = self.data();
        d.uFlags = NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        d.hIcon = if on { self.rec } else { self.idle };
        copy_wstr(&mut d.szTip, tip);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        }
    }

    pub fn notify(&self, title: &str, text: &str, error: bool) {
        let mut d = self.data();
        d.uFlags = NIF_INFO;
        copy_wstr(&mut d.szInfoTitle, title);
        copy_wstr(&mut d.szInfo, text);
        d.dwInfoFlags = if error { NIIF_ERROR } else { NIIF_INFO };
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        }
    }

    pub fn remove(&self) {
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

/// "Record" glyph: a ring with a dot, anti-aliased, with a thin dark outline so it reads on
/// both light and dark taskbars.
fn make_icon(size: i32, rgb: [u8; 3]) -> HICON {
    let s = size as f32;
    let c = s / 2.0;
    let (r_out, r_in, r_dot) = (s * 0.46, s * 0.34, s * 0.2);
    let disc = |d: f32, r: f32| (r - d + 0.5).clamp(0.0, 1.0);
    let shape = |d: f32, grow: f32| {
        let ring = (disc(d, r_out + grow) - disc(d, r_in - grow)).max(0.0);
        ring.max(disc(d, r_dot + grow))
    };
    let mut px = vec![0u32; (size * size) as usize];
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 + 0.5 - c).powi(2) + (y as f32 + 0.5 - c).powi(2)).sqrt();
            let fill = shape(d, 0.0);
            let outline = (shape(d, s / 32.0 + 0.6) - fill).max(0.0) * 0.55;
            let a = fill + outline * (1.0 - fill);
            if a <= 0.0 {
                continue;
            }
            let k = fill / a; // straight alpha: color share vs. black outline
            let ch = |v: u8| (v as f32 * k).round() as u32;
            px[(y * size + x) as usize] =
                ((a * 255.0).round() as u32) << 24 | ch(rgb[0]) << 16 | ch(rgb[1]) << 8 | ch(rgb[2]);
        }
    }
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
