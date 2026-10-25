//! The note over everything when a clip is saved, like nvidia's instant replay: a pill in a
//! corner (or at the top or bottom middle) of the recorded monitor that fades in, says what happened and fades out. Clicks go
//! through it, it never takes focus, and it's kept out of capture, so the buffer never has it;
//! glint's screenshots let it in ([`set_capturable`]).
//!
//! It's a DWM glass window (per-pixel alpha from Direct2D) that's also layered, for the fade and
//! the click-through: capture exclusion refuses windows drawn with `UpdateLayeredWindow`.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use glint_core::geom::rect::Rect;
use glint_core::log::log;
use crate::config::Place;
use glint_shell::gfx::{Canvas, Painter, make_glass, rgba};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint};
use windows::Win32::Graphics::Direct2D::ID2D1HwndRenderTarget;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

const TIMER: usize = 1;
const FADE_IN: f32 = 0.12;
const FADE_OUT: f32 = 0.3;
/// How long "saved" stays; "saving" stays until it's replaced (or this long, should a save hang).
const SHOWN: Duration = Duration::from_millis(2600);
const SAVING: Duration = Duration::from_secs(20);

const BG: [u8; 3] = [16, 16, 20];
const WARN: [u8; 3] = [0xF2, 0x8B, 0x82];

pub enum Kind {
    Saving,
    Saved,
    Failed,
}

struct Note {
    hwnd: HWND,
    /// Kept for the window's life: the window target draws through them.
    _painter: Painter,
    _rt: ID2D1HwndRenderTarget,
    since: Instant,
    hold: Duration,
}

thread_local! {
    static NOTE: RefCell<Option<Note>> = const { RefCell::new(None) };
}

/// Lets screen capture see the note that's up (`on`), or keeps it out again; whether one is up.
pub fn set_capturable(on: bool) -> bool {
    NOTE.with(|n| match n.borrow().as_ref() {
        Some(note) => {
            let affinity = if on { WDA_NONE } else { WDA_EXCLUDEFROMCAPTURE };
            if let Err(e) = unsafe { SetWindowDisplayAffinity(note.hwnd, affinity) } {
                log(&format!("note: SetWindowDisplayAffinity failed: {e}"));
            }
            true
        }
        None => false,
    })
}

/// Shows `title` and `detail` at `place` on `monitor` (desktop coordinates), replacing a note
/// already up.
pub fn show(monitor: Rect, place: Place, kind: Kind, title: &str, detail: &str) {
    if let Err(e) = open(monitor, place, kind, title, detail) {
        log(&format!("can't show the note: {e}"));
    }
}

fn open(monitor: Rect, place: Place, kind: Kind, title: &str, detail: &str) -> Result<(), String> {
    let painter = Painter::new()?;
    let (big, small) = (painter.font(14.0)?, painter.font(11.5)?);
    let s = painter.scale;
    let (tw, _) = painter.measure(title, &big);
    let (dw, _) = painter.measure(detail, &small);
    let (pad, dot, h) = (painter.px(16.0), painter.px(10.0), painter.px(52.0));
    let w = (pad * 2.0 + dot + painter.px(12.0) + tw.max(dw)).ceil();
    let mark = match kind {
        Kind::Failed => WARN,
        _ => crate::app::AMBER,
    };
    let saving = matches!(kind, Kind::Saving);
    let margin = painter.px(24.0) as i32;
    let at = place.at(work_area(monitor), (w as i32, h as i32), margin);

    // a note already up is replaced in place, without fading in again
    let since = match NOTE.with(|n| n.borrow_mut().take()) {
        Some(old) => {
            unsafe {
                let _ = DestroyWindow(old.hwnd);
            }
            Instant::now() - old.since.elapsed().min(Duration::from_secs_f32(FADE_IN))
        }
        None => Instant::now(),
    };
    let hwnd = window(at)?;
    make_glass(hwnd);
    let rt = match painter.window_target(hwnd, at.w, at.h) {
        Ok(rt) => rt,
        Err(e) => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return Err(e);
        }
    };
    unsafe {
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA);
        // only takes once the layered window has its transparency set
        if let Err(e) = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) {
            log(&format!("note: SetWindowDisplayAffinity failed: {e}"));
        }
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    painter.render(&rt, 1.0, |c| draw(c, (w, h, pad, dot, s), mark, saving, (title, &big), (detail, &small)))?;
    let hold = if saving { SAVING } else { SHOWN };
    NOTE.with(|n| *n.borrow_mut() = Some(Note { hwnd, _painter: painter, _rt: rt, since, hold }));
    tick();
    unsafe {
        SetTimer(Some(hwnd), TIMER, 16, None);
    }
    Ok(())
}

