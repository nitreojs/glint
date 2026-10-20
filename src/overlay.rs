//! Full-screen region picker in the style of ShareX: a frozen, dimmed screenshot of the
//! whole virtual desktop that fades in. Hovering highlights the window under the cursor (or
//! the whole monitor over the desktop) and a click picks it; dragging selects a region (the
//! selection shows undimmed). Esc / right-click cancels.
//!
//! The result is posted to the owner as `Box<Option<Selection>>` in LPARAM; the picker keeps
//! showing until the owner calls [`dismiss`], so the fade-out can overlap other work.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DWMWA_TRANSITIONS_FORCEDISABLED, DwmFlush, DwmGetWindowAttribute,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, w};

use crate::gfx::{Dib, Painter, animate, rgba};
use crate::monitors::{Output, fit_drag, fit_rect, output_at};
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
/// The hover highlight glides from one window to the next.
const GLIDE_MS: f32 = 120.0;
/// Jumps of the drag box glide too: from the hover highlight when the drag starts, and to
/// the other side of the start point when it flips (the encoder minimum makes both jump).
const FLIP_MS: f32 = 120.0;
/// Posted to the picker once per composed frame while a glide runs.
const WM_GLIDE: u32 = WM_APP + 40;

/// Vsync pacing for the glide: a thread waits on DwmFlush (one return per composed frame)
/// and posts [`WM_GLIDE`], so the highlight moves once per refresh. WM_TIMER can't do
/// that: it never fires faster than ~10 ms and drifts against the refresh.
struct Ticker {
    run: AtomicBool,
    /// A tick is queued and not yet handled; keeps a slow frame from piling up ticks.
    pending: AtomicBool,
}

const HINT: &str = "Drag to select a region   ·   Click to record the highlighted window or monitor   ·   Esc to cancel";

/// A pre-rendered pill (label or hint) and where it sits, in client coordinates.
struct Pill {
    rect: Rect,
    dib: Dib,
    text: String,
}

/// The highlight moving from `from` to the current target.
#[derive(Clone, Copy)]
struct Glide {
    from: Rect,
    start: Instant,
    ms: f32,
    /// A drag box flipping across its start point (it keeps following the drag).
    flip: bool,
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
    /// Visible top-level windows when the picker opened, topmost first (desktop coordinates).
    windows: Vec<Rect>,
    /// Last mouse position (client coordinates).
    cursor: (i32, i32),
    drag_start: Option<(i32, i32)>,
    /// The mouse moved past [`CLICK_SLOP`] since the button went down.
    dragging: bool,
    /// Which side of the drag start the cursor is on (left, above); a change flips the box.
    side: Option<(bool, bool)>,
    /// The encoder's smallest frame; selections grow to it.
    min: (i32, i32),
    /// Current (fitted) selection or hover highlight in client coordinates, as displayed.
    sel: Option<Rect>,
    label: Option<Pill>,
    /// What the highlight shows or glides to (client coordinates) and the output it's on.
    target: Option<(Rect, usize)>,
    /// The running glide, if any.
    glide: Option<Glide>,
    /// Runs while a glide is in flight.
    ticker: Option<Arc<Ticker>>,
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
            if let Some(t) = &self.ticker {
                t.run.store(false, Ordering::Relaxed);
            }
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
pub fn open(owner: HWND, msg: u32, outputs: Vec<Output>, min: (i32, i32)) -> Result<HWND, String> {
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
        let windows = visible_windows();
        let _ = GdiFlush();
        for (d, &s) in dim.pixels_mut().iter_mut().zip(bright.pixels()) {
            let rb = (((s & 0x00FF_00FF) * DIM) >> 8) & 0x00FF_00FF;
            let g = (((s & 0x0000_FF00) * DIM) >> 8) & 0x0000_FF00;
            *d = rb | g;
        }

        let painter = Painter::new()?;
        let font = painter.font(13.0)?;
        let (hw, _) = pill_size(&painter, &font, HINT);
        // The hint goes into both images so a highlighted window doesn't hide it.
        for o in &outputs {
            let r = o.rect.offset(-vx, -vy);
            let (x, y) = (r.x + (r.w - hw) / 2, r.y + painter.px(28.0) as i32);
            for img in [&dim, &bright] {
                let hint = render_pill(&painter, &font, HINT, img, x, y)?;
                let p = hint.rect;
                let _ = BitBlt(img.dc, p.x, p.y, p.w, p.h, Some(hint.dib.dc), 0, 0, SRCCOPY);
            }
        }
        let _ = GdiFlush();

        let hinst = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = w!("glint_overlay");
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
            w!("glint"),
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
            let mut st = State {
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
                windows,
                cursor: (0, 0),
                drag_start: None,
                dragging: false,
                side: None,
                min,
                sel: None,
                label: None,
                target: None,
                glide: None,
                ticker: None,
                done: false,
                last_paint: Cell::new(Instant::now()),
                frame,
            };
            // Highlight what's under the cursor right away, before the first paint.
            let mut p = POINT::default();
            if GetCursorPos(&mut p).is_ok() {
                st.cursor = (p.x - vx, p.y - vy);
                st.update_selection(st.cursor);
            }
            *s.borrow_mut() = Some(st);
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

/// Visible top-level windows, topmost first, by their visible DWM frame (without the
/// invisible resize borders). Taken together with the screenshot, so hovering matches it.
fn visible_windows() -> Vec<Rect> {
    unsafe extern "system" fn add(hwnd: HWND, lp: LPARAM) -> BOOL {
        let list = unsafe { &mut *(lp.0 as *mut Vec<Rect>) };
        list.extend(unsafe { window_rect(hwnd) });
        TRUE
    }
    let mut list = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(add), LPARAM(&mut list as *mut Vec<Rect> as isize));
    }
    list
}

