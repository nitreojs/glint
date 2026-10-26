//! Full-screen region picker in the style of ShareX: a frozen, dimmed screenshot of the
//! whole virtual desktop that fades in. Hovering highlights the window under the cursor, or with
//! Shift (per the settings) the UI element inside it (the wheel walks out to bigger parts), or the
//! whole monitor over the desktop (big targets once the cursor rests on them), and a click
//! picks it; Ctrl+click collects several into one pick; dragging selects a region, across
//! monitors if need be (the selection shows undimmed, its moving corner snaps to visible edges
//! nearby unless Alt is held, Ctrl moves the whole box); Ctrl+A selects every monitor at once.
//! Esc / right-click cancels. For screenshots the picked region then opens in the markup editor
//! ([`crate::shot::editor`]) on the same frozen screen. In draw-first mode the editor is up from
//! the start, toolbar and all, and while its region tool is on (the default) the picker picks over
//! whatever is drawn: the pick is the finished shot, or with Shift held it opens in the editor.
//!
//! The result is posted to the owner as `Box<Option<Picked>>` in LPARAM; the picker keeps
//! showing until the owner calls [`dismiss`], so the fade-out can overlap other work.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use glint_core::config::UiElements;
use glint_core::geom::dwell::{Dwell, Kind, Target};
use glint_core::geom::fit::{
    Fit, fit_drag, fit_rect, multi_pick, output_at, overlapped, shift_drag, toggle, window_pick,
};
use glint_core::geom::monitor::Output as Monitor;
use glint_core::geom::rect::Rect;
use glint_core::geom::snap::{self, Edges, snap_edge};
use glint_core::log::log;
use glint_core::naming::Source;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, TRUE, WPARAM};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmFlush, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, VK_A, VK_CONTROL, VK_ESCAPE, VK_MENU, VK_RETURN, VK_SHIFT, VK_SPACE,
    VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, w};

use crate::platform::cursor::{self, CursorImage};
use crate::platform::elements::Elements;
use glint_shell::gfx::{Painter, animate};
use glint_shell::identity::identify;
use crate::platform::windows::{client_rect, visible_windows};
use crate::shot::Shot;
use crate::shot::editor::{self, Editor};

mod frozen;
mod glide;
mod live;
mod paint;
mod pill;

use frozen::Frozen;
use glide::{FLIP_MS, GLIDE_MS, Glide, Ticker, WM_GLIDE};
use live::Live;
use pill::{Pill, hint_due, hint_rects, hint_text, label_spot, pill_size, render_pill};

#[derive(Clone, Debug)]
pub(crate) struct Selection {
    /// Desktop coordinates, inside the virtual screen; even sides for [`Purpose::Record`].
    pub rect: Rect,
    /// Every output `rect` overlaps, in enumeration order; never empty.
    pub outputs: Vec<Monitor>,
    /// The app window picked, when one app's window (or a part of it) was.
    pub source: Option<Source>,
}

/// What the picker is for.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Purpose {
    /// Selections are sized for the encoder: its smallest frame (`min`), even sides.
    Record { min: (i32, i32) },
    /// The picked region opens in the markup editor; `draw_first` opens the editor on the whole
    /// screen straight away (ShareX style), where a picked region is the finished shot.
    Shot { draw_first: bool },
}

pub(crate) enum Picked {
    Region(Selection),
    Shot(Shot),
}

/// What a pick takes (desktop coordinates): the rect, the parts of it kept for a multi-window
/// screenshot, and the windows picked (None for a monitor; empty for a drag).
type Pick = (Rect, Option<Vec<Rect>>, Vec<Option<HWND>>);

/// Selection outline: 1 px white inside 1 px black, both outside the selected pixels (inside on
/// sides at the edge of its monitors).
const RING: i32 = 2;
const CLICK_SLOP: i32 = 4;
/// Brightness kept outside the selection, out of 256.
pub(crate) const DIM: u32 = 150;
const FADE_IN_MS: f32 = 140.0;
const FADE_OUT_MS: f32 = 110.0;
const EDITOR_TIMER: usize = 0x6E01;
/// Fires when the cursor has rested on a big target long enough for the highlight to go there.
const DWELL_TIMER: usize = 0x6E02;
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
    clients: Vec<Option<Rect>>,
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
    /// Draw-first with the editor's region tool on: picking, over `flat` when something is drawn.
    region: bool,
    /// The frozen screen with the editor's marks drawn in.
    flat: Option<Frozen>,
    /// Holds the hover highlight back from big targets for a moment.
    dwell: Dwell,
    /// Last mouse position (client coordinates).
    cursor: (i32, i32),
    drag_start: Option<(i32, i32)>,
    /// Added to the cursor for the drag's free corner: Ctrl slides the whole box.
    slide: (i32, i32),
    /// Ctrl was down when the button went down (Ctrl+click); it slides the box only once pressed again.
    ctrl_at_press: bool,
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
    /// Markup editor: once a screenshot region is picked, or from the start in draw-first mode.
    edit: Option<Editor>,
    /// The app window the screenshot shows, from the last pick.
    source: Option<Source>,
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
    /// Screenshots: the cursor as it was when the picker opened (hotspot in client coordinates),
    /// for the editor to start with.
    shot_cursor: Option<(Arc<CursorImage>, (i32, i32))>,
    /// Screenshots: the picture the editor's cursor tool places.
    arrow: Option<Arc<CursorImage>>,
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

