//! Windows 10's own shadow is square, and so is the 1 px frame that comes with it, which shows
//! through a window's see-through corners. A window that paints itself rounded there goes without
//! both and gets this shadow instead: four click-through layered strips kept right below it,
//! shaped like Windows 10's, rounded like the window.

use std::cell::RefCell;

use glint_shell::gfx::Dib;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, TRUE, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    BeginDeferWindowPos, CreateWindowExW, DefWindowProcW, DeferWindowPos, DestroyWindow, EndDeferWindowPos,
    GetForegroundWindow, GetWindowRect, IsIconic, IsWindowVisible, IsZoomed, RegisterClassExW, SW_HIDE,
    SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WINDOWPOS,
    WM_DESTROY, WM_EXITSIZEMOVE, WM_NCACTIVATE, WM_WINDOWPOSCHANGED, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::w;

use super::RADIUS;

/// How far the shadow reaches past the window, at 96 dpi.
const MARGIN: f32 = 24.0;
/// How far into the window its corners still shape the shadow: past this the edges are straight.
const REACH: f32 = 36.0;

/// A blurred rounded rect the window's size grown by `spread`, `alpha` dark at its heart; at 96 dpi.
struct Layer {
    alpha: f32,
    sigma: f32,
    spread: f32,
}

/// Fitted to Windows 10's own shadow around a window, focused and not.
struct Look {
    layers: [Layer; 2],
    /// The shadow sits this far right and down.
    offset: (f32, f32),
}

const FOCUSED: Look = Look {
    layers: [Layer { alpha: 0.30, sigma: 5.4, spread: -0.7 }, Layer { alpha: 0.056, sigma: 4.1, spread: 12.0 }],
    offset: (1.5, 2.3),
};
const UNFOCUSED: Look = Look {
    layers: [Layer { alpha: 0.077, sigma: 1.5, spread: 0.7 }, Layer { alpha: 0.26, sigma: 5.9, spread: 0.0 }],
    offset: (0.9, 1.4),
};

/// The shadow around a small window whose corners are `2 * reach` apart, cut up to stretch over a
/// window of any size: the corners as they are, the middle row and column repeated.
struct Template {
    dpi: u32,
    focused: bool,
    margin: i32,
    reach: i32,
    side: i32,
    alpha: Vec<u8>,
}

impl Template {
    fn new(look: &Look, dpi: u32, focused: bool) -> Template {
        let scale = dpi as f32 / 96.0;
        let margin = (MARGIN * scale).ceil() as i32;
        let reach = (REACH * scale).ceil() as i32;
        let side = 2 * (margin + reach);
        let window = (2 * reach) as f32;
        let radius = RADIUS as f32 * scale;
        let (ox, oy) = (look.offset.0 * scale, look.offset.1 * scale);
        let mut alpha = Vec::with_capacity((side * side) as usize);
        for py in 0..side {
            for px in 0..side {
                let (x, y) = ((px - margin) as f32 + 0.5 - ox, (py - margin) as f32 + 0.5 - oy);
                let a: f32 = look
                    .layers
                    .iter()
                    .map(|l| l.alpha * cover(x, y, window, radius, l.spread * scale, l.sigma * scale))
                    .sum();
                alpha.push((a * 255.0).round().clamp(0.0, 255.0) as u8);
            }
        }
        Template { dpi, focused, margin, reach, side, alpha }
    }

    /// The template column (or row) for window-relative `v` along a window `len` long.
    fn at(&self, v: i32, len: i32) -> i32 {
        let t = if v < self.reach {
            v + self.margin
        } else if v >= len - self.reach {
            v - len + 2 * self.reach + self.margin
        } else {
            self.reach + self.margin
        };
        t.clamp(0, self.side - 1)
    }
}