/// Windows thinner than this (physical px) in either direction are never picked.
const MIN_PICK: i32 = 24;

/// Where `hwnd` appears on screen, or None if the user can't see it as a window.
unsafe fn window_rect(hwnd: HWND) -> Option<Rect> {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        // Click-through windows are overlays (GPU, game, recording), not something to pick.
        if ex & WS_EX_TRANSPARENT.0 != 0 {
            return None;
        }
        // Fully transparent layered windows (e.g. the idle GeForce overlay) can't be seen.
        if ex & WS_EX_LAYERED.0 != 0 {
            let (mut alpha, mut flags) = (255u8, LAYERED_WINDOW_ATTRIBUTES_FLAGS(0));
            if GetLayeredWindowAttributes(hwnd, None, Some(&mut alpha), Some(&mut flags)).is_ok()
                && flags.contains(LWA_ALPHA)
                && alpha == 0
            {
                return None;
            }
        }
        // Cloaked: on another virtual desktop, a suspended UWP app, or a shell surface.
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as _, 4);
        if cloaked != 0 {
            return None;
        }
        // The desktop itself; hovering it highlights the whole monitor instead.
        let mut buf = [0u16; 16];
        let n = GetClassNameW(hwnd, &mut buf).max(0) as usize;
        let class = &buf[..n];
        if [w!("Progman"), w!("WorkerW")].iter().any(|c| c.as_wide() == class) {
            return None;
        }
        let mut rc = RECT::default();
        let size = size_of::<RECT>() as u32;
        if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut rc as *mut RECT as _, size).is_err() {
            GetWindowRect(hwnd, &mut rc).ok()?;
        }
        let r = Rect::from_win(&rc);
        // Slivers are drop shadows, borders and helper windows, not pick targets.
        (r.w >= MIN_PICK && r.h >= MIN_PICK).then_some(r)
    }
}

impl State {
    fn to_client(&self, r: Rect) -> Rect {
        r.offset(-self.origin.0, -self.origin.1)
    }

    /// The selection that would be recorded for a drag from `a` to `b` (client coords).
    fn fitted(&self, a: (i32, i32), b: (i32, i32)) -> Option<(Rect, usize)> {
        let (ox, oy) = self.origin;
        fit_drag((a.0 + ox, a.1 + oy), (b.0 + ox, b.1 + oy), &self.outputs, self.min)
    }

