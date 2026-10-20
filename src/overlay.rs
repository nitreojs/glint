//! Full-screen region picker in the style of ShareX: a frozen, dimmed screenshot of the
//! whole virtual desktop that fades in. Hovering highlights the window under the cursor (or
//! the whole monitor over the desktop) and a click picks it; dragging selects a region (the
//! selection shows undimmed). Esc / right-click cancels. For screenshots the picked region
//! then opens in the markup editor ([`crate::editor`]) on the same frozen screen.
//!
//! The result is posted to the owner as `Box<Option<Picked>>` in LPARAM; the picker keeps
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
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, VK_CONTROL, VK_ESCAPE, VK_RETURN, VK_SHIFT, VK_SPACE,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, w};

use crate::editor::{self, Editor, Output};
use crate::gfx::{Canvas, Dib, Painter, animate, rgba};
use crate::monitors::{Fit, Output as Monitor, fit_drag, fit_rect, output_at};
use crate::util::{Rect, log};

mod live;
use live::Live;

#[derive(Clone, Debug)]
pub struct Selection {
    /// Desktop coordinates, already clipped to `output` with even size.
    pub rect: Rect,
    pub output: Monitor,
}

/// What the picker is for.
#[derive(Clone, Copy, Debug)]
pub enum Purpose {
    /// Selections are sized for the encoder: its smallest frame (`min`), even sides.
    Record { min: (i32, i32) },
    /// The picked region opens in the markup editor.
    Shot,
}

/// A finished screenshot as 0x00RRGGBB rows, and what to do with it.
pub struct Shot {
    pub pixels: Vec<u32>,
    pub w: u32,
    pub h: u32,
    pub output: Output,
}

pub enum Picked {
    Region(Selection),
    Shot(Shot),
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
const NOTICE_TIMER: usize = 0x6E01;

/// Vsync pacing for the glide: a thread waits on DwmFlush (one return per composed frame)
/// and posts [`WM_GLIDE`], so the highlight moves once per refresh. WM_TIMER can't do
/// that: it never fires faster than ~10 ms and drifts against the refresh.
struct Ticker {
    run: AtomicBool,
    /// A tick is queued and not yet handled; keeps a slow frame from piling up ticks.
    pending: AtomicBool,
}

const HINT_RECORD: &str = "Drag to select a region   ·   Click to record the highlighted window or monitor   ·   Esc to cancel";
const HINT_SHOT: &str = "Drag to select a region   ·   Click to capture the highlighted window or monitor   ·   Esc to cancel";

/// A pre-rendered pill (label or hint) and where it sits, in client coordinates.
struct Pill {
    rect: Rect,
    dib: Option<Dib>,
    text: String,
}

struct Frozen {
    bright: Dib,
    dim: Dib,
    /// The hint pills drawn over the bright image, shown where a highlight covers a hint.
    hints: Vec<Pill>,
}

enum Screen {
    Frozen(Frozen),
    Live(Live),
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
    size: (i32, i32),
    screen: Screen,
    painter: Painter,
    font: IDWriteTextFormat,
    white: HBRUSH,
    black: HBRUSH,
    outputs: Vec<Monitor>,
    /// Visible top-level windows when the picker opened, topmost first (desktop coordinates).
    windows: Vec<Rect>,
    /// Last mouse position (client coordinates).
    cursor: (i32, i32),
    drag_start: Option<(i32, i32)>,
    /// The mouse moved past [`CLICK_SLOP`] since the button went down.
    dragging: bool,
    /// Which side of the drag start the cursor is on (left, above); a change flips the box.
    side: Option<(bool, bool)>,
    purpose: Purpose,
    /// How selections are sized (from the purpose).
    fit: Fit,
    /// Current (fitted) selection or hover highlight in client coordinates, as displayed.
    sel: Option<Rect>,
    label: Option<Pill>,
    /// Markup editor, once a screenshot region is picked.
    edit: Option<Editor>,
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
    ShowFrozen,
    Finish(Option<Picked>),
    /// WM_SETCURSOR: show this system cursor.
    Cursor(PCWSTR),
}

/// Renders a pill with `text` over a copy of `bg` at (x, y).
fn render_pill(painter: &Painter, font: &IDWriteTextFormat, text: &str, bg: &Dib, x: i32, y: i32) -> Result<Pill, String> {
    let (w, h) = pill_size(painter, font, text);
    let dib = Dib::new(w, h)?;
    unsafe {
        let _ = BitBlt(dib.dc, 0, 0, dib.w, dib.h, Some(bg.dc), x, y, SRCCOPY);
    }
    painter.draw(&dib, |c| draw_pill(c, painter, font, text, w as f32, h as f32))?;
    Ok(Pill { rect: Rect { x, y, w, h }, dib: Some(dib), text: text.to_string() })
}

fn draw_pill(c: &Canvas, painter: &Painter, font: &IDWriteTextFormat, text: &str, w: f32, h: f32) {
    let pad = painter.px(11.0);
    c.round_rect(0.0, 0.0, w, h, h / 2.0, PILL_BG);
    c.text(text, font, [pad, 0.0, w - pad * 2.0 + 2.0, h], PILL_TEXT);
}

fn pill_size(painter: &Painter, font: &IDWriteTextFormat, text: &str) -> (i32, i32) {
    let (tw, _) = painter.measure(text, font);
    ((tw.ceil() + painter.px(11.0) * 2.0) as i32, painter.px(26.0) as i32)
}

fn hint_text(purpose: Purpose) -> &'static str {
    match purpose {
        Purpose::Record { .. } => HINT_RECORD,
        Purpose::Shot => HINT_SHOT,
    }
}

