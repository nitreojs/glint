//! Full-screen region picker in the style of ShareX: a frozen, dimmed screenshot of the
//! whole virtual desktop that fades in. Hovering highlights the window under the cursor, or with
//! Shift (per the settings) the UI element inside it (the wheel walks out to bigger parts), or the
//! whole monitor over the desktop, and a click
//! picks it; Ctrl+click collects several into one pick; dragging selects a region, across
//! monitors if need be (the selection shows undimmed, its moving edges snap to nearby edges);
//! Ctrl+A selects every monitor at once. Esc / right-click cancels. For screenshots the picked
//! region then opens in the markup editor ([`crate::shot::editor`]) on the same frozen screen, or
//! in draw-first mode the editor opens on the whole screen and the picker returns on request.
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
    GetKeyState, ReleaseCapture, SetCapture, VK_A, VK_CONTROL, VK_ESCAPE, VK_RETURN, VK_SHIFT, VK_SPACE,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, w};

use crate::config::UiElements;
use crate::platform::elements::Elements;
use crate::platform::gfx::{Painter, animate};
use crate::platform::log::log;
use crate::platform::monitors::Output as Monitor;
use crate::platform::rect::Rect;
use crate::platform::snap::{self, Edges, snap_edge};
use crate::platform::windows::{client_rect, visible_windows};
use crate::shot::Shot;
use crate::shot::editor::{self, Editor};

mod fit;
mod frozen;
mod glide;
mod live;
mod paint;
mod pill;

use fit::{Fit, fit_drag, fit_rect, multi_pick, output_at, overlapped, toggle, window_pick};
use frozen::Frozen;
use glide::{FLIP_MS, GLIDE_MS, Glide, Ticker, WM_GLIDE};
use live::Live;
use pill::{Pill, hint_rects, hint_text, pill_size, render_pill};

#[derive(Clone, Debug)]
pub(crate) struct Selection {
    /// Desktop coordinates, inside the virtual screen; even sides for [`Purpose::Record`].
    pub rect: Rect,
    /// Every output `rect` overlaps, in enumeration order; never empty.
    pub outputs: Vec<Monitor>,
}

/// What the picker is for.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Purpose {
    /// Selections are sized for the encoder: its smallest frame (`min`), even sides.
    Record { min: (i32, i32) },
    /// The picked region opens in the markup editor; `draw_first` opens the editor on the whole
    /// screen straight away (ShareX style), with region picking as one of its tools.
    Shot { draw_first: bool },
}

pub(crate) enum Picked {
    Region(Selection),
    Shot(Shot),
}

/// Selection outline: 1 px white inside 1 px black, both outside the selected pixels.
const RING: i32 = 2;
const CLICK_SLOP: i32 = 4;
/// Brightness kept outside the selection, out of 256.
pub(crate) const DIM: u32 = 150;
const FADE_IN_MS: f32 = 140.0;
const FADE_OUT_MS: f32 = 110.0;
const NOTICE_TIMER: usize = 0x6E01;
/// Posted by [`Elements`] when its results change.
const WM_ELEMENTS: u32 = WM_APP + 41;

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
    windows: Vec<(HWND, Rect)>,
    /// The windows' and their client areas' rects, for snapping.
    frames: Vec<Rect>,
    ui: UiElements,
    /// Shift is held, as of the last key or mouse message.
    shift: bool,
    /// Started on first use: UI Automation switches on apps' accessibility trees.
    elements: Option<Elements>,
    /// The UI elements under `chain_at` (desktop coordinates), innermost first, then the window and
    /// its monitor; `depth` is the one hovering highlights (the wheel moves it).
    chain: Vec<Rect>,
    chain_at: (i32, i32),
    depth: usize,
    /// Ctrl+clicked elements, windows or monitors (desktop coordinates), picked together.
    chosen: Vec<Rect>,
    /// Everything shown bright: the chosen rects and the selection (client coordinates).
    lit: Vec<Rect>,
    /// Edges a drag snaps to while the button is down (desktop coordinates).
    snap: Option<Edges>,
    /// Draw-first mode while picking a region: the editor and its screen, back after the pick.
    parked: Option<(Editor, Screen)>,
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
    /// What the highlight shows or glides to (client coordinates).
    target: Option<Rect>,
    /// Ctrl+A pinned the highlight to every monitor (until a drag starts).
    whole: bool,
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

