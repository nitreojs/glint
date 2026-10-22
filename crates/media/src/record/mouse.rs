//! Mouse buttons for click highlights, from a low-level mouse hook on its own thread. Windows
//! skips hooks that answer slowly, so the callback only stamps the event and queues it.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};

use glint_core::geom::rect::Rect;
use glint_core::log::log;
use glint_core::record::clicks::{Button, Press, to_region};
use glint_core::record::pipeline::Mouse;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, HC_ACTION, HHOOK, MSG, MSLLHOOKSTRUCT, MWMO_INPUTAVAILABLE,
    MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW, QS_ALLINPUT, SetWindowsHookExW, UnhookWindowsHookEx,
    WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP,
};

use super::win::{hns, qpc};
use crate::rect::WinRect;

const POLL_MS: u32 = 10;

struct Raw {
    button: Button,
    down: bool,
    qpc: i64,
    pos: (i32, i32),
}

thread_local! {
    static QUEUE: RefCell<Option<Sender<Raw>>> = const { RefCell::new(None) };
}

fn button(message: u32) -> Option<(Button, bool)> {
    match message {
        WM_LBUTTONDOWN => Some((Button::Left, true)),
        WM_LBUTTONUP => Some((Button::Left, false)),
        WM_RBUTTONDOWN => Some((Button::Right, true)),
        WM_RBUTTONUP => Some((Button::Right, false)),
        WM_MBUTTONDOWN => Some((Button::Middle, true)),
        WM_MBUTTONUP => Some((Button::Middle, false)),
        _ => None,
    }
}

unsafe extern "system" fn on_mouse(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32
        && let Some((button, down)) = button(wp.0 as u32)
    {
        // MSLLHOOKSTRUCT.pt is in per-monitor-aware (physical) screen coordinates.
        let pt = unsafe { (*(lp.0 as *const MSLLHOOKSTRUCT)).pt };
        let raw = Raw { button, down, qpc: qpc(), pos: (pt.x, pt.y) };
        QUEUE.with_borrow(|q| {
            if let Some(q) = q {
                let _ = q.send(raw);
            }
        });
    }
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

/// DPI / 96 of the monitor showing most of `region`.
pub(super) fn scale(region: Rect) -> f32 {
    let (mut x, mut y) = (0, 0);
    unsafe {
        let monitor = MonitorFromRect(&region.to_win(), MONITOR_DEFAULTTONEAREST);
        match GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) {
            Ok(()) if x > 0 => x as f32 / 96.0,
            _ => 1.0,
        }
    }
}

pub(super) struct Hook {
    pub(super) region: Rect,
}

fn unhook(hook: HHOOK) {
    if let Err(e) = unsafe { UnhookWindowsHookEx(hook) } {
        log(&format!("removing the mouse hook failed: {e}"));
    }
}

fn release(out: &mut dyn FnMut(Press), held: &mut [bool; 3], r: Rect, button: Button, at: i64, pos: (i32, i32)) {
    if std::mem::take(&mut held[button as usize]) {
        let (x, y) = ((pos.0 - r.x) as f32 + 0.5, (pos.1 - r.y) as f32 + 0.5);
        out(Press { button, down: false, at, x, y });
    }
}

impl Mouse for Hook {
    fn run(&mut self, stop: &AtomicBool, listening: &dyn Fn() -> bool, out: &mut dyn FnMut(Press)) {
        let (tx, rx) = mpsc::channel();
        QUEUE.set(Some(tx));
        let module = unsafe { GetModuleHandleW(None) }.ok().map(Into::into);
        let mut hook = None;
        let mut failed = false;
        let mut held = [false; 3];
        let r = self.region;
        while !stop.load(Ordering::SeqCst) {
            match hook {
                None if !failed && listening() => match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(on_mouse), module, 0) } {
                    Ok(h) => hook = Some(h),
                    Err(e) => {
                        log(&format!("click highlights unavailable: SetWindowsHookExW failed: {e}"));
                        failed = true;
                    }
                },
                Some(h) if !listening() => {
                    unhook(h);
                    hook = None;
                    let at = hns(qpc());
                    for b in [Button::Left, Button::Right, Button::Middle] {
                        release(out, &mut held, r, b, at, (r.x, r.y));
                    }
                }
                _ => {}
            }
            unsafe {
                let _ = MsgWaitForMultipleObjectsEx(None, POLL_MS, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&msg);
                }
            }
            for raw in rx.try_iter() {
                let at = hns(raw.qpc);
                if !raw.down {
                    release(out, &mut held, r, raw.button, at, raw.pos);
                } else if let Some((x, y)) = to_region(raw.pos, r) {
                    held[raw.button as usize] = true;
                    out(Press { button: raw.button, down: true, at, x, y });
                }
            }
        }
        if let Some(h) = hook {
            unhook(h);
        }
        QUEUE.set(None);
    }
}
