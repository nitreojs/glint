//! A notification-area icon: its picture, tooltip and balloon notifications. Clicks arrive at the
//! window as the `callback` message (version 4: the event in the low word of LPARAM).

use glint_core::wide::copy_wstr;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject, GetDC, ReleaseDC,
};
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, DestroyIcon, HICON, ICONINFO, SM_CXSMICON};

/// The tray's icon size in pixels (16 / 20 / 24 / 32), so pictures are drawn exactly, not scaled.
pub fn icon_size() -> i32 {
    unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()) }.max(16)
}

/// An icon from `size`×`size` premultiplied 0xAARRGGBB pixels; destroy it with `DestroyIcon`.
pub fn make_icon(size: i32, px: &[u32]) -> HICON {
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
        let n = px.len().min((size * size) as usize);
        std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u32, n);
        let mask = CreateBitmap(size, size, 1, 1, None);
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info).unwrap_or_default();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}

/// The icon in the notification area; removed when dropped. It doesn't own the pictures it shows.
pub struct TrayIcon {
    hwnd: HWND,
    callback: u32,
    icon: HICON,
    tip: String,
}

impl TrayIcon {
    pub fn new(hwnd: HWND, callback: u32, icon: HICON, tip: &str) -> TrayIcon {
        let tray = TrayIcon { hwnd, callback, icon, tip: tip.to_string() };
        tray.add();
        tray
    }

    fn data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: self.hwnd, uID: 1, ..Default::default() }
    }

    /// Adds the icon; also after Explorer restarts (the `TaskbarCreated` message).
    pub fn add(&self) {
        let mut d = self.data();
        d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
        d.uCallbackMessage = self.callback;
        d.hIcon = self.icon;
        copy_wstr(&mut d.szTip, &self.tip);
        d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &d);
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &d);
        }
    }

    /// Shows `icon` and `tip`; either unchanged is left alone.
    pub fn show(&mut self, icon: HICON, tip: &str) {
        let mut d = self.data();
        if icon != self.icon {
            self.icon = icon;
            d.uFlags |= NIF_ICON;
            d.hIcon = icon;
        }
        if tip != self.tip {
            self.tip = tip.to_string();
            d.uFlags |= NIF_TIP | NIF_SHOWTIP;
            copy_wstr(&mut d.szTip, tip);
        }
        if d.uFlags.0 != 0 {
            unsafe {
                let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
            }
        }
    }

    pub fn icon(&self) -> HICON {
        self.icon
    }

    pub fn tip(&self) -> &str {
        &self.tip
    }

    /// A balloon (a toast on Windows 10 and 11).
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

impl Drop for TrayIcon {
    fn drop(&mut self) {
        self.remove();
    }
}

/// Destroys `icon` (made by [`make_icon`]).
pub fn destroy(icon: HICON) {
    unsafe {
        let _ = DestroyIcon(icon);
    }
}
