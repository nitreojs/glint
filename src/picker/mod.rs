//! Full-screen region picker in the style of ShareX: a frozen, dimmed screenshot of the
//! whole virtual desktop that fades in. Hovering highlights the window under the cursor (or
//! the whole monitor over the desktop) and a click picks it; dragging selects a region (the
//! selection shows undimmed). Esc / right-click cancels. For screenshots the picked region
//! then opens in the markup editor ([`crate::shot::editor`]) on the same frozen screen.
//!
//! The result is posted to the owner as `Box<Option<Picked>>` in LPARAM; the picker keeps
//! showing until the owner calls [`dismiss`], so the fade-out can overlap other work.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, TRUE, WPARAM};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmFlush, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, VK_CONTROL, VK_ESCAPE, VK_RETURN, VK_SHIFT, VK_SPACE,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, w};

use crate::platform::gfx::{Painter, animate};
use crate::platform::log::log;
use crate::platform::monitors::Output as Monitor;
use crate::platform::rect::Rect;
use crate::platform::windows::visible_windows;
use crate::shot::Shot;
use crate::shot::editor::{self, Editor};

mod fit;
mod frozen;
mod glide;
mod live;
mod paint;
mod pill;

use fit::{Fit, fit_drag, fit_rect, output_at};
use frozen::Frozen;
use glide::{FLIP_MS, GLIDE_MS, Glide, Ticker, WM_GLIDE};
use live::Live;
use pill::{Pill, hint_rects, hint_text, pill_size, render_pill};

#[derive(Clone, Debug)]
pub(crate) struct Selection {
    /// Desktop coordinates, already clipped to `output` with even size.
    pub rect: Rect,
    pub output: Monitor,
}

/// What the picker is for.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Purpose {
    /// Selections are sized for the encoder: its smallest frame (`min`), even sides.
    Record { min: (i32, i32) },
    /// The picked region opens in the markup editor.
    Shot,
}

pub(crate) enum Picked {
    Region(Selection),
    Shot(Shot),
}

/// Selection outline: 1 px white inside 1 px black, both outside the selected pixels.
const RING: i32 = 2;
const CLICK_SLOP: i32 = 4;
/// Brightness kept outside the selection, out of 256.
const DIM: u32 = 150;
const FADE_IN_MS: f32 = 140.0;
const FADE_OUT_MS: f32 = 110.0;
const NOTICE_TIMER: usize = 0x6E01;

enum Screen {
    Frozen(Frozen),
    Live(Live),
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

fn fade_window(hwnd: HWND, t: f32) {
    unsafe {
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), (t * 255.0) as u8, LWA_ALPHA);
    }
}

/// Fades the picker in. `msg` is posted to `owner` with the result.
pub(crate) fn open(owner: HWND, msg: u32, outputs: Vec<Monitor>, purpose: Purpose, freeze: bool) -> Result<HWND, String> {
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
pub(crate) fn cancel(hwnd: HWND) {
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}

/// Fades the picker out and destroys it. Returns once the desktop is visible again.
pub(crate) fn dismiss(hwnd: HWND) {
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
