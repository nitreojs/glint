//! Full-screen region picker in the style of ShareX: a frozen, dimmed screenshot of the
//! whole virtual desktop that fades in. Drag to select (the selection shows undimmed), click
//! to pick a whole monitor, Esc / right-click to cancel.
//!
//! The result is posted to the owner as `Box<Option<Selection>>` in LPARAM; the picker keeps
//! showing until the owner calls [`dismiss`], so the fade-out can overlap other work.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmFlush, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, w};

use crate::gfx::{Dib, Painter, animate, rgba};
use crate::monitors::{Output, fit_selection, output_at};
use crate::util::Rect;

#[derive(Clone, Debug)]
pub struct Selection {
    /// Desktop coordinates, already clipped to `output` with even size.
    pub rect: Rect,
    pub output: Output,
}

/// Selection outline: 1 px white inside 1 px black, both outside the selected pixels.
const RING: i32 = 2;
const CLICK_SLOP: i32 = 4;
/// Brightness kept outside the selection, out of 256.
const DIM: u32 = 150;
const PILL_BG: D2D1_COLOR_F = rgba(16, 16, 20, 0.86);
const PILL_TEXT: D2D1_COLOR_F = rgba(255, 255, 255, 0.96);
const FADE_IN_MS: f32 = 140.0;
const FADE_OUT_MS: f32 = 110.0;
const HINT: &str = "Drag to select a region   ·   Click to record the whole monitor   ·   Esc to cancel";

/// A pre-rendered pill (label or hint) and where it sits, in client coordinates.
struct Pill {
    rect: Rect,
    dib: Dib,
    text: String,
}

struct State {
    hwnd: HWND,
    owner: HWND,
    msg: u32,
    origin: (i32, i32),
    bright: Dib,
    dim: Dib,
    painter: Painter,
    font: IDWriteTextFormat,
    white: HBRUSH,
    black: HBRUSH,
    outputs: Vec<Output>,
    drag_start: Option<(i32, i32)>,
    /// Current (fitted) selection in client coordinates.
    sel: Option<Rect>,
    label: Option<Pill>,
    /// A result was posted; ignore input until dismissed.
    done: bool,
    /// Input outranks WM_PAINT, so while the mouse moves the picker paints itself, at most
    /// once per display refresh.
    last_paint: Cell<Instant>,
    frame: Duration,
}

impl Drop for State {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.white.into());
            let _ = DeleteObject(self.black.into());
        }
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

enum Action {
    Default,
    Handled,
    Capture,
    /// Handled; repaint right away (outside the state borrow).
    Paint,
    Release,
    Finish(Option<Selection>),
}

/// Renders a pill with `text` over a copy of `bg` at (x, y).
fn render_pill(painter: &Painter, font: &IDWriteTextFormat, text: &str, bg: &Dib, x: i32, y: i32) -> Result<Pill, String> {
    let (tw, _) = painter.measure(text, font);
    let h = painter.px(26.0);
    let pad = painter.px(11.0);
    let w = tw.ceil() + pad * 2.0;
    let dib = Dib::new(w as i32, h as i32)?;
    unsafe {
        let _ = BitBlt(dib.dc, 0, 0, dib.w, dib.h, Some(bg.dc), x, y, SRCCOPY);
    }
    painter.draw(&dib, |c| {
        c.round_rect(0.0, 0.0, w, h, h / 2.0, PILL_BG);
        c.text(text, font, [pad, 0.0, tw + 2.0, h], PILL_TEXT);
    })?;
    Ok(Pill { rect: Rect { x, y, w: dib.w, h: dib.h }, dib, text: text.to_string() })
}

fn pill_size(painter: &Painter, font: &IDWriteTextFormat, text: &str) -> (i32, i32) {
    let (tw, _) = painter.measure(text, font);
    ((tw.ceil() + painter.px(11.0) * 2.0) as i32, painter.px(26.0) as i32)
}