/// `monitor` without the taskbar, so notes at the bottom clear it.
fn work_area(monitor: Rect) -> Rect {
    unsafe {
        let centre = POINT { x: monitor.x + monitor.w / 2, y: monitor.y + monitor.h / 2 };
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(MonitorFromPoint(centre, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() {
            return monitor;
        }
        let r = info.rcWork;
        Rect::from_points(r.left, r.top, r.right, r.bottom)
    }
}

fn draw(
    c: &Canvas,
    (w, h, pad, dot, s): (f32, f32, f32, f32, f32),
    mark: [u8; 3],
    saving: bool,
    (title, big): (&str, &IDWriteTextFormat),
    (detail, small): (&str, &IDWriteTextFormat),
) {
    c.round_rect(0.0, 0.0, w, h, h / 2.0, rgba(BG[0], BG[1], BG[2], 0.92));
    let centre = (pad + dot / 2.0, h / 2.0);
    if saving {
        c.ring(centre, dot / 2.0, 2.0 * s, rgba(mark[0], mark[1], mark[2], 1.0));
    } else {
        c.circle(centre, dot / 2.0, rgba(mark[0], mark[1], mark[2], 1.0));
    }
    let x = pad + dot + 12.0 * s;
    c.text(title, big, [x, h * 0.16, w - x, h * 0.42], rgba(255, 255, 255, 0.96));
    c.text(detail, small, [x, h * 0.54, w - x, h * 0.32], rgba(255, 255, 255, 0.55));
}

fn window(at: Rect) -> Result<HWND, String> {
    unsafe {
        let hinst = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = w!("afterglow_note");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc); // fails harmlessly when already registered
        let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        CreateWindowExW(ex, class, w!("afterglow"), WS_POPUP, at.x, at.y, at.w, at.h, None, None, Some(hinst.into()), None)
            .map_err(|e| format!("CreateWindowEx: {e}"))
    }
}

/// How opaque the note is `t` seconds after it came up, if it's still up.
fn alpha(t: f32, hold: f32) -> Option<f32> {
    if t < FADE_IN {
        Some(t / FADE_IN)
    } else if t < hold {
        Some(1.0)
    } else if t < hold + FADE_OUT {
        Some(1.0 - (t - hold) / FADE_OUT)
    } else {
        None
    }
}

/// Shows the note at its opacity now; closes it once it has faded out.
fn tick() {
    let gone = NOTE.with(|n| {
        let n = n.borrow();
        let note = n.as_ref()?;
        match alpha(note.since.elapsed().as_secs_f32(), note.hold.as_secs_f32()) {
            Some(a) => unsafe {
                let byte = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
                let _ = SetLayeredWindowAttributes(note.hwnd, COLORREF(0), byte, LWA_ALPHA);
                None
            },
            None => Some(note.hwnd),
        }
    });
    if let Some(hwnd) = gone {
        NOTE.with(|n| n.borrow_mut().take());
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wp.0 == TIMER => {
            tick();
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_fades_in_holds_and_fades_out() {
        assert_eq!(alpha(0.0, 2.0), Some(0.0));
        assert_eq!(alpha(FADE_IN / 2.0, 2.0), Some(0.5));
        assert_eq!(alpha(1.0, 2.0), Some(1.0));
        assert!(alpha(2.0 + FADE_OUT / 2.0, 2.0).is_some_and(|a| (a - 0.5).abs() < 1e-5));
        assert_eq!(alpha(2.0 + FADE_OUT + 0.01, 2.0), None);
    }
}