fn hint_rects(painter: &Painter, font: &IDWriteTextFormat, hint: &str, outputs: &[Monitor], origin: (i32, i32)) -> Vec<Rect> {
    let (w, h) = pill_size(painter, font, hint);
    let top = painter.px(28.0) as i32;
    outputs
        .iter()
        .map(|o| {
            let r = o.rect.offset(-origin.0, -origin.1);
            Rect { x: r.x + (r.w - w) / 2, y: r.y + top, w, h }
        })
        .collect()
}

impl Frozen {
    fn capture(area: Rect, hints: &[Rect], hint: &str, painter: &Painter, font: &IDWriteTextFormat) -> Result<Frozen, String> {
        unsafe {
            let bright = Dib::new(area.w, area.h)?;
            let mut dim = Dib::new(area.w, area.h)?;
            let screen = GetDC(None);
            let _ = BitBlt(bright.dc, 0, 0, area.w, area.h, Some(screen), area.x, area.y, ROP_CODE(SRCCOPY.0 | CAPTUREBLT.0));
            ReleaseDC(None, screen);
            let _ = GdiFlush();
            for (d, &s) in dim.pixels_mut().iter_mut().zip(bright.pixels()) {
                let rb = (((s & 0x00FF_00FF) * DIM) >> 8) & 0x00FF_00FF;
                let g = (((s & 0x0000_FF00) * DIM) >> 8) & 0x0000_FF00;
                *d = rb | g;
            }
            // The hint is baked into the dimmed image only: the bright one is what a screenshot
            // captures. A copy drawn over the bright image shows where a highlight covers it.
            let mut pills = Vec::with_capacity(hints.len());
            for r in hints {
                let on_dim = render_pill(painter, font, hint, &dim, r.x, r.y)?;
                if let Some(src) = &on_dim.dib {
                    let _ = BitBlt(dim.dc, r.x, r.y, r.w, r.h, Some(src.dc), 0, 0, SRCCOPY);
                }
                pills.push(render_pill(painter, font, hint, &bright, r.x, r.y)?);
            }
            let _ = GdiFlush();
            Ok(Frozen { bright, dim, hints: pills })
        }
    }
}