/// Takes a screenshot and fades the picker in. `msg` is posted to `owner` with the result.
pub fn open(owner: HWND, msg: u32, outputs: Vec<Output>) -> Result<HWND, String> {
    unsafe {
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);

        let bright = Dib::new(vw, vh)?;
        let mut dim = Dib::new(vw, vh)?;
        let screen = GetDC(None);
        let _ = BitBlt(bright.dc, 0, 0, vw, vh, Some(screen), vx, vy, ROP_CODE(SRCCOPY.0 | CAPTUREBLT.0));
        ReleaseDC(None, screen);
        let _ = GdiFlush();
        for (d, &s) in dim.pixels_mut().iter_mut().zip(bright.pixels()) {
            let rb = (((s & 0x00FF_00FF) * DIM) >> 8) & 0x00FF_00FF;
            let g = (((s & 0x0000_FF00) * DIM) >> 8) & 0x0000_FF00;
            *d = rb | g;
        }

        let painter = Painter::new()?;
        let font = painter.font(13.0)?;
        let (hw, _) = pill_size(&painter, &font, HINT);
        for o in &outputs {
            let r = o.rect.offset(-vx, -vy);
            let hint = render_pill(&painter, &font, HINT, &dim, r.x + (r.w - hw) / 2, r.y + painter.px(28.0) as i32)?;
            let p = hint.rect;
            let _ = BitBlt(dim.dc, p.x, p.y, p.w, p.h, Some(hint.dib.dc), 0, 0, SRCCOPY);
        }
        let _ = GdiFlush();

        let hinst = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = w!("hzrec_overlay");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc); // fails harmlessly when already registered
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            class,
            w!("hzrec"),
            WS_POPUP,
            vx,
            vy,
            vw,
            vh,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .map_err(|e| format!("CreateWindowEx: {e}"))?;
        let on = TRUE;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, &on as *const BOOL as _, 4);
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA);

        let max_hz = outputs.iter().map(|o| o.refresh_hz).fold(60.0, f64::max);
        let frame = Duration::from_secs_f64(0.9 / max_hz);
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                hwnd,
                owner,
                msg,
                origin: (vx, vy),
                bright,
                dim,
                painter,
                font,
                white: CreateSolidBrush(COLORREF(0x00FFFFFF)),
                black: CreateSolidBrush(COLORREF(0)),
                outputs,
                drag_start: None,
                sel: None,
                label: None,
                done: false,
                last_paint: Cell::new(Instant::now()),
                frame,
            })
        });
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);
        let _ = SetForegroundWindow(hwnd);
        animate(FADE_IN_MS, |t| {
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), (t * 255.0) as u8, LWA_ALPHA);
        });
        Ok(hwnd)
    }
}

/// Cancels an open picker (a `None` result is posted).
pub fn cancel(hwnd: HWND) {
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}

/// Fades the picker out and destroys it. Returns once the desktop is visible again.
pub fn dismiss(hwnd: HWND) {
    let st = STATE.with(|s| {
        let mut g = s.borrow_mut();
        if g.as_ref().is_some_and(|st| st.hwnd == hwnd) { g.take() } else { None }
    });
    unsafe {
        let _ = ReleaseCapture();
        animate(FADE_OUT_MS, |t| {
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), ((1.0 - t) * 255.0) as u8, LWA_ALPHA);
        });
        let _ = DestroyWindow(hwnd);
        drop(st);
        let _ = DwmFlush(); // make sure the composed desktop no longer contains the picker
    }
}

fn point(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32)
}

fn rgn(r: Rect) -> HRGN {
    unsafe { CreateRectRgn(r.x, r.y, r.right(), r.bottom()) }
}

/// The four strips between `outer` and `inner`.
fn ring(outer: Rect, inner: Rect) -> [RECT; 4] {
    [
        RECT { left: outer.x, top: outer.y, right: outer.right(), bottom: inner.y },
        RECT { left: outer.x, top: inner.bottom(), right: outer.right(), bottom: outer.bottom() },
        RECT { left: outer.x, top: inner.y, right: inner.x, bottom: inner.bottom() },
        RECT { left: inner.right(), top: inner.y, right: outer.right(), bottom: inner.bottom() },
    ]
}

impl State {
    fn to_desktop(&self, r: Rect) -> Rect {
        r.offset(self.origin.0, self.origin.1)
    }
    fn to_client(&self, r: Rect) -> Rect {
        r.offset(-self.origin.0, -self.origin.1)
    }

    /// The selection that would be recorded for a drag from `a` to `b` (client coords).
    fn fitted(&self, a: (i32, i32), b: (i32, i32)) -> Option<(Rect, usize)> {
        let raw = self.to_desktop(Rect::from_points(a.0, a.1, b.0, b.1));
        fit_selection(raw, &self.outputs)
    }

