//! Pinned screenshots: borderless topmost windows showing a shot where it was taken. Drag to
//! move, wheel to zoom (Ctrl+wheel: opacity), double-click or Esc to close, right-click for a
//! menu. Pins live on the thread that opens them; copy and save go back to the owner.

use std::cell::RefCell;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, GdiFlush};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

use super::Shot;
use super::editor::Output;
use crate::platform::gfx::Dib;
use crate::platform::log::log;

const BORDER: i32 = 1;
/// Palette blue, opaque.
const BORDER_COLOR: u32 = 0xFF0A_84FF;
const MAX_ZOOM: f32 = 8.0;
const MAX_PIXELS: f32 = 64.0e6;
const MIN_SIDE: f32 = 16.0;
const MIN_OPACITY: f32 = 0.2;
const ZOOM_STEP: f32 = 1.1;
const OPACITY_STEP: f32 = 0.1;
const WHEEL_DELTA: f32 = 120.0;
const MK_CONTROL: usize = 0x0008;
const ID_COPY: usize = 1;
const ID_SAVE: usize = 2;
const ID_CLOSE: usize = 3;

struct Pin {
    hwnd: HWND,
    shot: Shot,
    /// The shot as premultiplied 0xAARRGGBB, what a layered window shows.
    premul: Vec<u32>,
    zoom: f32,
    opacity: f32,
    owner: HWND,
    msg: u32,
}

thread_local! {
    static PINS: RefCell<Vec<Pin>> = const { RefCell::new(Vec::new()) };
}

fn premultiply(p: u32, alpha: bool) -> u32 {
    if !alpha {
        return p | 0xFF00_0000;
    }
    let a = p >> 24;
    let ch = |shift: u32| ((((p >> shift) & 0xFF) * a + 127) / 255) << shift;
    (a << 24) | ch(16) | ch(8) | ch(0)
}

/// Resamples `src` (`w`×`h`) into `dst` rows of `stride`, starting at `off`, as `dw`×`dh`:
/// nearest pixel when growing (crisp), box average when shrinking.
fn resample(src: &[u32], (w, h): (usize, usize), dst: &mut [u32], stride: usize, off: usize, (dw, dh): (usize, usize)) {
    let span = |d: usize, n: usize, dn: usize| (d * n / dn, ((d + 1) * n).div_ceil(dn).max(d * n / dn + 1).min(n));
    for dy in 0..dh {
        let (y0, y1) = span(dy, h, dh);
        let row = &mut dst[off + dy * stride..off + dy * stride + dw];
        for (dx, out) in row.iter_mut().enumerate() {
            let (x0, x1) = span(dx, w, dw);
            if dw >= w && dh >= h {
                *out = src[y0 * w + x0];
                continue;
            }
            let mut sum = [0u32; 4];
            for y in y0..y1 {
                for &p in &src[y * w + x0..y * w + x1] {
                    for (i, s) in sum.iter_mut().enumerate() {
                        *s += (p >> (8 * i)) & 0xFF;
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            *out = sum.iter().enumerate().fold(0, |acc, (i, s)| acc | ((s + n / 2) / n) << (8 * i));
        }
    }
}

impl Pin {
    fn size(&self) -> (i32, i32) {
        let (w, h) = (self.shot.w as f32 * self.zoom, self.shot.h as f32 * self.zoom);
        (w.round().max(1.0) as i32, h.round().max(1.0) as i32)
    }

    /// The zoom range that keeps the window between a usable and a sane size.
    fn zoom_limits(&self) -> (f32, f32) {
        let (w, h) = (self.shot.w as f32, self.shot.h as f32);
        let max = MAX_ZOOM.min((MAX_PIXELS / (w * h)).sqrt()).max(1.0);
        let min = (MIN_SIDE / w.min(h)).min(1.0);
        (min, max)
    }

    /// Redraws the window with its top-left corner at `pos`.
    fn show(&self, pos: (i32, i32)) -> Result<(), String> {
        let (w, h) = self.size();
        let (ow, oh) = (w + 2 * BORDER, h + 2 * BORDER);
        let mut dib = Dib::new(ow, oh)?;
        let px = dib.pixels_mut();
        px.fill(BORDER_COLOR);
        let stride = ow as usize;
        let src = (self.shot.w as usize, self.shot.h as usize);
        resample(&self.premul, src, px, stride, BORDER as usize * (stride + 1), (w as usize, h as usize));
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: (self.opacity * 255.0).round() as u8,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            let _ = GdiFlush();
            UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&POINT { x: pos.0, y: pos.1 }),
                Some(&SIZE { cx: ow, cy: oh }),
                Some(dib.dc),
                Some(&POINT::default()),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
            .map_err(|e| format!("UpdateLayeredWindow: {e}"))
        }
    }

    fn origin(&self) -> (i32, i32) {
        let mut r = RECT::default();
        let _ = unsafe { GetWindowRect(self.hwnd, &mut r) };
        (r.left, r.top)
    }

    /// Zooms by `notches` wheel notches, keeping the image point under `cursor` in place.
    fn zoom_by(&mut self, notches: f32, cursor: (i32, i32)) -> Result<(), String> {
        let (min, max) = self.zoom_limits();
        let zoom = (self.zoom * ZOOM_STEP.powf(notches)).clamp(min, max);
        if zoom == self.zoom {
            return Ok(());
        }
        let (x, y) = self.origin();
        let k = zoom / self.zoom;
        let anchor = |c: i32, o: i32| c - ((c - o - BORDER) as f32 * k).round() as i32 - BORDER;
        self.zoom = zoom;
        self.show((anchor(cursor.0, x), anchor(cursor.1, y)))
    }

    fn fade_by(&mut self, notches: f32) -> Result<(), String> {
        self.opacity = (self.opacity + notches * OPACITY_STEP).clamp(MIN_OPACITY, 1.0);
        self.show(self.origin())
    }

    /// Hands a copy of the shot to the owner (copy / save), like the editor's buttons.
    fn send(&self, output: Output) {
        let s = &self.shot;
        let shot = Shot { pixels: s.pixels.clone(), w: s.w, h: s.h, alpha: s.alpha, at: s.at, output };
        unsafe {
            let boxed = Box::into_raw(Box::new(shot));
            if PostMessageW(Some(self.owner), self.msg, WPARAM(0), LPARAM(boxed as isize)).is_err() {
                drop(Box::from_raw(boxed));
            }
        }
    }
}

/// Pins `shot` over the desktop where it was taken. Copy and save from its menu post a
/// `Box<Shot>` (output `Copy` / `Save`) in LPARAM of `msg` to `owner`.
pub(crate) fn open(shot: Shot, owner: HWND, msg: u32) -> Result<(), String> {
    unsafe {
        let hinst = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = w!("glint_pin");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_SIZEALL).unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc); // fails harmlessly when already registered
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            class,
            w!("glint pin"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .map_err(|e| format!("CreateWindowEx: {e}"))?;
        let premul = shot.pixels.iter().map(|&p| premultiply(p, shot.alpha)).collect();
        let pos = (shot.at.x - BORDER, shot.at.y - BORDER);
        let pin = Pin { hwnd, shot, premul, zoom: 1.0, opacity: 1.0, owner, msg };
        if let Err(e) = pin.show(pos) {
            let _ = DestroyWindow(hwnd);
            return Err(e);
        }
        PINS.with(|p| p.borrow_mut().push(pin));
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        Ok(())
    }
}