fn fade_window(hwnd: HWND, t: f32) {
    unsafe {
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), (t * 255.0) as u8, LWA_ALPHA);
    }
}

/// Fades the picker in. `msg` is posted to `owner` with the result.
pub fn open(owner: HWND, msg: u32, outputs: Vec<Monitor>, purpose: Purpose, freeze: bool) -> Result<HWND, String> {
    unsafe {
        let area = Rect {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            w: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            h: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        };
        let windows = visible_windows();
        let painter = Painter::new()?;
        let font = painter.font(13.0)?;
        let hint = hint_text(purpose);
        let hints = hint_rects(&painter, &font, hint, &outputs, (area.x, area.y));
        let screen = if freeze {
            Screen::Frozen(Frozen::capture(area, &hints, hint, &painter, &font)?)
        } else {
            Screen::Live(Live::new(area, &hints, hint, &painter, &font)?)
        };

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
            area.x,
            area.y,
            area.w,
            area.h,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .map_err(|e| format!("CreateWindowEx: {e}"))?;
        let on = TRUE;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, &on as *const BOOL as _, 4);
        // layered windows at alpha 0 let the mouse through
        let alpha = if freeze { 0 } else { 1 };
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA);

        let max_hz = outputs.iter().map(|o| o.refresh_hz).fold(60.0, f64::max);
        let frame = Duration::from_secs_f64(0.9 / max_hz);
        STATE.with(|s| {
            let mut st = State {
                hwnd,
                owner,
                msg,
                origin: (area.x, area.y),
                size: (area.w, area.h),
                screen,
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
                purpose,
                fit: match purpose {
                    Purpose::Record { min } => Fit { min, even: true },
                    Purpose::Shot => Fit::ANY,
                },
                sel: None,
                label: None,
                edit: None,
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
                st.cursor = (p.x - area.x, p.y - area.y);
                st.update_selection(st.cursor);
            }
            *s.borrow_mut() = Some(st);
        });
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);
        let _ = SetForegroundWindow(hwnd);
        animate(FADE_IN_MS, |t| {
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    st.set_fade(t);
                }
            })
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
    let mut st = STATE.with(|s| {
        let mut g = s.borrow_mut();
        if g.as_ref().is_some_and(|st| st.hwnd == hwnd) { g.take() } else { None }
    });
    unsafe {
        let _ = ReleaseCapture();
        animate(FADE_OUT_MS, |t| match st.as_mut() {
            Some(st) => st.set_fade(1.0 - t),
            None => fade_window(hwnd, 1.0 - t),
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
pub(crate) fn ring(outer: Rect, inner: Rect) -> [RECT; 4] {
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
        fit_drag((a.0 + ox, a.1 + oy), (b.0 + ox, b.1 + oy), &self.outputs, self.fit)
    }

    /// What a click at `p` (client coords) picks: the topmost window under it, else its monitor.
    fn target_at(&self, p: (i32, i32)) -> Option<(Rect, usize)> {
        let d = (p.0 + self.origin.0, p.1 + self.origin.1);
        let mon = self.outputs[output_at(&self.outputs, d.0, d.1)?].rect;
        let rect = self.windows.iter().find(|w| w.contains(d.0, d.1)).copied().unwrap_or(mon);
        fit_rect(rect, d, &self.outputs, self.fit)
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
        if matches!(self.screen, Screen::Live(_)) {
            return self.schedule_live_sync();
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

    fn schedule_live_sync(&self) {
        let corner = RECT { left: 0, top: 0, right: 1, bottom: 1 };
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), Some(&corner), false);
        }
    }

    fn set_fade(&mut self, t: f32) {
        match &mut self.screen {
            Screen::Frozen(_) => fade_window(self.hwnd, t),
            Screen::Live(live) => live.set_fade(t, &self.painter, &self.font),
        }
    }

    fn freeze(&mut self) -> Result<(), String> {
        let Screen::Live(live) = &self.screen else { return Ok(()) };
        live.hide();
        fade_window(self.hwnd, 0.0);
        unsafe {
            let _ = DwmFlush();
        }
        let area = Rect { x: self.origin.0, y: self.origin.1, w: self.size.0, h: self.size.1 };
        let hint = hint_text(self.purpose);
        let hints = hint_rects(&self.painter, &self.font, hint, &self.outputs, self.origin);
        self.screen = Screen::Frozen(Frozen::capture(area, &hints, hint, &self.painter, &self.font)?);
        self.label = None;
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
        Ok(())
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
        let text = match self.purpose {
            Purpose::Record { .. } => format!("{} × {}   ·   {} fps", target.w, target.h, self.outputs[oi].native_fps()),
            Purpose::Shot => format!("{} × {}", target.w, target.h),
        };
        let same_text = self.label.as_ref().filter(|l| l.text == text);
        let (lw, lh) = match same_text {
            Some(l) => (l.rect.w, l.rect.h),
            None => pill_size(&self.painter, &self.font, &text),
        };
        let gap = self.painter.px(6.0) as i32;
        let above = sel.y - RING - gap - lh;
        // In the editor the toolbar may sit above the shot; the label then goes inside.
        let bar_above = self.edit.as_ref().is_some_and(|e| e.bar.bottom() <= sel.y);
        let (x, y, on_dim) = if above >= mon.y && !bar_above {
            (sel.x - RING, above, true)
        } else {
            (sel.x + gap + 2, sel.y + gap + 2, false)
        };
        let x = x.clamp(mon.x, (mon.right() - lw).max(mon.x));
        if same_text.is_some_and(|l| (l.rect.x, l.rect.y) == (x, y)) {
            return self.set_selection(Some(sel), None); // label unchanged
        }
        let label = match &self.screen {
            Screen::Frozen(f) => render_pill(&self.painter, &self.font, &text, if on_dim { &f.dim } else { &f.bright }, x, y).ok(),
            Screen::Live(_) => Some(Pill { rect: Rect { x, y, w: lw, h: lh }, dib: None, text }),
        };
        self.set_selection(Some(sel), Some(label));
    }

    /// The region a release at `end` picks: the drag, or what's highlighted after a click.
    fn finish_drag(&mut self, end: (i32, i32)) -> Option<(Rect, usize)> {
        let start = self.drag_start.take()?;
        if std::mem::take(&mut self.dragging) { self.fitted(start, end) } else { self.target_at(end) }
    }

    /// Recording: done. Screenshot: the region opens in the editor.
    fn picked(&mut self, (rect, oi): (Rect, usize)) -> Action {
        match self.purpose {
            Purpose::Record { .. } => Action::Finish(Some(Picked::Region(Selection { rect, output: self.outputs[oi].clone() }))),
            Purpose::Shot => {
                self.set_gliding(false);
                self.glide = None;
                let was_live = matches!(self.screen, Screen::Live(_));
                if let Err(e) = self.freeze() {
                    log(&format!("screenshot failed: {e}"));
                    return Action::Finish(None);
                }
                let sel = self.to_client(rect);
                let ed = Editor::new(sel, self.to_client(self.outputs[oi].rect), self.painter.scale);
                self.invalidate(&ed.extent());
                self.edit = Some(ed);
                self.target = Some((sel, oi));
                self.show(Some(sel));
                if was_live { Action::ShowFrozen } else { Action::Release }
            }
        }
    }

    fn invalidate(&self, rects: &[Rect]) {
        for r in rects {
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), Some(&r.to_win()), false);
            }
        }
    }

    /// Runs an editor call and turns its outcome into repaints or the picker's result.
    fn edit_event(&mut self, call: impl FnOnce(&mut Editor) -> editor::Event) -> Action {
        let Some(ed) = self.edit.as_mut() else { return Action::Handled };
        let before = ed.frame();
        let event = call(ed);
        if let Some(left) = ed.notice_left() {
            unsafe { SetTimer(Some(self.hwnd), NOTICE_TIMER, left.as_millis() as u32 + 1, None) };
        }
        match event {
            editor::Event::None => Action::Handled,
            editor::Event::Repaint => {
                let (damage, sel) = (ed.damage(&before), ed.sel);
                self.invalidate(&damage);
                if let Some((_, oi)) = self.target {
                    self.target = Some((sel, oi));
                    self.show(Some(sel));
                }
                if self.last_paint.get().elapsed() >= self.frame { Action::Paint } else { Action::Handled }
            }
            editor::Event::Cancel => Action::Finish(None),
            editor::Event::Finish(output) => {
                let Screen::Frozen(f) = &self.screen else { return Action::Finish(None) };
                match ed.pixels(&f.bright, &self.painter) {
                    Ok((pixels, w, h)) => Action::Finish(Some(Picked::Shot(Shot { pixels, w, h, output }))),
                    Err(e) => {
                        log(&format!("screenshot failed: {e}"));
                        Action::Finish(None)
                    }
                }
            }
        }
    }

    /// Every pixel is written exactly once per paint, so nothing flickers.
    unsafe fn paint(&mut self) {
        if let Screen::Live(live) = &mut self.screen {
            unsafe {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(self.hwnd, &mut ps);
                self.last_paint.set(Instant::now());
                FillRect(hdc, &ps.rcPaint, self.black);
                let _ = EndPaint(self.hwnd, &ps);
            }
            return live.sync(self.sel, self.label.as_ref(), &self.painter, &self.font);
        }
        // Rendering can change what the editor covers (a tooltip appears): paint that too.
        if let (Some(ed), Screen::Frozen(f)) = (self.edit.as_mut(), &self.screen) {
            let before = ed.extent();
            if let Err(e) = ed.render(&f.bright, &self.painter, &self.font) {
                log(&format!("editor render failed: {e}"));
            }
            let after = ed.extent();
            if after != before {
                self.invalidate(&after);
            }
        }
        let Screen::Frozen(f) = &self.screen else { return };
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(self.hwnd, &mut ps);
            self.last_paint.set(Instant::now());
            let rp = Rect::from_win(&ps.rcPaint);
            let label = self.label.as_ref();

            // Dimmed screen around everything else.
            let saved = SaveDC(hdc);
            let mut holes: Vec<Rect> = self.sel.iter().map(|s| s.inflate(RING)).collect();
            holes.extend(label.map(|l| l.rect));
            for h in &holes {
                ExcludeClipRect(hdc, h.x, h.y, h.right(), h.bottom());
            }
            let _ = BitBlt(hdc, rp.x, rp.y, rp.w, rp.h, Some(f.dim.dc), rp.x, rp.y, SRCCOPY);
            let _ = RestoreDC(hdc, saved);

            if let Some(ed) = &self.edit {
                if let Err(e) = ed.paint(hdc, &self.painter, &self.font, self.white, self.black) {
                    log(&format!("editor paint failed: {e}"));
                }
            } else if let Some(s) = self.sel {
                let saved = SaveDC(hdc);
                if let Some(l) = label {
                    ExcludeClipRect(hdc, l.rect.x, l.rect.y, l.rect.right(), l.rect.bottom());
                }
                if let Some(i) = s.intersect(&rp) {
                    let _ = BitBlt(hdc, i.x, i.y, i.w, i.h, Some(f.bright.dc), i.x, i.y, SRCCOPY);
                }
                for r in ring(s.inflate(1), s) {
                    FillRect(hdc, &r, self.white);
                }
                for r in ring(s.inflate(2), s.inflate(1)) {
                    FillRect(hdc, &r, self.black);
                }
                IntersectClipRect(hdc, s.x, s.y, s.right(), s.bottom());
                for h in f.hints.iter().filter(|h| h.rect.intersect(&s).is_some()) {
                    if let Some(dib) = &h.dib {
                        let r = h.rect;
                        let _ = BitBlt(hdc, r.x, r.y, r.w, r.h, Some(dib.dc), 0, 0, SRCCOPY);
                    }
                }
                let _ = RestoreDC(hdc, saved);
            }
            if let Some(l) = label
                && let Some(dib) = &l.dib
            {
                let r = l.rect;
                let _ = BitBlt(hdc, r.x, r.y, r.w, r.h, Some(dib.dc), 0, 0, SRCCOPY);
            }
            let _ = EndPaint(self.hwnd, &ps);
        }
    }

    /// Input while the markup editor is open.
    fn handle_edit(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Action {
        match msg {
            WM_LBUTTONDOWN => {
                let p = point(lp);
                match self.edit_event(|e| e.mouse_down(p)) {
                    Action::Finish(r) => Action::Finish(r),
                    _ => Action::Capture,
                }
            }
            WM_MOUSEMOVE => {
                let p = point(lp);
                self.cursor = p;
                self.edit_event(|e| e.mouse_move(p))
            }
            WM_LBUTTONUP => {
                let p = point(lp);
                match self.edit_event(|e| e.mouse_up(p)) {
                    Action::Finish(r) => Action::Finish(r),
                    _ => Action::Release,
                }
            }
            // bit 30 of lparam: the key was already down (auto-repeat)
            WM_KEYDOWN if lp.0 & (1 << 30) != 0 && (wp.0 == VK_RETURN.0 as usize || wp.0 == VK_SPACE.0 as usize) => {
                Action::Handled
            }
            WM_KEYDOWN => {
                let held = |vk: u16| unsafe { GetKeyState(vk as i32) } < 0;
                let (ctrl, shift) = (held(VK_CONTROL.0), held(VK_SHIFT.0));
                self.edit_event(|e| e.key(wp.0 as u32, ctrl, shift))
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as i32;
                self.edit_event(|e| e.wheel(delta))
            }
            WM_TIMER if wp.0 == NOTICE_TIMER => {
                let _ = unsafe { KillTimer(Some(self.hwnd), NOTICE_TIMER) };
                self.edit_event(|e| e.tick())
            }
            WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
                let mut p = POINT::default();
                let _ = unsafe { GetCursorPos(&mut p) };
                let at = (p.x - self.origin.0, p.y - self.origin.1);
                self.edit.as_ref().map_or(Action::Default, |e| Action::Cursor(e.cursor(at)))
            }
            WM_RBUTTONUP => Action::Handled,
            WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => Action::Finish(None),
            WM_CLOSE => Action::Finish(None),
            _ => Action::Default,
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
        if self.edit.is_some() {
            return self.handle_edit(msg, wp, lp);
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
                Some(picked) => self.picked(picked),
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
            WM_KEYDOWN if wp.0 == VK_RETURN.0 as usize || wp.0 == VK_SPACE.0 as usize => {
                self.drag_start.get_or_insert(self.cursor);
                match self.finish_drag(self.cursor) {
                    Some(picked) => self.picked(picked),
                    None => Action::Handled,
                }
            }
            WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => Action::Finish(None),
            WM_CLOSE => Action::Finish(None),
            _ => Action::Default,
        }
    }
}

fn finish(result: Option<Picked>) {
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
            Action::ShowFrozen => {
                let _ = ReleaseCapture();
                let _ = UpdateWindow(hwnd);
                fade_window(hwnd, 1.0);
                LRESULT(0)
            }
            Action::Finish(sel) => {
                finish(sel);
                LRESULT(0)
            }
            Action::Cursor(id) => {
                SetCursor(LoadCursorW(None, id).ok());
                LRESULT(1)
            }
        }
    }
}