fn held(vk: u16) -> bool {
    unsafe { GetKeyState(vk as i32) < 0 }
}

/// Fades the picker in. `msg` is posted to `owner` with the result. `elements` says when hovering
/// highlights UI elements inside windows instead of the windows. `with_cursor`: a screenshot keeps
/// the cursor, as a mark the editor can move or delete.
pub(crate) fn open(
    owner: HWND,
    msg: u32,
    outputs: Vec<Monitor>,
    purpose: Purpose,
    freeze: bool,
    elements: UiElements,
    with_cursor: bool,
) -> Result<HWND, String> {
    let draw_first = matches!(purpose, Purpose::Shot { draw_first: true });
    let shot = matches!(purpose, Purpose::Shot { .. });
    // before the overlay's own cursor takes over
    let shot_cursor = if shot && with_cursor { cursor::current() } else { None };
    let arrow = if shot { cursor::arrow().map(Arc::new) } else { None };
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
        let clients: Vec<Option<Rect>> = windows.iter().map(|w| client_rect(w.0)).collect();
        let painter = Painter::new()?;
        let font = painter.font(13.0)?;
        let hint = hint_text(purpose, outputs.len(), elements);
        let hints = if hint_due() { hint_rects(&painter, &font, &hint, &outputs, (area.x, area.y)) } else { Vec::new() };
        let screen = if freeze {
            let mut frozen = Frozen::capture(area, &outputs)?;
            if !draw_first {
                frozen.add_hints(&hints, &hint, &painter, &font)?;
                // draw-first's editor shows it as a mark from the start
                if let Some((img, at)) = &shot_cursor {
                    frozen.show_cursor(img, (at.0 - area.x, at.1 - area.y));
                }
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
                clients,
                ui: elements,
                shift: false,
                elements: None,
                chain: Vec::new(),
                chain_at: (0, 0),
                depth: 0,
                chosen: Vec::new(),
                lit: Vec::new(),
                snap: None,
                region: false,
                flat: None,
                dwell: Dwell::default(),
                cursor: (0, 0),
                drag_start: None,
                slide: (0, 0),
                ctrl_at_press: false,
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
                source: None,
                target: None,
                whole: false,
                glide: None,
                ticker: None,
                done: false,
                last_paint: Cell::new(Instant::now()),
                frame,
                shot_cursor: shot_cursor.map(|(img, at)| (Arc::new(img), (at.0 - area.x, at.1 - area.y))),
                arrow,
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
                let mut ed = Editor::canvas(screen, st.monitors(), st.painter.scale, st.cursor);
                st.equip(&mut ed);
                st.add_cursor(&mut ed);
                schedule_tick(hwnd, &ed);
                st.edit = Some(ed);
                st.sync_mode();
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

    fn to_desktop(&self, p: (i32, i32)) -> (i32, i32) {
        (p.0 + self.origin.0, p.1 + self.origin.1)
    }

    /// The virtual screen (desktop coordinates).
    fn area(&self) -> Rect {
        Rect { x: self.origin.0, y: self.origin.1, w: self.size.0, h: self.size.1 }
    }

    /// The monitors in client coordinates.
    fn monitors(&self) -> Vec<Rect> {
        self.outputs.iter().map(|o| self.to_client(o.rect)).collect()
    }

    /// Ctrl (pressed since the button went down) slides the dragged box instead of resizing it.
    fn sliding(&self) -> bool {
        self.dragging && !self.ctrl_at_press && held(VK_CONTROL.0)
    }

    /// The drag's free corner for the cursor at `cur` (client coords).
    fn corner(&self, cur: (i32, i32)) -> (i32, i32) {
        (cur.0 + self.slide.0, cur.1 + self.slide.1)
    }

    /// The selection (desktop coords) for a drag from `a` to its free corner `b` (client coords).
    /// The corner snaps to visible edges nearby unless Alt is held or Ctrl is sliding the box.
    fn fitted(&self, a: (i32, i32), b: (i32, i32)) -> Rect {
        let mut c = self.to_desktop(b);
        if let Some(e) = &self.snap
            && !held(VK_MENU.0)
            && !self.sliding()
        {
            let t = snap::threshold(self.painter.scale);
            c = (snap_edge(c.0, (c.1, c.1), &e.xs, t), snap_edge(c.1, (c.0, c.0), &e.ys, t));
        }
        fit_drag(self.to_desktop(a), c, self.area(), self.fit)
    }

    /// The cursor moved from `last` to `p` with Ctrl sliding the box: the drag start and the free
    /// corner both move, as far as the screen allows.
    fn slide_drag(&mut self, last: (i32, i32), p: (i32, i32)) {
        let Some(start) = self.drag_start else { return };
        let d = (p.0 - last.0, p.1 - last.1);
        let client = Rect { x: 0, y: 0, w: self.size.0, h: self.size.1 };
        let moved = shift_drag(start, self.corner(last), d, client);
        self.drag_start = Some((start.0 + moved.0, start.1 + moved.1));
        self.slide = (self.slide.0 + moved.0 - d.0, self.slide.1 + moved.1 - d.1);
    }

    /// Whether hovering shows UI elements now (the setting and Shift).
    fn parts(&self) -> bool {
        self.ui.used(self.shift)
    }

    /// What hovering `p` (client coords) highlights, in desktop coords: the UI element the wheel
    /// chose, else the topmost window under it, else its monitor.
    fn hovered(&self, p: (i32, i32)) -> Option<Rect> {
        let d = self.to_desktop(p);
        if self.parts()
            && self.chain_at == d
            && let Some(&r) = self.chain.get(self.depth)
        {
            return Some(r);
        }
        let mon = self.outputs[output_at(&self.outputs, d.0, d.1)?].rect;
        let win = self.window_at(d).and_then(|i| window_pick(self.windows[i].1, &self.outputs));
        Some(win.unwrap_or(mon))
    }

    /// The topmost window whose pick contains desktop point `d`.
    fn window_at(&self, d: (i32, i32)) -> Option<usize> {
        self.windows.iter().position(|w| {
            w.1.contains(d.0, d.1) && window_pick(w.1, &self.outputs).is_some_and(|r| r.contains(d.0, d.1))
        })
    }

    /// Whether desktop rect `r` is a monitor (or all of them), a window, or a part of one.
    fn kind(&self, r: Rect) -> Kind {
        if r == self.area() || self.outputs.iter().any(|o| o.rect == r) {
            Kind::Monitor
        } else if self.windows.iter().any(|w| window_pick(w.1, &self.outputs) == Some(r)) {
            Kind::Window
        } else {
            Kind::Element
        }
    }

    /// The window picking desktop rect `r` takes: None for a monitor, else the window it is (or
    /// is part of, the topmost over its centre).
    fn owner(&self, r: Rect) -> Option<HWND> {
        if self.kind(r) == Kind::Monitor {
            return None;
        }
        let exact = self.windows.iter().find(|w| window_pick(w.1, &self.outputs) == Some(r));
        let centre = || self.window_at((r.x + r.w / 2, r.y + r.h / 2)).map(|i| &self.windows[i]);
        exact.or_else(centre).map(|w| w.0)
    }

    /// What the highlight shows for `p` (client coords), in desktop coords and unfitted: every
    /// monitor after Ctrl+A, else what's hovered.
    fn hover_target(&self, p: (i32, i32)) -> Option<Target> {
        if self.whole {
            return Some((self.area(), Kind::Monitor));
        }
        self.hovered(p).map(|r| (r, self.kind(r)))
    }

    /// What the highlight shows for `p` (client coords), in desktop coords.
    fn target_at(&self, p: (i32, i32)) -> Option<Rect> {
        self.hover_target(p).map(|(r, _)| fit_rect(r, self.area(), self.fit))
    }

    /// What a click at `p` (client coords) picks: the target, or with Ctrl+clicked rects those and the
    /// hovered one together.
    fn click_pick(&self, p: (i32, i32)) -> Option<Pick> {
        if self.chosen.is_empty() {
            let owner = if self.whole { None } else { self.hovered(p).and_then(|r| self.owner(r)) };
            return self.target_at(p).map(|r| (r, None, vec![owner]));
        }
        let mut set = self.chosen.clone();
        set.extend(self.hovered(p).filter(|h| !self.chosen.contains(h)));
        self.multi(&set)
    }

    /// The pick for several rects (desktop coords): their bounding box, cut out for screenshots.
    fn multi(&self, set: &[Rect]) -> Option<Pick> {
        let (rect, cut) = multi_pick(set, self.area(), self.fit, matches!(self.purpose, Purpose::Shot { .. }))?;
        Some((rect, cut, set.iter().map(|&r| self.owner(r)).collect()))
    }

    /// The app behind the picked `windows`: there's one when every pick is a window of the same app.
    fn source_of(windows: &[Option<HWND>]) -> Option<Source> {
        let mut apps = windows.iter().map(|w| w.and_then(identify));
        let first = apps.next()??;
        apps.all(|a| a.is_some_and(|a| a.exe == first.exe)).then_some(first)
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
            el.request(self.to_desktop(p));
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
        if at != self.to_desktop(self.cursor) {
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

    /// The visible edges a selection snaps to (desktop coords): of windows, their client areas and
    /// the UI elements found so far where no window above covers them, and of monitors.
    fn snap_edges(&self) -> Edges {
        let frames: Vec<Rect> = self.windows.iter().map(|w| w.1).collect();
        let mut parts: Vec<(usize, Rect)> =
            self.clients.iter().enumerate().filter_map(|(i, c)| c.map(|c| (i, c))).collect();
        parts.extend(self.elements.iter().flat_map(Elements::known));
        let monitors: Vec<Rect> = self.outputs.iter().map(|o| o.rect).collect();
        snap::visible(&frames, &parts, &monitors)
    }

    fn equip(&self, ed: &mut Editor) {
        ed.set_snap(self.snap_edges().offset(-self.origin.0, -self.origin.1));
        let windows = self.windows.iter().zip(&self.clients).filter_map(|(w, c)| {
            window_pick(w.1, &self.outputs).map(|r| (self.to_client(r), c.map(|c| self.to_client(c))))
        });
        ed.set_windows(windows.collect());
        if let Some(arrow) = &self.arrow {
            ed.set_arrow(arrow.clone());
        }
    }

    /// A new editor starts with the cursor the screenshot kept, if it kept one.
    fn add_cursor(&self, ed: &mut Editor) {
        if let Some((img, at)) = &self.shot_cursor {
            ed.add_cursor(img.clone(), *at);
        }
    }

    fn element_at(&self, p: (i32, i32)) -> Option<Rect> {
        let d = self.to_desktop(p);
        let first = *self.chain.first().filter(|_| self.parts() && self.chain_at == d)?;
        (self.kind(first) == Kind::Element).then(|| self.to_client(first))
    }

    fn edit_element(&mut self, p: (i32, i32)) -> Option<Rect> {
        if !self.edit.as_ref().is_some_and(Editor::wants_elements) {
            return None;
        }
        self.track(p);
        self.element_at(p)
    }

    fn under(&self, (x, y): (i32, i32)) -> u32 {
        match &self.screen {
            Screen::Frozen(f) => f.bright.pixel(x, y).unwrap_or(0),
            Screen::Live(_) => 0,
        }
    }

    /// What picking shows: the screen with draw-first's marks drawn in, or the frozen screen.
    fn pick_frozen(&self) -> Option<&Frozen> {
        match (&self.flat, &self.screen) {
            (Some(flat), _) => Some(flat),
            (None, Screen::Frozen(f)) => Some(f),
            (None, Screen::Live(_)) => None,
        }
    }

    /// Forgets the pick in progress and everything it shows.
    fn reset_pick(&mut self) {
        self.set_gliding(false);
        self.glide = None;
        self.drag_start = None;
        self.slide = (0, 0);
        self.dragging = false;
        self.snap = None;
        self.whole = false;
        self.chosen.clear();
        self.lit.clear();
        self.target = None;
        self.dwell.set(None);
        self.sel = None;
        self.label = None;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), DWELL_TIMER);
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// Draw-first: follows the editor between its region tool (the picker picks, over the marks)
    /// and drawing.
    fn sync_mode(&mut self) {
        let region = self.edit.as_ref().is_some_and(Editor::picks_region);
        if region == self.region {
            return;
        }
        self.region = region;
        self.reset_pick();
        if region {
            self.flatten();
            self.shift = held(VK_SHIFT.0);
            self.track(self.cursor);
            self.update_selection(self.cursor);
        } else {
            self.flat = None;
        }
    }

    /// Draws the editor's marks into the screen picking shows.
    fn flatten(&mut self) {
        self.flat = None;
        if let (Some(ed), Screen::Frozen(f)) = (&self.edit, &self.screen)
            && ed.has_marks()
        {
            match ed.flattened(&f.bright, &self.painter).and_then(Frozen::new) {
                Ok(flat) => self.flat = Some(flat),
                Err(e) => log(&format!("can't show the marks: {e}")),
            }
        }
        if self.target.is_some() {
            self.label = None;
            self.show(self.sel);
        }
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
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

    /// The hint has done its job once a drag starts.
    fn drop_hints(&mut self) {
        let gone: Vec<Rect> = match &mut self.screen {
            Screen::Frozen(f) => f.hints.drain(..).map(|(on_dim, _)| on_dim.rect).collect(),
            Screen::Live(l) => {
                l.drop_hints();
                Vec::new()
            }
        };
        self.invalidate(&gone);
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

    /// The hover highlight follows the cursor at `cur`, except onto big targets: those wait until
    /// the cursor has rested on them. Drags update right away.
    fn update_hover(&mut self, cur: (i32, i32)) {
        if self.drag_start.is_some_and(|_| self.dragging) {
            return self.update_selection(cur);
        }
        let to = self.hover_target(cur);
        match self.dwell.hover(to, Instant::now()) {
            Some(left) => unsafe {
                SetTimer(Some(self.hwnd), DWELL_TIMER, left.as_millis() as u32 + 1, None);
            },
            None => self.update_selection(cur),
        }
    }

    /// Shows what the mouse at `cur` would pick: the drag selection while dragging, otherwise
    /// the window (or monitor) under the cursor, gliding there from the last one. A drag
    /// follows the cursor instantly, except for jumps (the drag starting, the box flipping
    /// across its start point), which glide.
    fn update_selection(&mut self, cur: (i32, i32)) {
        let drag = self.drag_start.filter(|_| self.dragging);
        let corner = self.corner(cur);
        let target = match drag {
            Some(start) => Some(self.fitted(start, corner)),
            None => {
                self.dwell.set(self.hover_target(cur));
                self.target_at(cur)
            }
        }
        .map(|r| self.to_client(r));
        if target == self.target {
            return; // unchanged, or already gliding there
        }
        self.target = target;
        let glide = match (drag, self.sel, target) {
            (Some(start), Some(from), Some(_)) => {
                let side = (corner.0 < start.0, corner.1 < start.1);
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
            Some((r, ..)) => (self.to_client(r), self.to_client(r)),
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
        let gap = self.painter.px(6.0) as i32;
        let bar = self.edit.as_ref().map(|e| e.bar);
        // In the editor the toolbar may sit above the shot; the label then goes inside.
        let bar_above = !self.region && bar.is_some_and(|b| b.bottom() <= near.y);
        let (x, y) = label_spot(near, mon, (lw, lh), gap, bar, bar_above);
        if same_text.is_some_and(|l| (l.rect.x, l.rect.y) == (x, y)) {
            return self.set_selection(Some(sel), None); // label unchanged
        }
        let on_bright = self.chosen.iter().map(|&c| self.to_client(c)).chain([sel]).any(|r| r.contains(x, y));
        let label = match self.pick_frozen() {
            Some(f) => render_pill(&self.painter, &self.font, &text, if on_bright { &f.bright } else { &f.dim }, x, y).ok(),
            None => Some(Pill { rect: Rect { x, y, w: lw, h: lh }, dib: None, text }),
        };
        self.set_selection(Some(sel), Some(label));
    }

    /// What a release at `end` picks (desktop coords): the drag, or what a click picks.
    fn finish_drag(&mut self, end: (i32, i32)) -> Option<Pick> {
        let start = self.drag_start.take()?;
        let corner = self.corner(end);
        self.slide = (0, 0);
        let pick = if std::mem::take(&mut self.dragging) {
            Some((self.fitted(start, corner), None, Vec::new()))
        } else {
            self.click_pick(end)
        };
        self.snap = None;
        pick
    }

    /// Recording: done. Screenshot: the region opens in the editor, `cutout` (desktop coords) the
    /// only parts of it kept; in draw-first mode it's the finished shot unless `edit` (Shift held).
    fn picked(&mut self, (rect, cutout, windows): Pick, edit: bool) -> Action {
        let source = State::source_of(&windows);
        match self.purpose {
            Purpose::Record { .. } => {
                let outputs = overlapped(rect, &self.outputs).cloned().collect();
                Action::Finish(Some(Picked::Region(Selection { rect, outputs, source })))
            }
            Purpose::Shot { .. } => {
                self.set_gliding(false);
                self.glide = None;
                self.source = source;
                let was_live = matches!(self.screen, Screen::Live(_));
                if let Err(e) = self.freeze() {
                    log(&format!("screenshot failed: {e}"));
                    return Action::Finish(None);
                }
                let sel = self.to_client(rect);
                let cutout = cutout.map(|c| c.into_iter().map(|r| self.to_client(r)).collect());
                if let Some(mut ed) = self.edit.take() {
                    self.equip(&mut ed);
                    self.edit = Some(ed);
                    let action = self.edit_event(|e| e.region_picked(sel, cutout, edit));
                    if let Action::Finish(r) = action {
                        return Action::Finish(r);
                    }
                    self.target = Some(sel);
                    self.show(Some(sel));
                    return Action::Release;
                }
                self.chosen.clear();
                if let Screen::Frozen(f) = &mut self.screen
                    && let Some(was) = f.hide_cursor()
                {
                    self.invalidate(&[was]);
                }
                let mut ed = Editor::new(sel, cutout, self.monitors(), self.painter.scale);
                self.equip(&mut ed);
                self.add_cursor(&mut ed);
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
        schedule_tick(self.hwnd, ed);
        match event {
            editor::Event::None => Action::Handled,
            editor::Event::Repaint => {
                let (damage, sel, changed) = (ed.damage(&before), ed.sel, ed.marks_changed(&before));
                let was = self.region;
                self.sync_mode();
                self.invalidate(&damage);
                if self.region && was && changed {
                    self.flatten();
                } else if !self.region && self.target.is_some() {
                    self.target = Some(sel);
                    self.show(Some(sel));
                }
                if self.last_paint.get().elapsed() >= self.frame { Action::Paint } else { Action::Handled }
            }
            editor::Event::Cancel => Action::Finish(None),
            editor::Event::Finish(output) => {
                let Screen::Frozen(f) = &self.screen else { return Action::Finish(None) };
                match ed.pixels(&f.bright, &self.painter) {
                    Ok((pixels, rect, alpha)) => {
                        let (w, h, at) = (rect.w as u32, rect.h as u32, rect.offset(self.origin.0, self.origin.1));
                        let source = self.source.clone();
                        Action::Finish(Some(Picked::Shot(Shot { pixels, w, h, alpha, at, output, source })))
                    }
                    Err(e) => {
                        log(&format!("screenshot failed: {e}"));
                        Action::Finish(None)
                    }
                }
            }
        }
    }

    /// Ctrl or Alt went down or up: the editor's drag moves or snaps accordingly.
    fn edit_mods(&mut self) -> Action {
        let (ctrl, alt, at) = (held(VK_CONTROL.0), held(VK_MENU.0), self.cursor);
        self.edit_event(|e| {
            e.set_mods(ctrl, alt);
            e.mouse_move(at)
        })
    }

    /// Input while the markup editor is open (and not picking).
    fn handle_edit(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Action {
        match msg {
            WM_LBUTTONDOWN => {
                let p = point(lp);
                let under = self.under(p);
                match self.edit_event(|e| e.mouse_down(p, under)) {
                    Action::Finish(r) => Action::Finish(r),
                    _ => Action::Capture,
                }
            }
            WM_MOUSEMOVE => {
                let p = point(lp);
                self.cursor = p;
                self.shift = held(VK_SHIFT.0);
                let element = self.edit_element(p);
                let (ctrl, alt) = (held(VK_CONTROL.0), held(VK_MENU.0));
                self.edit_event(|e| {
                    e.set_element(element);
                    e.set_mods(ctrl, alt);
                    e.mouse_move(p)
                })
            }
            WM_LBUTTONUP => {
                let p = point(lp);
                match self.edit_event(|e| e.mouse_up(p)) {
                    Action::Finish(r) => Action::Finish(r),
                    _ => Action::Release,
                }
            }
            // bit 30 of lparam: the key was already down (auto-repeat)
            WM_KEYDOWN
                if lp.0 & (1 << 30) != 0
                    && [VK_RETURN, VK_SPACE, VK_TAB].iter().any(|vk| wp.0 == vk.0 as usize) =>
            {
                Action::Handled
            }
            WM_KEYDOWN | WM_KEYUP if wp.0 == VK_SHIFT.0 as usize => {
                self.shift = msg == WM_KEYDOWN;
                let element = self.edit_element(self.cursor);
                self.edit_event(|e| {
                    e.set_element(element);
                    e.refresh_pick()
                })
            }
            WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP
                if wp.0 == VK_CONTROL.0 as usize || wp.0 == VK_MENU.0 as usize =>
            {
                self.edit_mods()
            }
            WM_KEYDOWN => {
                let (ctrl, shift) = (held(VK_CONTROL.0), held(VK_SHIFT.0));
                self.edit_event(|e| e.key(wp.0 as u32, ctrl, shift))
            }
            WM_KEYUP => self.edit_event(|e| e.key_up(wp.0 as u32)),
            WM_CHAR => self.edit_event(|e| e.char(wp.0 as u16)),
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as i32;
                self.edit_event(|e| e.wheel(delta))
            }
            WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
                let at = self.cursor_at();
                self.edit.as_ref().map_or(Action::Default, |e| Action::Cursor(e.cursor(at)))
            }
            WM_RBUTTONDOWN => {
                let p = point(lp);
                self.edit_event(|e| e.right_click(p))
            }
            WM_RBUTTONUP => Action::Handled,
            WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => Action::Finish(None),
            WM_CLOSE => Action::Finish(None),
            _ => Action::Default,
        }
    }

    /// The mouse position now (client coordinates).
    fn cursor_at(&self) -> (i32, i32) {
        let mut p = POINT::default();
        let _ = unsafe { GetCursorPos(&mut p) };
        (p.x - self.origin.0, p.y - self.origin.1)
    }

    /// Draw-first's region tool: whether `p` (client coords) is on the editor's toolbar.
    fn on_bar(&self, p: (i32, i32)) -> bool {
        self.region && self.edit.as_ref().is_some_and(|e| e.bar.contains(p.0, p.1))
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
        // Alt alone would enter menu mode, Alt+key would beep.
        if msg == WM_SYSCHAR || (msg == WM_SYSCOMMAND && (wp.0 & 0xFFF0) as u32 == SC_KEYMENU) {
            return Action::Handled;
        }
        if self.done {
            return match msg {
                WM_CLOSE | WM_KEYDOWN | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONUP | WM_MOUSEMOVE => Action::Handled,
                _ => Action::Default,
            };
        }
        if msg == WM_ELEMENTS {
            self.refresh_chain();
            if self.edit.is_some() && !self.region {
                let element = self.element_at(self.cursor);
                return self.edit_event(|e| {
                    e.set_element(element);
                    e.refresh_pick()
                });
            }
            if self.dragging {
                self.snap = Some(self.snap_edges());
            }
            self.update_hover(self.cursor);
            return Action::Handled;
        }
        if msg == WM_TIMER && wp.0 == EDITOR_TIMER {
            let _ = unsafe { KillTimer(Some(self.hwnd), EDITOR_TIMER) };
            return self.edit_event(|e| e.tick());
        }
        if msg == WM_TIMER && wp.0 == DWELL_TIMER {
            let _ = unsafe { KillTimer(Some(self.hwnd), DWELL_TIMER) };
            if self.edit.is_none() || self.region {
                self.update_hover(self.cursor);
            }
            return Action::Paint;
        }
        if self.edit.is_some() && !self.region {
            return self.handle_edit(msg, wp, lp);
        }
        self.handle_pick(msg, wp, lp)
    }

    /// Input while picking: select-first before the editor opens, or draw-first's region tool,
    /// where the editor keeps its toolbar, keys and right-clicks.
    fn handle_pick(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Action {
        let ctrl = || held(VK_CONTROL.0);
        let pressing_bar = self.region && self.edit.as_ref().is_some_and(Editor::dragging);
        match msg {
            WM_LBUTTONDOWN if self.on_bar(point(lp)) => {
                let p = point(lp);
                match self.edit_event(|e| e.mouse_down(p, 0)) {
                    Action::Finish(r) => Action::Finish(r),
                    _ => Action::Capture,
                }
            }
            WM_LBUTTONDOWN => {
                self.drag_start = Some(point(lp));
                self.slide = (0, 0);
                self.ctrl_at_press = ctrl();
                self.dragging = false;
                self.side = None;
                self.snap = Some(self.snap_edges());
                Action::Capture
            }
            WM_MOUSEMOVE => {
                let p = point(lp);
                let last = std::mem::replace(&mut self.cursor, p);
                self.shift = held(VK_SHIFT.0);
                let mut paint = false;
                if self.region && self.drag_start.is_none() {
                    paint = matches!(self.edit_event(|e| e.mouse_move(p)), Action::Paint);
                }
                if self.sliding() {
                    self.slide_drag(last, p);
                }
                self.track(p);
                let mut cleared = false;
                if let Some(s) = self.drag_start
                    && !self.dragging
                {
                    self.dragging = (p.0 - s.0).abs() >= CLICK_SLOP || (p.1 - s.1).abs() >= CLICK_SLOP;
                    self.whole &= !self.dragging;
                    if self.dragging {
                        self.drop_hints();
                    }
                    cleared = self.dragging && !self.chosen.is_empty();
                    if cleared {
                        self.chosen.clear();
                    }
                }
                self.update_hover(p);
                if cleared {
                    self.set_selection(self.sel, None);
                }
                if paint || self.last_paint.get().elapsed() >= self.frame { Action::Paint } else { Action::Handled }
            }
            WM_LBUTTONUP if pressing_bar => {
                let p = point(lp);
                match self.edit_event(|e| e.mouse_up(p)) {
                    Action::Finish(r) => Action::Finish(r),
                    _ => Action::Release,
                }
            }
            WM_LBUTTONUP if !self.dragging && self.drag_start.is_some() && ctrl() => {
                self.drag_start = None;
                self.snap = None;
                self.toggle_at(point(lp));
                Action::Release
            }
            WM_LBUTTONUP => match self.finish_drag(point(lp)) {
                Some(pick) => self.picked(pick, held(VK_SHIFT.0)),
                None => {
                    self.update_selection(point(lp));
                    Action::Release
                }
            },
            WM_RBUTTONUP => {
                if self.drag_start.take().is_some() {
                    self.dragging = false;
                    self.slide = (0, 0);
                    self.snap = None;
                    self.update_selection(point(lp));
                    Action::Release
                } else if self.region {
                    let p = point(lp);
                    self.edit_event(|e| e.right_click(p))
                } else {
                    Action::Finish(None)
                }
            }
            WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP if wp.0 == VK_SHIFT.0 as usize => {
                self.set_shift(matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN));
                Action::Handled
            }
            WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP
                if wp.0 == VK_CONTROL.0 as usize || wp.0 == VK_MENU.0 as usize =>
            {
                if matches!(msg, WM_KEYUP | WM_SYSKEYUP) && wp.0 == VK_CONTROL.0 as usize {
                    self.ctrl_at_press = false;
                }
                if self.dragging {
                    self.update_selection(self.cursor);
                }
                Action::Handled
            }
            WM_MOUSEWHEEL if self.drag_start.is_none() => {
                let up = ((wp.0 >> 16) & 0xFFFF) as i16 > 0;
                let here = self.chain_at == self.to_desktop(self.cursor);
                if here && self.parts() && !self.whole {
                    let last = self.chain.len().saturating_sub(1);
                    self.depth = if up { (self.depth + 1).min(last) } else { self.depth.saturating_sub(1) };
                    self.update_selection(self.cursor);
                }
                Action::Handled
            }
            WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize && self.region => {
                self.edit_event(|e| e.key(VK_ESCAPE.0 as u32, false, false))
            }
            WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => Action::Finish(None),
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
                if lp.0 & (1 << 30) != 0 {
                    return Action::Handled; // auto-repeat
                }
                let pick = if self.drag_start.is_none() && !self.chosen.is_empty() {
                    self.multi(&self.chosen)
                } else {
                    self.drag_start.get_or_insert(self.cursor);
                    self.finish_drag(self.cursor)
                };
                match pick {
                    Some(pick) => self.picked(pick, held(VK_SHIFT.0)),
                    None => Action::Handled,
                }
            }
            WM_KEYDOWN if self.region && self.drag_start.is_none() => {
                if lp.0 & (1 << 30) != 0 && wp.0 == VK_TAB.0 as usize {
                    return Action::Handled;
                }
                let shift = held(VK_SHIFT.0);
                self.edit_event(|e| e.key(wp.0 as u32, ctrl(), shift))
            }
            WM_KEYUP if self.region => self.edit_event(|e| e.key_up(wp.0 as u32)),
            WM_SETCURSOR if self.region && (lp.0 & 0xFFFF) as u32 == HTCLIENT && self.on_bar(self.cursor_at()) => {
                Action::Cursor(IDC_HAND)
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

fn schedule_tick(hwnd: HWND, ed: &Editor) {
    if let Some(left) = ed.next_tick() {
        unsafe { SetTimer(Some(hwnd), EDITOR_TIMER, left.as_millis() as u32 + 1, None) };
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