    /// What a click at `p` (client coords) picks: the topmost window under it, else its monitor.
    fn target_at(&self, p: (i32, i32)) -> Option<(Rect, usize)> {
        let d = (p.0 + self.origin.0, p.1 + self.origin.1);
        let mon = self.outputs[output_at(&self.outputs, d.0, d.1)?].rect;
        let rect = self.windows.iter().find(|w| w.contains(d.0, d.1)).copied().unwrap_or(mon);
        fit_rect(rect, d, &self.outputs, self.min)
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

    /// Shows what the mouse at `cur` would pick: the drag selection while dragging, otherwise
    /// the window (or monitor) under the cursor, gliding there from the last one. A drag
    /// follows the cursor instantly, except for jumps (the drag starting, the box flipping
    /// across its start point), which glide.
    fn update_selection(&mut self, cur: (i32, i32)) {
        let drag = self.drag_start.filter(|_| self.dragging);
        let target = match drag {
            Some(start) => self.fitted(start, cur),
            None => self.target_at(cur),
        }
        .map(|(r, oi)| (self.to_client(r), oi));
        if target == self.target {
            return; // unchanged, or already gliding there
        }
        self.target = target;
        let glide = match (drag, self.sel, target) {
            (Some(start), Some(from), Some(_)) => {
                let side = (cur.0 < start.0, cur.1 < start.1);
                // The first drag frame turns the hover highlight into the drag box; a flip moves
                // the box to the other side of its start. Both glide, following the cursor.
                let jumped = self.side.replace(side).is_none_or(|s| s != side);
                match self.glide {
                    _ if jumped => Some(Glide { from, start: Instant::now(), ms: FLIP_MS, flip: true }),
                    Some(g) if g.flip => Some(g), // keep gliding toward the moving box
                    _ => None,
                }
            }
            (None, Some(from), Some((to, _))) if from != to => {
                Some(Glide { from, start: Instant::now(), ms: GLIDE_MS, flip: false })
            }
            _ => None,
        };
        self.glide = glide;
        if self.glide.is_some() {
            self.set_gliding(true);
        } else {
            self.show(target.map(|(r, _)| r));
        }
    }

    /// Shows the next glide frame; returns whether the glide is still running.
    fn step_glide(&mut self) -> bool {
        let (Some(Glide { from, start, ms, .. }), Some((to, _))) = (self.glide, self.target) else { return false };
        let t = (start.elapsed().as_secs_f32() * 1000.0 / ms).min(1.0);
        let e = 1.0 - (1.0 - t).powi(3); // ease-out cubic
        let mix = |a: i32, b: i32| a + ((b - a) as f32 * e).round() as i32;
        self.show(Some(Rect { x: mix(from.x, to.x), y: mix(from.y, to.y), w: mix(from.w, to.w), h: mix(from.h, to.h) }));
        if t >= 1.0 {
            self.glide = None;
        }
        self.glide.is_some()
    }

    fn set_gliding(&mut self, on: bool) {
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

    /// Displays the highlight at `sel` (client coordinates), labelled with the target's size.
    fn show(&mut self, sel: Option<Rect>) {
        let (Some(sel), Some((target, oi))) = (sel, self.target) else {
            return self.set_selection(None, Some(None));
        };
        let mon = self.to_client(self.outputs[oi].rect);
        let text = format!("{} × {}   ·   {} fps", target.w, target.h, self.outputs[oi].native_fps());
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
        let fitted = if std::mem::take(&mut self.dragging) { self.fitted(start, end) } else { self.target_at(end) };
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
        if msg == WM_GLIDE {
            if let Some(t) = &self.ticker {
                t.pending.store(false, Ordering::Relaxed);
            }
            if !self.step_glide() {
                self.set_gliding(false);
            }
            return Action::Paint;
        }
        if self.done {
            return match msg {
                WM_CLOSE | WM_KEYDOWN | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONUP | WM_MOUSEMOVE => Action::Handled,
                _ => Action::Default,
            };
        }
        match msg {
            WM_LBUTTONDOWN => {
                self.drag_start = Some(point(lp));
                self.dragging = false;
                self.side = None;
                Action::Capture
            }
            WM_MOUSEMOVE => {
                let p = point(lp);
                self.cursor = p;
                if let Some(s) = self.drag_start
                    && !self.dragging
                {
                    self.dragging = (p.0 - s.0).abs() >= CLICK_SLOP || (p.1 - s.1).abs() >= CLICK_SLOP;
                }
                self.update_selection(p);
                if self.last_paint.get().elapsed() >= self.frame { Action::Paint } else { Action::Handled }
            }
            WM_LBUTTONUP => match self.finish_drag(point(lp)) {
                Some(sel) => Action::Finish(Some(sel)),
                None => {
                    self.update_selection(point(lp));
                    Action::Release
                }
            },
            WM_RBUTTONUP => {
                if self.drag_start.take().is_some() {
                    self.dragging = false;
                    self.update_selection(point(lp));
                    Action::Release
                } else {
                    Action::Finish(None)
                }
            }
            WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => Action::Finish(None),
            // Enter confirms the drag in progress, or picks what's highlighted.
            WM_KEYDOWN if wp.0 == VK_RETURN.0 as usize => {
                self.drag_start.get_or_insert(self.cursor);
                match self.finish_drag(self.cursor) {
                    Some(sel) => Action::Finish(Some(sel)),
                    None => Action::Handled,
                }
            }
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