/// Runs `f` on the pin behind `hwnd`.
fn with_pin<R>(hwnd: HWND, f: impl FnOnce(&mut Pin) -> R) -> Option<R> {
    PINS.with(|p| p.try_borrow_mut().ok().and_then(|mut v| v.iter_mut().find(|p| p.hwnd == hwnd).map(f)))
}

fn close(hwnd: HWND) {
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

fn menu(hwnd: HWND, at: POINT) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(menu, MF_STRING, ID_COPY, w!("Copy"));
        let _ = AppendMenuW(menu, MF_STRING, ID_SAVE, w!("Save"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_CLOSE, w!("Close"));
        let _ = SetForegroundWindow(hwnd);
        let flags = TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY;
        let picked = TrackPopupMenu(menu, flags, at.x, at.y, None, hwnd, None).0 as usize;
        let _ = DestroyMenu(menu);
        match picked {
            ID_COPY => {
                with_pin(hwnd, |p| p.send(Output::Copy));
            }
            ID_SAVE => {
                with_pin(hwnd, |p| p.send(Output::Save));
            }
            ID_CLOSE => close(hwnd),
            _ => {}
        }
    }
}

fn screen_point(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        // the whole window is a caption: dragging moves it, double-clicking reaches WM_NCLBUTTONDBLCLK
        WM_NCHITTEST => return LRESULT(HTCAPTION as isize),
        WM_NCLBUTTONDBLCLK => close(hwnd),
        WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => close(hwnd),
        WM_NCRBUTTONDOWN => {}
        WM_NCRBUTTONUP => {
            let at = screen_point(lp);
            menu(hwnd, POINT { x: at.0, y: at.1 });
        }
        WM_CONTEXTMENU => {
            let at = match screen_point(lp) {
                (-1, -1) => with_pin(hwnd, |p| p.origin()).unwrap_or_default(),
                at => at,
            };
            menu(hwnd, POINT { x: at.0, y: at.1 });
        }
        WM_MOUSEWHEEL => {
            let notches = ((wp.0 >> 16) & 0xFFFF) as i16 as f32 / WHEEL_DELTA;
            let ctrl = wp.0 & MK_CONTROL != 0;
            let cursor = screen_point(lp);
            let shown = with_pin(hwnd, |p| if ctrl { p.fade_by(notches) } else { p.zoom_by(notches, cursor) })
                .unwrap_or(Ok(()));
            if let Err(e) = shown {
                log(&format!("pin: {e}"));
            }
        }
        WM_DESTROY => {
            PINS.with(|p| p.borrow_mut().retain(|p| p.hwnd != hwnd));
        }
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
    LRESULT(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplies_only_with_alpha() {
        assert_eq!(premultiply(0x0012_3456, false), 0xFF12_3456);
        assert_eq!(premultiply(0x80FF_8000, true), 0x8080_4000);
        assert_eq!(premultiply(0x00FF_FFFF, true), 0);
    }

    #[test]
    fn resampling_grows_crisp_and_shrinks_by_average() {
        let src = [0xFF00_0000, 0xFF00_00FF, 0xFF00_00FF, 0xFF00_0000];
        let mut big = vec![0; 16];
        resample(&src, (2, 2), &mut big, 4, 0, (4, 4));
        assert_eq!(&big[..4], &[0xFF00_0000, 0xFF00_0000, 0xFF00_00FF, 0xFF00_00FF]);
        assert_eq!(&big[12..], &[0xFF00_00FF, 0xFF00_00FF, 0xFF00_0000, 0xFF00_0000]);
        let mut small = vec![0; 3];
        resample(&src, (2, 2), &mut small, 3, 1, (1, 1));
        assert_eq!(small, [0, 0xFF00_0080, 0]);
    }
}