/// How much of a rounded square `side` wide (`radius`, grown by `spread`), blurred by a gaussian of
/// `sigma`, covers the point `x, y`.
fn cover(x: f32, y: f32, side: f32, radius: f32, spread: f32, sigma: f32) -> f32 {
    const STEPS: usize = 48;
    let (lo, hi, r) = (-spread, side + spread, (radius + spread).max(0.0));
    let (from, to) = ((y - 4.0 * sigma).max(lo), (y + 4.0 * sigma).min(hi));
    if from >= to {
        return 0.0;
    }
    let step = (to - from) / STEPS as f32;
    let norm = 1.0 / (sigma * std::f32::consts::TAU.sqrt());
    let mut sum = 0.0;
    for i in 0..STEPS {
        let v = from + (i as f32 + 0.5) * step;
        // the rounded square's row at v, blurred across
        let t = if v < lo + r { lo + r - v } else if v > hi - r { v - (hi - r) } else { 0.0 };
        let inset = r - (r * r - t * t).max(0.0).sqrt();
        let row = phi((hi - inset - x) / sigma) - phi((lo + inset - x) / sigma);
        let d = (y - v) / sigma;
        sum += (-0.5 * d * d).exp() * norm * row * step;
    }
    sum
}

/// The standard normal CDF.
fn phi(z: f32) -> f32 {
    0.5 * (1.0 + erf(z / std::f32::consts::SQRT_2))
}

/// Abramowitz & Stegun 7.1.26, within 1.5e-7.
fn erf(x: f32) -> f32 {
    let (sign, x) = (x.signum(), x.abs());
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let poly = ((((1.061_405_4 * t - 1.453_152_1) * t + 1.421_413_8) * t - 0.284_496_74) * t + 0.254_829_6) * t;
    sign * (1.0 - poly * (-x * x).exp())
}

struct Shadow {
    owner: HWND,
    strips: [Strip; 4],
    focused: bool,
    shown: bool,
    /// The window size the strips are around.
    size: (i32, i32),
    /// The window grew and hasn't painted its new part yet: the strips follow once it has (and
    /// restack then if this says so).
    pending: Option<bool>,
    templates: Vec<Template>,
}

struct Strip {
    hwnd: HWND,
    /// Kept between draws, a little bigger than needed, so a resize doesn't make a new one.
    dib: Option<Dib>,
    /// The size, dpi and focus it's drawn for.
    drawn: Option<(i32, i32, u32, bool)>,
}

thread_local! {
    static SHADOW: RefCell<Option<Shadow>> = const { RefCell::new(None) };
}

unsafe extern "system" fn strip_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// Gives `owner` the shadow; [`message`] keeps it in place.
pub(super) fn attach(owner: HWND) {
    let Some(strips) = (unsafe { strips() }) else { return };
    let focused = unsafe { GetForegroundWindow() } == owner;
    let strips = strips.map(|hwnd| Strip { hwnd, dib: None, drawn: None });
    let shadow = Shadow { owner, strips, focused, shown: false, size: (0, 0), pending: None, templates: Vec::new() };
    SHADOW.with_borrow_mut(|s| *s = Some(shadow));
    with(|s| s.sync(true));
}

unsafe fn strips() -> Option<[HWND; 4]> {
    unsafe {
        let hinst = GetModuleHandleW(None).ok()?;
        let class = w!("glint_window_shadow");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(strip_proc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc); // fails harmlessly when already registered
        let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        let mut strips = [HWND::default(); 4];
        for i in 0..strips.len() {
            match CreateWindowExW(ex, class, w!(""), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst.into()), None) {
                Ok(h) => {
                    let off = TRUE;
                    let size = size_of_val(&off) as u32;
                    let _ = DwmSetWindowAttribute(h, DWMWA_TRANSITIONS_FORCEDISABLED, &off as *const _ as _, size);
                    strips[i] = h;
                }
                Err(_) => {
                    for &h in &strips[..i] {
                        let _ = DestroyWindow(h);
                    }
                    return None;
                }
            }
        }
        Some(strips)
    }
}