    /// Replaces the selection and, if `label` is Some, the label; invalidates exactly the
    /// pixels that change.
    fn set_selection(&mut self, sel: Option<Rect>, label: Option<Option<Pill>>) {
        let (old, new) = (self.sel, sel);
        if old == new && label.is_none() {
            return;
        }
        self.sel = sel;
        let mut labels = Vec::new();
        if let Some(label) = label {
            labels.extend(self.label.as_ref().map(|l| l.rect));
            self.label = label;
            labels.extend(self.label.as_ref().map(|l| l.rect));
        }
        unsafe {
            let damage = CreateRectRgn(0, 0, 0, 0);
            let add = |r: HRGN| {
                CombineRgn(Some(damage), Some(damage), Some(r), RGN_OR);
                let _ = DeleteObject(r.into());
            };
            // Pixels that switch between dimmed and bright.
            let empty = Rect::default();
            let (a, b) = (rgn(old.unwrap_or(empty)), rgn(new.unwrap_or(empty)));
            let xor = CreateRectRgn(0, 0, 0, 0);
            CombineRgn(Some(xor), Some(a), Some(b), RGN_XOR);
            let _ = DeleteObject(a.into());
            let _ = DeleteObject(b.into());
            add(xor);
            for s in [old, new].into_iter().flatten() {
                let r = rgn(s.inflate(RING));
                let inner = rgn(s);
                CombineRgn(Some(r), Some(r), Some(inner), RGN_DIFF);
                let _ = DeleteObject(inner.into());
                add(r);
            }
            for l in labels {
                add(rgn(l));
            }
            let _ = InvalidateRgn(self.hwnd, Some(damage), false);
            let _ = DeleteObject(damage.into());
        }
    }

    fn update_selection(&mut self, cur: (i32, i32)) {
        let Some(start) = self.drag_start else { return };
        let Some((r, oi)) = self.fitted(start, cur) else {
            return self.set_selection(None, Some(None));
        };
        let sel = self.to_client(r);
        let mon = self.to_client(self.outputs[oi].rect);
        let text = format!("{} × {}   ·   {} fps", r.w, r.h, self.outputs[oi].native_fps());
        let same_text = self.label.as_ref().filter(|l| l.text == text);
        let (lw, lh) = match same_text {
            Some(l) => (l.rect.w, l.rect.h),
            None => pill_size(&self.painter, &self.font, &text),
        };
        let gap = self.painter.px(6.0) as i32;
        let above = sel.y - RING - gap - lh;
        let (x, y, bg) = if above >= mon.y {
            (sel.x - RING, above, &self.dim)
        } else {
            (sel.x + gap + 2, sel.y + gap + 2, &self.bright)
        };
        let x = x.clamp(mon.x, (mon.right() - lw).max(mon.x));
        if same_text.is_some_and(|l| (l.rect.x, l.rect.y) == (x, y)) {
            return self.set_selection(Some(sel), None); // label unchanged
        }
        let label = render_pill(&self.painter, &self.font, &text, bg, x, y).ok();
        self.set_selection(Some(sel), Some(label));
    }

    fn finish_drag(&mut self, end: (i32, i32)) -> Option<Selection> {
        let start = self.drag_start.take()?;
        let click = (end.0 - start.0).abs() < CLICK_SLOP && (end.1 - start.1).abs() < CLICK_SLOP;
        let fitted = if click {
            let (dx, dy) = (end.0 + self.origin.0, end.1 + self.origin.1);
            output_at(&self.outputs, dx, dy).and_then(|i| fit_selection(self.outputs[i].rect, &self.outputs))
        } else {
            self.fitted(start, end)
        };
        fitted.map(|(rect, oi)| Selection { rect, output: self.outputs[oi].clone() })
    }

