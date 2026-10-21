//! Notification-area icon (idle / recording) and balloon notifications.

use glint_core::icon::{RECORDING_RED, add_badge, icon_pixels};
use glint_core::wide::copy_wstr;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject, GetDC, ReleaseDC,
};
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, DestroyIcon, HICON, ICONINFO, SM_CXSMICON};

pub(crate) const IDLE_TIP: &str = "glint\nclick: screenshot · double-click: settings";
const READY_MARK: &str = " (update ready)";

pub(crate) struct Tray {
    hwnd: HWND,
    callback: u32,
    size: i32,
    idle: HICON,
    rec: HICON,
    recording: bool,
    accent: [u8; 3],
    /// A downloaded update waits: the idle icon has a dot and the tooltip says so.
    update_ready: bool,
}

impl Tray {
    pub(crate) fn new(hwnd: HWND, callback: u32, accent: [u8; 3]) -> Tray {
        // Drawn at the exact small-icon size (16 / 20 / 24 / 32 px), not scaled down.
        let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()) }.max(16);
        let tray = Tray {
            hwnd,
            callback,
            size,
            idle: make_icon(size, true, accent, false),
            rec: make_icon(size, false, RECORDING_RED, false),
            recording: false,
            accent,
            update_ready: false,
        };
        tray.add();
        tray
    }

    /// Redraws the idle icon in a new accent colour.
    pub(crate) fn set_accent(&mut self, accent: [u8; 3]) {
        self.accent = accent;
        self.redraw_idle();
    }

    /// Marks the icon and tooltip while a downloaded update waits for a restart.
    pub(crate) fn set_update_ready(&mut self, ready: bool) {
        if self.update_ready == ready {
            return;
        }
        self.update_ready = ready;
        self.redraw_idle();
        if !self.recording {
            self.set_idle_tip(IDLE_TIP);
        }
    }

    fn redraw_idle(&mut self) {
        let old = std::mem::replace(&mut self.idle, make_icon(self.size, true, self.accent, self.update_ready));
        if !self.recording {
            let mut d = self.data();
            d.uFlags = NIF_ICON;
            d.hIcon = self.idle;
            unsafe {
                let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
            }
        }
        unsafe {
            let _ = DestroyIcon(old);
        }
    }

    /// `tip` with the update mark after its first line.
    fn idle_tip(&self, tip: &str) -> String {
        tip_with_mark(tip, self.update_ready)
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
        let tip = if self.recording { "glint: recording".to_string() } else { self.idle_tip(IDLE_TIP) };
        copy_wstr(&mut d.szTip, &tip);
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
        copy_wstr(&mut d.szTip, &if on { tip.to_string() } else { self.idle_tip(tip) });
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        }
    }

    pub(crate) fn set_idle_tip(&self, tip: &str) {
        if self.recording {
            return;
        }
        let mut d = self.data();
        d.uFlags = NIF_TIP | NIF_SHOWTIP;
        copy_wstr(&mut d.szTip, &self.idle_tip(tip));
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


fn tip_with_mark(tip: &str, ready: bool) -> String {
    if !ready {
        return tip.to_string();
    }
    match tip.split_once('\n') {
        Some((first, rest)) => format!("{first}{READY_MARK}\n{rest}"),
        None => format!("{tip}{READY_MARK}"),
    }
}

fn make_icon(size: i32, highlight: bool, color: [u8; 3], badge: bool) -> HICON {
    let mut px = icon_pixels(size, highlight, color);
    if badge {
        add_badge(&mut px, size, color);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_waiting_update_shows_in_the_tooltip() {
        assert_eq!(tip_with_mark(IDLE_TIP, false), IDLE_TIP);
        assert_eq!(tip_with_mark(IDLE_TIP, true), "glint (update ready)\nclick: screenshot · double-click: settings");
        assert_eq!(tip_with_mark("glint: uploading 40 %", true), "glint: uploading 40 % (update ready)");
    }
}