/// Follows the window's moves, sizes, focus and end.
pub(super) fn message(msg: u32, wp: WPARAM, lp: LPARAM) {
    match msg {
        WM_WINDOWPOSCHANGED => {
            let pos = unsafe { &*(lp.0 as *const WINDOWPOS) };
            // moves and sizes keep the stacking; anything else may have put windows in between
            let restack = !pos.flags.contains(SWP_NOZORDER);
            with(|s| {
                let grew = !pos.flags.contains(SWP_NOSIZE) && (pos.cx > s.size.0 || pos.cy > s.size.1);
                if grew && s.shown {
                    // until the window paints its new part that part shows the strips still below
                    // it, the shadow catching up, where strips moved out already would leave a gap
                    s.pending = Some(s.pending.unwrap_or(false) || restack);
                } else {
                    s.sync(restack);
                }
            });
        }
        WM_EXITSIZEMOVE => painted(),
        WM_NCACTIVATE => with(|s| {
            let focused = wp.0 != 0;
            if std::mem::replace(&mut s.focused, focused) != focused {
                s.sync(false);
            }
        }),
        WM_DESTROY => {
            let gone = SHADOW.with(|cell| cell.try_borrow_mut().ok().and_then(|mut s| s.take()));
            for strip in gone.map(|s| s.strips).into_iter().flatten() {
                let _ = unsafe { DestroyWindow(strip.hwnd) };
            }
        }
        _ => {}
    }
}

/// The window painted: the strips catch up with a size it grew to.
pub(super) fn painted() {
    with(|s| {
        if let Some(restack) = s.pending {
            s.sync(restack);
        }
    });
}

/// Runs `f` on the shadow, unless there's none or it's already busy further up the stack.
fn with(f: impl FnOnce(&mut Shadow)) {
    SHADOW.with(|cell| {
        if let Ok(mut s) = cell.try_borrow_mut()
            && let Some(s) = s.as_mut()
        {
            f(s);
        }
    });
}

impl Shadow {
    /// Shows the strips around the window (`restack`: right below it again), or hides them while
    /// it's hidden, minimized or maximized.
    fn sync(&mut self, restack: bool) {
        let restack = restack || self.pending.take().unwrap_or(false);
        let owner = self.owner;
        let mut r = RECT::default();
        let visible = unsafe {
            IsWindowVisible(owner).as_bool()
                && !IsIconic(owner).as_bool()
                && !IsZoomed(owner).as_bool()
                && GetWindowRect(owner, &mut r).is_ok()
        };
        if !visible {
            if self.shown {
                for s in &self.strips {
                    let _ = unsafe { ShowWindow(s.hwnd, SW_HIDE) };
                }
                self.shown = false;
            }
            return;
        }
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        self.size = (w, h);
        let dpi = unsafe { GetDpiForWindow(owner) }.max(96);
        let at = rects(w, h, dpi);
        // a redrawn strip is moved along with it
        let drawn = self.draw(w, h, dpi, &at, (r.left, r.top));
        let restack = restack || !self.shown;
        let moves = (0..4).filter(|&i| restack || !drawn[i]).count();
        if moves == 0 {
            return;
        }
        unsafe {
            let Ok(mut batch) = BeginDeferWindowPos(moves as i32) else { return };
            for (i, (s, &(x, y, _, _))) in self.strips.iter().zip(&at).enumerate() {
                let (after, flags) = if restack {
                    (Some(owner), SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW)
                } else if !drawn[i] {
                    (None, SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER)
                } else {
                    continue;
                };
                match DeferWindowPos(batch, s.hwnd, after, r.left + x, r.top + y, 0, 0, flags) {
                    Ok(b) => batch = b,
                    Err(_) => return,
                }
            }
            let _ = EndDeferWindowPos(batch);
        }
        self.shown = true;
    }