    /// Every pixel is written exactly once per paint, so nothing flickers.
    unsafe fn paint(&self) {
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(self.hwnd, &mut ps);
            self.last_paint.set(Instant::now());
            let rp = Rect::from_win(&ps.rcPaint);
            let label = self.label.as_ref();

            let saved = SaveDC(hdc);
            if let Some(s) = self.sel {
                let o = s.inflate(RING);
                ExcludeClipRect(hdc, o.x, o.y, o.right(), o.bottom());
            }
            if let Some(l) = label {
                ExcludeClipRect(hdc, l.rect.x, l.rect.y, l.rect.right(), l.rect.bottom());
            }
            let _ = BitBlt(hdc, rp.x, rp.y, rp.w, rp.h, Some(self.dim.dc), rp.x, rp.y, SRCCOPY);
            let _ = RestoreDC(hdc, saved);

            if let Some(s) = self.sel {
                let saved = SaveDC(hdc);
                if let Some(l) = label {
                    ExcludeClipRect(hdc, l.rect.x, l.rect.y, l.rect.right(), l.rect.bottom());
                }
                if let Some(i) = s.intersect(&rp) {
                    let _ = BitBlt(hdc, i.x, i.y, i.w, i.h, Some(self.bright.dc), i.x, i.y, SRCCOPY);
                }
                for r in ring(s.inflate(1), s) {
                    FillRect(hdc, &r, self.white);
                }
                for r in ring(s.inflate(2), s.inflate(1)) {
                    FillRect(hdc, &r, self.black);
                }
                let _ = RestoreDC(hdc, saved);
            }
            if let Some(l) = label {
                let r = l.rect;
                let _ = BitBlt(hdc, r.x, r.y, r.w, r.h, Some(l.dib.dc), 0, 0, SRCCOPY);
            }
            let _ = EndPaint(self.hwnd, &ps);
        }
    }

    fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Action {
        if msg == WM_PAINT {
            unsafe { self.paint() };
            return Action::Handled;
        }
        if self.done {
            return match msg {
                WM_CLOSE | WM_KEYDOWN | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONUP | WM_MOUSEMOVE => Action::Handled,
                _ => Action::Default,
            };
        }
        match msg {
            WM_LBUTTONDOWN => {
                self.set_selection(None, Some(None));
                self.drag_start = Some(point(lp));
                Action::Capture
            }
            WM_MOUSEMOVE => {
                if self.drag_start.is_none() {
                    return Action::Handled;
                }
                self.update_selection(point(lp));
                if self.last_paint.get().elapsed() >= self.frame { Action::Paint } else { Action::Handled }
            }
            WM_LBUTTONUP => match self.finish_drag(point(lp)) {
                Some(sel) => Action::Finish(Some(sel)),
                None => {
                    self.set_selection(None, Some(None));
                    Action::Release
                }
            },
            WM_RBUTTONUP => {
                if self.drag_start.take().is_some() {
                    self.set_selection(None, Some(None));
                    Action::Release
                } else {
                    Action::Finish(None)
                }
            }
            WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => Action::Finish(None),
            WM_KEYDOWN if wp.0 == VK_RETURN.0 as usize => match (self.drag_start, self.sel) {
                (Some(_), Some(s)) => Action::Finish(self.finish_drag((s.right(), s.bottom()))),
                _ => Action::Handled,
            },
            WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => Action::Finish(None),
            WM_CLOSE => Action::Finish(None),
            _ => Action::Default,
        }
    }
}

fn finish(result: Option<Selection>) {
    let target = STATE.with(|s| {
        s.borrow_mut().as_mut().map(|st| {
            st.done = true;
            (st.owner, st.msg)
        })
    });
    let Some((owner, msg)) = target else { return };
    unsafe {
        let _ = ReleaseCapture();
        let boxed = Box::into_raw(Box::new(result));
        if PostMessageW(Some(owner), msg, WPARAM(0), LPARAM(boxed as isize)).is_err() {
            drop(Box::from_raw(boxed));
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_ERASEBKGND {
        return LRESULT(1);
    }
    let action = STATE.with(|s| match s.try_borrow_mut() {
        Ok(mut g) => match g.as_mut() {
            Some(st) if st.hwnd == hwnd => st.handle(msg, wp, lp),
            _ => Action::Default,
        },
        Err(_) => Action::Default,
    });
    unsafe {
        match action {
            Action::Default => DefWindowProcW(hwnd, msg, wp, lp),
            Action::Handled => LRESULT(0),
            Action::Capture => {
                SetCapture(hwnd);
                LRESULT(0)
            }
            Action::Paint => {
                let _ = UpdateWindow(hwnd);
                LRESULT(0)
            }
            Action::Release => {
                let _ = ReleaseCapture();
                LRESULT(0)
            }
            Action::Finish(sel) => {
                finish(sel);
                LRESULT(0)
            }
        }
    }
}
