use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use super::State;
use crate::platform::rect::Rect;

/// The hover highlight glides from one window to the next.
pub(super) const GLIDE_MS: f32 = 120.0;
/// Jumps of the drag box glide too: from the hover highlight when the drag starts, and to
/// the other side of the start point when it flips (the encoder minimum makes both jump).
pub(super) const FLIP_MS: f32 = 120.0;
/// Posted to the picker once per composed frame while a glide runs.
pub(super) const WM_GLIDE: u32 = WM_APP + 40;

/// Vsync pacing for the glide: a thread waits on DwmFlush (one return per composed frame)
/// and posts [`WM_GLIDE`], so the highlight moves once per refresh. WM_TIMER can't do
/// that: it never fires faster than ~10 ms and drifts against the refresh.
pub(super) struct Ticker {
    pub(super) run: AtomicBool,
    /// A tick is queued and not yet handled; keeps a slow frame from piling up ticks.
    pub(super) pending: AtomicBool,
}

/// The highlight moving from `from` to the current target.
#[derive(Clone, Copy)]
pub(super) struct Glide {
    pub(super) from: Rect,
    pub(super) start: Instant,
    pub(super) ms: f32,
    /// A drag box flipping across its start point (it keeps following the drag).
    pub(super) flip: bool,
}

impl State {
    /// Shows the next glide frame; returns whether the glide is still running.
    pub(super) fn step_glide(&mut self) -> bool {
        let (Some(Glide { from, start, ms, .. }), Some(to)) = (self.glide, self.target) else { return false };
        let t = (start.elapsed().as_secs_f32() * 1000.0 / ms).min(1.0);
        let e = 1.0 - (1.0 - t).powi(3); // ease-out cubic
        let mix = |a: i32, b: i32| a + ((b - a) as f32 * e).round() as i32;
        self.show(Some(Rect { x: mix(from.x, to.x), y: mix(from.y, to.y), w: mix(from.w, to.w), h: mix(from.h, to.h) }));
        if t >= 1.0 {
            self.glide = None;
        }
        self.glide.is_some()
    }

    pub(super) fn set_gliding(&mut self, on: bool) {
        if !on {
            if let Some(t) = self.ticker.take() {
                t.run.store(false, Ordering::Relaxed);
            }
            return;
        }
        if self.ticker.is_some() {
            return;
        }
        let t = Arc::new(Ticker { run: AtomicBool::new(true), pending: AtomicBool::new(false) });
        let (ticker, hwnd) = (t.clone(), self.hwnd.0 as isize);
        std::thread::spawn(move || {
            while ticker.run.load(Ordering::Relaxed) {
                if unsafe { DwmFlush() }.is_err() {
                    std::thread::sleep(Duration::from_millis(7));
                }
                if ticker.run.load(Ordering::Relaxed) && !ticker.pending.swap(true, Ordering::Relaxed) {
                    let _ = unsafe { PostMessageW(Some(HWND(hwnd as _)), WM_GLIDE, WPARAM(0), LPARAM(0)) };
                }
            }
        });
        self.ticker = Some(t);
    }
}