    /// Draws, and puts in place, the strips whose size, dpi or focus changed; says which.
    fn draw(&mut self, w: i32, h: i32, dpi: u32, at: &[(i32, i32, i32, i32); 4], origin: (i32, i32)) -> [bool; 4] {
        let focused = self.focused;
        let mut drawn = [false; 4];
        if at.iter().zip(&self.strips).all(|(&(_, _, sw, sh), s)| s.drawn == Some((sw, sh, dpi, focused))) {
            return drawn;
        }
        let i = match self.templates.iter().position(|t| t.dpi == dpi && t.focused == focused) {
            Some(i) => i,
            None => {
                self.templates.push(Template::new(if focused { &FOCUSED } else { &UNFOCUSED }, dpi, focused));
                self.templates.len() - 1
            }
        };
        let t = &self.templates[i];
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        for ((s, &(x, y, sw, sh)), drawn) in self.strips.iter_mut().zip(at).zip(&mut drawn) {
            if sw <= 0 || sh <= 0 || s.drawn == Some((sw, sh, dpi, focused)) {
                continue;
            }
            if s.dib.as_ref().is_none_or(|d| d.w < sw || d.h < sh) {
                // room to grow by a bit before the next one
                s.dib = Dib::new(sw + sw / 4, sh + sh / 4).ok();
            }
            let Some(dib) = &mut s.dib else { continue };
            let stride = dib.w as usize;
            let side = t.side as usize;
            let columns: Vec<usize> = (0..sw).map(|i| t.at(x + i, w) as usize).collect();
            for (j, row) in dib.pixels_mut().chunks_exact_mut(stride).take(sh as usize).enumerate() {
                let line = &t.alpha[t.at(y + j as i32, h) as usize * side..][..side];
                for (px, &c) in row.iter_mut().zip(&columns) {
                    // black, premultiplied: only the alpha
                    *px = u32::from(line[c]) << 24;
                }
            }
            let (pos, size, from) = (POINT { x: origin.0 + x, y: origin.1 + y }, SIZE { cx: sw, cy: sh }, POINT::default());
            unsafe {
                let _ = UpdateLayeredWindow(
                    s.hwnd,
                    None,
                    Some(&pos),
                    Some(&size),
                    Some(dib.dc),
                    Some(&from),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );
            }
            s.drawn = Some((sw, sh, dpi, focused));
            *drawn = true;
        }
        drawn
    }
}

/// The strips as window-relative `(x, y, w, h)`: above and below, with the rows the corners round
/// off; left and right in between.
fn rects(w: i32, h: i32, dpi: u32) -> [(i32, i32, i32, i32); 4] {
    let scale = dpi as f32 / 96.0;
    let m = (MARGIN * scale).ceil() as i32;
    let r = ((RADIUS as f32 * scale).ceil() as i32).min(h / 2);
    [(-m, -m, w + 2 * m, m + r), (-m, h - r, w + 2 * m, m + r), (-m, r, m, h - 2 * r), (w, r, m, h - 2 * r)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shadow_matches_windows_10_along_the_edges() {
        // Windows 10's focused shadow, 1..=8 px out from a window's left and bottom edges
        let left = [42, 34, 28, 23, 19, 16, 15, 13];
        let bottom = [60, 52, 46, 40, 36, 32, 28, 25];
        let t = Template::new(&FOCUSED, 96, true);
        let mid = t.margin + t.reach;
        for (i, (&l, &b)) in left.iter().zip(&bottom).enumerate() {
            let i = i as i32;
            let ours_left = t.alpha[(mid * t.side + t.margin - 1 - i) as usize];
            let ours_bottom = t.alpha[((t.side - t.margin + i) * t.side + mid) as usize];
            assert!(ours_left.abs_diff(l) <= 5, "left {i}: {ours_left} vs {l}");
            assert!(ours_bottom.abs_diff(b) <= 5, "bottom {i}: {ours_bottom} vs {b}");
        }
    }

    #[test]
    fn a_window_of_any_size_reads_its_corners_as_they_are_and_repeats_the_middle() {
        let t = Template::new(&UNFOCUSED, 96, false);
        let (m, r) = (t.margin, t.reach);
        assert_eq!(t.at(-m, 500), 0);
        assert_eq!(t.at(r - 1, 500), m + r - 1);
        assert_eq!(t.at(250, 500), m + r);
        assert_eq!(t.at(500 - r, 500), m + r);
        assert_eq!(t.at(500 + m - 1, 500), t.side - 1);
    }

    #[test]
    fn the_strips_ring_the_window_without_overlapping() {
        let [top, bottom, left, right] = rects(640, 480, 96);
        assert_eq!(top, (-24, -24, 688, 32));
        assert_eq!(bottom, (-24, 472, 688, 32));
        assert_eq!((left, right), ((-24, 8, 24, 464), (640, 8, 24, 464)));
    }

    #[test]
    fn erf_is_close() {
        for (x, e) in [(0.0, 0.0), (0.5, 0.520_499_9), (1.0, 0.842_700_8), (-2.0, -0.995_322_3)] {
            assert!((erf(x) - e).abs() < 1e-5, "{x}");
        }
    }
}