/// Fades the picker in. `msg` is posted to `owner` with the result. `elements` says when hovering
/// highlights UI elements inside windows instead of the windows.
pub(crate) fn open(
    owner: HWND,
    msg: u32,
    outputs: Vec<Monitor>,
    purpose: Purpose,
    freeze: bool,
    elements: UiElements,
) -> Result<HWND, String> {
    let draw_first = matches!(purpose, Purpose::Shot { draw_first: true });
    // drawing needs a still image
    let freeze = freeze || draw_first;
    unsafe {
        let area = Rect {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            w: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            h: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        };
        let windows = visible_windows();
        let mut frames: Vec<Rect> = windows.iter().map(|w| w.1).collect();
        frames.extend(windows.iter().filter_map(|w| client_rect(w.0)));
        let painter = Painter::new()?;
        let font = painter.font(13.0)?;
        let hint = hint_text(purpose, outputs.len(), elements);
        let hints = hint_rects(&painter, &font, &hint, &outputs, (area.x, area.y));
        let screen = if freeze {
            let mut frozen = Frozen::capture(area, &outputs)?;
            if !draw_first {
                frozen.add_hints(&hints, &hint, &painter, &font)?;
            }
            Screen::Frozen(frozen)
        } else {
            Screen::Live(Live::new(area, &hints, &hint, &painter, &font)?)
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
                frames,
                ui: elements,
                shift: false,
                elements: None,
                chain: Vec::new(),
                chain_at: (0, 0),
                depth: 0,
                chosen: Vec::new(),
                lit: Vec::new(),
                snap: None,
                parked: None,
                cursor: (0, 0),
                drag_start: None,
                dragging: false,
                side: None,
                purpose,
                fit: match purpose {
                    Purpose::Record { min } => Fit { min, even: true },
                    Purpose::Shot { .. } => Fit::ANY,
                },
                sel: None,
                label: None,
                edit: None,
                target: None,
                whole: false,
                glide: None,
                ticker: None,
                done: false,
                last_paint: Cell::new(Instant::now()),
                frame,
            };
            if elements == UiElements::Always {
                st.start_elements();
            }
            // Highlight what's under the cursor right away, before the first paint.
            let mut p = POINT::default();
            if GetCursorPos(&mut p).is_ok() {
                st.cursor = (p.x - area.x, p.y - area.y);
            }
            if draw_first {
                let screen = Rect { x: 0, y: 0, w: area.w, h: area.h };
                let monitors = st.outputs.iter().map(|o| st.to_client(o.rect)).collect();
                let mut ed = Editor::canvas(screen, monitors, st.painter.scale, st.cursor);
                ed.set_snap(st.snap_rects_client());
                st.edit = Some(ed);
            } else {
                st.track(st.cursor);
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

    /// The virtual screen (desktop coordinates).
    fn area(&self) -> Rect {
        Rect { x: self.origin.0, y: self.origin.1, w: self.size.0, h: self.size.1 }
    }

    /// The selection (desktop coords) that would be recorded for a drag from `a` to `b` (client coords).
    /// The cursor's edges snap to nearby edges unless Ctrl is held.
    fn fitted(&self, a: (i32, i32), b: (i32, i32)) -> Rect {
        let (ox, oy) = self.origin;
        let mut c = (b.0 + ox, b.1 + oy);
        if let Some(e) = &self.snap
            && unsafe { GetKeyState(VK_CONTROL.0 as i32) } >= 0
        {
            let t = snap::threshold(self.painter.scale);
            c = (snap_edge(c.0, &e.xs, t), snap_edge(c.1, &e.ys, t));
        }
        fit_drag((a.0 + ox, a.1 + oy), c, self.area(), self.fit)
    }

    /// Whether hovering shows UI elements now (the setting and Shift).
    fn parts(&self) -> bool {
        self.ui.used(self.shift)
    }

    /// What hovering `p` (client coords) highlights, in desktop coords: the UI element the wheel
    /// chose, else the topmost window under it, else its monitor.
    fn hovered(&self, p: (i32, i32)) -> Option<Rect> {
        let d = (p.0 + self.origin.0, p.1 + self.origin.1);
        if self.parts()
            && self.chain_at == d
            && let Some(&r) = self.chain.get(self.depth)
        {
            return Some(r);
        }
        let mon = self.outputs[output_at(&self.outputs, d.0, d.1)?].rect;
        let win = self
            .windows
            .iter()
            .filter(|w| w.1.contains(d.0, d.1))
            .find_map(|w| window_pick(w.1, &self.outputs).filter(|r| r.contains(d.0, d.1)));
        Some(win.unwrap_or(mon))
    }

    /// What the highlight shows for `p` (client coords), in desktop coords: every monitor after Ctrl+A,
    /// else what's hovered.
    fn target_at(&self, p: (i32, i32)) -> Option<Rect> {
        let area = self.area();
        if self.whole {
            return Some(fit_rect(area, area, self.fit));
        }
        self.hovered(p).map(|r| fit_rect(r, area, self.fit))
    }

    /// What a click at `p` (client coords) picks: the target, or with Ctrl+clicked rects those and the
    /// hovered one together.
    fn click_pick(&self, p: (i32, i32)) -> Option<(Rect, Option<Vec<Rect>>)> {
        if self.chosen.is_empty() {
            return self.target_at(p).map(|r| (r, None));
        }
        let mut set = self.chosen.clone();
        set.extend(self.hovered(p).filter(|h| !self.chosen.contains(h)));
        self.multi(&set)
    }

    /// The pick for several rects (desktop coords): their bounding box, cut out for screenshots.
    fn multi(&self, set: &[Rect]) -> Option<(Rect, Option<Vec<Rect>>)> {
        multi_pick(set, self.area(), self.fit, matches!(self.purpose, Purpose::Shot { .. }))
    }

    /// Ctrl+click at `p` (client coords): the hovered rect joins the chosen ones or leaves them.
    fn toggle_at(&mut self, p: (i32, i32)) {
        let Some(h) = self.hovered(p) else { return };
        let was_whole = std::mem::take(&mut self.whole);
        toggle(&mut self.chosen, h);
        if was_whole {
            self.update_selection(p);
        }
        self.show(self.sel);
    }

    fn start_elements(&mut self) {
        match Elements::start(self.windows.clone(), self.hwnd, WM_ELEMENTS) {
            Ok(el) => self.elements = Some(el),
            Err(e) => {
                log(&format!("ui element detection failed: {e}"));
                self.ui = UiElements::Off;
            }
        }
    }

    /// Asks for the UI elements under `p` (client coords) and takes whatever the detector has.
    fn track(&mut self, p: (i32, i32)) {
        if !self.parts() {
            return;
        }
        if self.elements.is_none() {
            self.start_elements();
        }
        if let Some(el) = &self.elements {
            el.request((p.0 + self.origin.0, p.1 + self.origin.1));
            self.refresh_chain();
        }
    }

    /// Shift pressed or released: the highlight glides between the window and the element under the cursor.
    fn set_shift(&mut self, held: bool) {
        if held == self.shift {
            return;
        }
        self.shift = held;
        self.track(self.cursor);
        self.update_selection(self.cursor);
    }

    /// Takes the detector's chain if it's for the cursor. The hovered rect stays hovered while it's in
    /// the new chain; otherwise the innermost element is.
    fn refresh_chain(&mut self) {
        let Some((at, found)) = self.elements.as_ref().and_then(Elements::latest) else { return };
        if at != (self.cursor.0 + self.origin.0, self.cursor.1 + self.origin.1) {
            return;
        }
        let area = self.area();
        let mut chain: Vec<Rect> = found.iter().filter_map(|r| r.intersect(&area)).collect();
        // The window rect ends the chain: picked like a hovered window, whole across its monitors.
        if let Some(last) = chain.last_mut()
            && let Some(w) = window_pick(*last, &self.outputs)
        {
            *last = w;
        }
        chain.extend(output_at(&self.outputs, at.0, at.1).map(|i| self.outputs[i].rect));
        chain.dedup();
        let hovered = self.chain.get(self.depth).filter(|_| self.chain_at == at);
        self.depth = hovered.and_then(|h| chain.iter().position(|c| c == h)).unwrap_or(0);
        self.chain = chain;
        self.chain_at = at;
    }

    /// Everything a selection's edges snap to (desktop coords): windows, their client areas, the UI
    /// elements found so far, monitors.
    fn snap_rects(&self) -> Vec<Rect> {
        let mut v = self.frames.clone();
        v.extend(self.outputs.iter().map(|o| o.rect));
        v.extend(self.elements.iter().flat_map(Elements::known));
        v
    }

    fn snap_rects_client(&self) -> Vec<Rect> {
        self.snap_rects().into_iter().map(|r| self.to_client(r)).collect()
    }

    /// Forgets the pick in progress and everything it shows.
    fn reset_pick(&mut self) {
        self.set_gliding(false);
        self.glide = None;
        self.drag_start = None;
        self.dragging = false;
        self.snap = None;
        self.whole = false;
        self.chosen.clear();
        self.lit.clear();
        self.target = None;
        self.sel = None;
        self.label = None;
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// Draw-first mode: the editor's Region tool shows the picker over the marked-up screen.
    fn pick_region(&mut self) -> Action {
        let (Some(ed), Screen::Frozen(f)) = (self.edit.as_ref(), &self.screen) else { return Action::Handled };
        let flat = match ed.flattened(&f.bright, &self.painter).and_then(Frozen::new) {
            Ok(flat) => flat,
            Err(e) => {
                log(&format!("can't pick a region: {e}"));
                return Action::Handled;
            }
        };
        let screen = std::mem::replace(&mut self.screen, Screen::Frozen(flat));
        self.parked = self.edit.take().map(|ed| (ed, screen));
        self.reset_pick();
        self.shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        self.track(self.cursor);
        self.update_selection(self.cursor);
        Action::Handled
    }

    /// Back to the parked editor, unchanged; without one the picker is cancelled.
    fn unpark(&mut self) -> Action {
        let Some((ed, screen)) = self.parked.take() else { return Action::Finish(None) };
        self.reset_pick();
        self.screen = screen;
        let sel = ed.has_selection().then_some(ed.sel);
        self.edit = Some(ed);
        if let Some(sel) = sel {
            self.target = Some(sel);
            self.show(Some(sel));
        }
        Action::Release
    }

    /// The monitor (client coords) a label for `r` (client coords) goes on: the one under its
    /// top-left corner, else the one where the part of `r` on it starts highest.
    fn label_monitor(&self, r: Rect) -> Option<Rect> {
        let d = r.offset(self.origin.0, self.origin.1);
        let mon = match output_at(&self.outputs, d.x, d.y) {
            Some(i) => self.outputs[i].rect,
            None => {
                self.outputs
                    .iter()
                    .filter_map(|o| o.rect.intersect(&d).map(|p| ((p.y, p.x), o.rect)))
                    .min_by_key(|&(at, _)| at)?
                    .1
            }
        };
        Some(self.to_client(mon))
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
        let frozen = Frozen::capture(self.area(), &self.outputs)?;
        self.screen = Screen::Frozen(frozen);
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
            Some(start) => Some(self.fitted(start, cur)),
            None => self.target_at(cur),
        }
        .map(|r| self.to_client(r));
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
            (None, Some(from), Some(to)) if from != to => {
                Some(Glide { from, start: Instant::now(), ms: GLIDE_MS, flip: false })
            }
            _ => None,
        };
        self.glide = glide;
        if self.glide.is_some() {
            self.set_gliding(true);
        } else {
            self.show(target);
        }
    }

    /// Displays the highlight at `sel` (client coordinates), labelled with the target's size
    /// (with Ctrl+clicked rects, the size of what a click picks) on a monitor that shows it.
    fn show(&mut self, sel: Option<Rect>) {
        let (Some(sel), Some(target)) = (sel, self.target) else {
            return self.set_selection(None, Some(None));
        };
        let multi = self.click_pick(self.cursor).filter(|_| !self.chosen.is_empty());
        let (near, target) = match multi {
            Some((r, _)) => (self.to_client(r), self.to_client(r)),
            None => (sel, target),
        };
        let text = match self.purpose {
            Purpose::Record { .. } => {
                let desktop = target.offset(self.origin.0, self.origin.1);
                let fps = overlapped(desktop, &self.outputs).map(Monitor::native_fps).max().unwrap_or_default();
                format!("{} × {}   ·   {} fps", target.w, target.h, fps)
            }
            Purpose::Shot { .. } => format!("{} × {}", target.w, target.h),
        };
        let same_text = self.label.as_ref().filter(|l| l.text == text);
        let (lw, lh) = match same_text {
            Some(l) => (l.rect.w, l.rect.h),
            None => pill_size(&self.painter, &self.font, &text),
        };
        let mon = self.label_monitor(near).or_else(|| self.label_monitor(target)).unwrap_or(near);
        let part = near.intersect(&mon).unwrap_or(mon);
        let gap = self.painter.px(6.0) as i32;
        let above = near.y - RING - gap - lh;
        // In the editor the toolbar may sit above the shot; the label then goes inside.
        let bar_above = self.edit.as_ref().is_some_and(|e| e.bar.bottom() <= near.y);
        let (x, y) = if part.y == near.y && above >= mon.y && !bar_above {
            (near.x - RING, above)
        } else {
            (part.x + gap + 2, part.y + gap + 2)
        };
        let x = x.clamp(mon.x, (mon.right() - lw).max(mon.x));
        let y = y.min((mon.bottom() - lh).max(mon.y));
        if same_text.is_some_and(|l| (l.rect.x, l.rect.y) == (x, y)) {
            return self.set_selection(Some(sel), None); // label unchanged
        }
        let on_bright = self.chosen.iter().map(|&c| self.to_client(c)).chain([sel]).any(|r| r.contains(x, y));
        let label = match &self.screen {
            Screen::Frozen(f) => render_pill(&self.painter, &self.font, &text, if on_bright { &f.bright } else { &f.dim }, x, y).ok(),
            Screen::Live(_) => Some(Pill { rect: Rect { x, y, w: lw, h: lh }, dib: None, text }),
        };
        self.set_selection(Some(sel), Some(label));
    }

    /// What a release at `end` picks (desktop coords): the drag, or what a click picks.
    fn finish_drag(&mut self, end: (i32, i32)) -> Option<(Rect, Option<Vec<Rect>>)> {
        let start = self.drag_start.take()?;
        let pick = if std::mem::take(&mut self.dragging) {
            Some((self.fitted(start, end), None))
        } else {
            self.click_pick(end)
        };
        self.snap = None;
        pick
    }

    /// Recording: done. Screenshot: the region opens in the editor, `cutout` (desktop coords) the
    /// only parts of it kept.
    fn picked(&mut self, rect: Rect, cutout: Option<Vec<Rect>>) -> Action {
        match self.purpose {
            Purpose::Record { .. } => {
                let outputs = overlapped(rect, &self.outputs).cloned().collect();
                Action::Finish(Some(Picked::Region(Selection { rect, outputs })))
            }
            Purpose::Shot { .. } => {
                self.set_gliding(false);
                self.glide = None;
                let was_live = matches!(self.screen, Screen::Live(_));
                if let Err(e) = self.freeze() {
                    log(&format!("screenshot failed: {e}"));
                    return Action::Finish(None);
                }
                let sel = self.to_client(rect);
                let cutout = cutout.map(|c| c.into_iter().map(|r| self.to_client(r)).collect());
                let mut ed = match self.parked.take() {
                    Some((mut ed, screen)) => {
                        self.reset_pick();
                        self.screen = screen;
                        ed.set_selection(sel, cutout);
                        ed
                    }
                    None => {
                        self.chosen.clear();
                        let monitors = self.outputs.iter().map(|o| self.to_client(o.rect)).collect();
                        Editor::new(sel, cutout, monitors, self.painter.scale)
                    }
                };
                ed.set_snap(self.snap_rects_client());
                self.invalidate(&ed.extent());
                self.edit = Some(ed);
                self.target = Some(sel);
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
                if self.target.is_some() {
                    self.target = Some(sel);
                    self.show(Some(sel));
                }
                if self.last_paint.get().elapsed() >= self.frame { Action::Paint } else { Action::Handled }
            }
            editor::Event::Cancel => Action::Finish(None),
            editor::Event::PickRegion => self.pick_region(),
            editor::Event::Finish(output) => {
                let Screen::Frozen(f) = &self.screen else { return Action::Finish(None) };
                match ed.pixels(&f.bright, &self.painter) {
                    Ok((pixels, rect, alpha)) => {
                        let (w, h, at) = (rect.w as u32, rect.h as u32, rect.offset(self.origin.0, self.origin.1));
                        Action::Finish(Some(Picked::Shot(Shot { pixels, w, h, alpha, at, output })))
                    }
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
            WM_CHAR => self.edit_event(|e| e.char(wp.0 as u16)),
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
        if msg == WM_ELEMENTS {
            if self.edit.is_none() {
                self.refresh_chain();
                if self.dragging {
                    self.snap = Some(snap::edges(&self.snap_rects()));
                }
                self.update_selection(self.cursor);
            }
            return Action::Handled;
        }
        if self.edit.is_some() {
            return self.handle_edit(msg, wp, lp);
        }
        let ctrl = || unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        match msg {
            WM_LBUTTONDOWN => {
                self.drag_start = Some(point(lp));
                self.dragging = false;
                self.side = None;
                self.snap = Some(snap::edges(&self.snap_rects()));
                Action::Capture
            }
            WM_MOUSEMOVE => {
                let p = point(lp);
                self.cursor = p;
                self.shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                self.track(p);
                let mut cleared = false;
                if let Some(s) = self.drag_start
                    && !self.dragging
                {
                    self.dragging = (p.0 - s.0).abs() >= CLICK_SLOP || (p.1 - s.1).abs() >= CLICK_SLOP;
                    self.whole &= !self.dragging;
                    cleared = self.dragging && !self.chosen.is_empty();
                    if cleared {
                        self.chosen.clear();
                    }
                }
                self.update_selection(p);
                if cleared {
                    self.set_selection(self.sel, None);
                }
                if self.last_paint.get().elapsed() >= self.frame { Action::Paint } else { Action::Handled }
            }
            WM_LBUTTONUP if !self.dragging && self.drag_start.is_some() && ctrl() => {
                self.drag_start = None;
                self.snap = None;
                self.toggle_at(point(lp));
                Action::Release
            }
            WM_LBUTTONUP => match self.finish_drag(point(lp)) {
                Some((rect, cutout)) => self.picked(rect, cutout),
                None => {
                    self.update_selection(point(lp));
                    Action::Release
                }
            },
            WM_RBUTTONUP => {
                if self.drag_start.take().is_some() {
                    self.dragging = false;
                    self.snap = None;
                    self.update_selection(point(lp));
                    Action::Release
                } else {
                    self.unpark()
                }
            }
            WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP if wp.0 == VK_SHIFT.0 as usize => {
                self.set_shift(matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN));
                Action::Handled
            }
            WM_KEYDOWN | WM_KEYUP if wp.0 == VK_CONTROL.0 as usize && self.dragging => {
                self.update_selection(self.cursor);
                Action::Handled
            }
            WM_MOUSEWHEEL if self.drag_start.is_none() => {
                let up = ((wp.0 >> 16) & 0xFFFF) as i16 > 0;
                let here = self.chain_at == (self.cursor.0 + self.origin.0, self.cursor.1 + self.origin.1);
                if here && self.parts() && !self.whole {
                    let last = self.chain.len().saturating_sub(1);
                    self.depth = if up { (self.depth + 1).min(last) } else { self.depth.saturating_sub(1) };
                    self.update_selection(self.cursor);
                }
                Action::Handled
            }
            WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => self.unpark(),
            WM_KEYDOWN if wp.0 == VK_A.0 as usize && ctrl() => {
                if self.drag_start.is_none() {
                    self.whole = true;
                    self.chosen.clear();
                    self.update_selection(self.cursor);
                    self.set_selection(self.sel, None);
                }
                Action::Handled
            }
            // Enter confirms the drag in progress or the Ctrl+clicked set, or picks what's highlighted.
            WM_KEYDOWN if wp.0 == VK_RETURN.0 as usize || wp.0 == VK_SPACE.0 as usize => {
                let pick = if self.drag_start.is_none() && !self.chosen.is_empty() {
                    self.multi(&self.chosen)
                } else {
                    self.drag_start.get_or_insert(self.cursor);
                    self.finish_drag(self.cursor)
                };
                match pick {
                    Some((rect, cutout)) => self.picked(rect, cutout),
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
