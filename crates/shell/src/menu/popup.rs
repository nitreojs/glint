//! The menu's windows and its modal loop. Every open level is a layered popup drawn by Direct2D
//! in software (no GPU device to wait for); the first one has the focus (keys go there), submenus never activate. The loop
//! handles our windows' input itself and dispatches everything else, so the app's timers and
//! posted results keep running while the menu is open, as with `TrackPopupMenu`.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use glint_core::geom::rect::Rect;
use glint_core::log::log;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Direct2D::ID2D1DCRenderTarget;
use windows::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmFlush, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, w};

use super::draw::{self, Fonts};
use super::layout::{Layout, Metrics, Side, place_at, place_beside};
use super::nav::{Effect, Key, Nav, step};
use super::{Item, Kind, Menu};
use crate::gfx::{Dib, Painter, Weight, ease_out};

/// Seconds to fade and slide in, and to fade out.
const OPEN: f32 = 0.13;
const CLOSE: f32 = 0.09;
/// How far a panel slides in, in 96-DPI pixels.
const SLIDE: f32 = 8.0;
/// How long the mouse rests on a row before its submenu opens (or another one closes).
const SUB_DELAY: Duration = Duration::from_millis(180);
const STATUS_EVERY: Duration = Duration::from_millis(500);
/// The highlight's glide: its time constant and its fade-out, in seconds.
const GLIDE: f32 = 0.035;
const UNLIGHT: f32 = 0.12;
/// Posted to the menu: close without a pick. WPARAM 1: it lost the focus, 0: a click outside.
const WM_DISMISS: u32 = WM_APP + 0x2A0;
/// How long after opening the menu takes the focus back should it lose it, as when it opens
/// from the tray's overflow flyout, which takes the focus back after the click that opened us.
const TAKE_FOCUS: Duration = Duration::from_millis(600);

thread_local! {
    static OPEN_NOW: Cell<bool> = const { Cell::new(false) };
    static HOOK: RefCell<Hook> = RefCell::new(Hook::default());
}

/// Shows `menu` at the cursor for `owner` (the app's tray window) and waits until it's closed;
/// returns the command picked, if any. `keyboard`: opened from the keyboard, so the first item
/// starts highlighted. `status` is asked twice a second for a new header status (`None`: as it
/// is). A menu already open makes this return `None` at once.
pub fn track(owner: HWND, menu: &Menu, keyboard: bool, mut status: impl FnMut() -> Option<String>) -> Option<usize> {
    if OPEN_NOW.replace(true) {
        return None;
    }
    let picked = match Session::open(owner, menu, keyboard) {
        Ok(mut session) => session.run(&mut status),
        Err(e) => {
            log(&format!("can't open the menu: {e}"));
            None
        }
    };
    OPEN_NOW.set(false);
    unsafe {
        // the tray menu's foreground dance: makes sure the owner's next input is handled
        let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
    }
    picked
}

/// The mouse outside the menu: a press anywhere else closes it, eaten (with its release) so it
/// doesn't also land, e.g. as a click on the tray icon that just opened the menu.
#[derive(Default)]
struct Hook {
    hook: Option<HHOOK>,
    root: HWND,
    panels: Vec<Rect>,
    /// Closing already: presses pass.
    closing: bool,
    /// Presses eaten whose release is still to come: left, right, middle, x.
    eaten: [bool; 4],
}

impl Hook {
    fn mouse(&mut self, msg: u32, pt: POINT) -> bool {
        let (button, down) = match msg {
            WM_LBUTTONDOWN => (0, true),
            WM_LBUTTONUP => (0, false),
            WM_RBUTTONDOWN => (1, true),
            WM_RBUTTONUP => (1, false),
            WM_MBUTTONDOWN => (2, true),
            WM_MBUTTONUP => (2, false),
            WM_XBUTTONDOWN => (3, true),
            WM_XBUTTONUP => (3, false),
            _ => return false,
        };
        if !down {
            return std::mem::take(&mut self.eaten[button]);
        }
        if self.closing || self.panels.iter().any(|r| r.contains(pt.x, pt.y)) {
            return false;
        }
        self.eaten[button] = true;
        unsafe {
            let _ = PostMessageW(Some(self.root), WM_DISMISS, WPARAM(0), LPARAM(0));
        }
        true
    }
}

unsafe extern "system" fn mouse_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let pt = unsafe { (*(lp.0 as *const MSLLHOOKSTRUCT)).pt };
        let eat = HOOK.with(|h| h.try_borrow_mut().is_ok_and(|mut h| h.mouse(wp.0 as u32, pt)));
        if eat {
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

/// The highlight of one panel, gliding to the hot row.
#[derive(Clone, Copy)]
struct Glide {
    y: f32,
    a: f32,
}

struct Level {
    hwnd: HWND,
    /// What the window shows, premultiplied, the shadow round the panel included.
    dib: Dib,
    layout: Layout,
    widths: Vec<(f32, f32)>,
    /// The row of the level before this one it opened from (0 for the menu itself).
    from: usize,
    /// The panel on screen.
    panel: Rect,
    side: Side,
    shown: Instant,
    glide: Option<Glide>,
    /// TrackMouseEvent is waiting to tell us the mouse left.
    tracking: bool,
    dirty: bool,
}

impl Drop for Level {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

struct Session<'a> {
    owner: HWND,
    menu: &'a Menu,
    painter: Painter,
    /// Draws every level's picture (software, per-pixel alpha).
    target: ID2D1DCRenderTarget,
    fonts: Fonts,
    m: Metrics,
    work: Rect,
    cursor: (i32, i32),
    status: String,
    nav: Nav,
    levels: Vec<Level>,
    /// The submenu of `levels[.0]` follows its hot row at this time.
    pending: Option<(usize, Instant)>,
    /// Fading out since then, to return this.
    closing: Option<(Instant, Option<usize>)>,
    last_frame: Instant,
    title_w: f32,
    opened: Instant,
}

impl<'a> Session<'a> {
    fn open(owner: HWND, menu: &'a Menu, keyboard: bool) -> Result<Session<'a>, String> {
        let mut pt = POINT::default();
        let (work, dpi) = unsafe {
            let _ = GetCursorPos(&mut pt);
            let monitor = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(monitor, &mut info);
            let (mut dx, mut dy) = (96, 96);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            let r = info.rcWork;
            (Rect::from_points(r.left, r.top, r.right, r.bottom), dx)
        };
        let mut painter = Painter::new()?;
        painter.scale = dpi as f32 / 96.0;
        let fonts = Fonts {
            label: painter.plex(Weight::Regular, 13.0)?,
            hint: painter.plex(Weight::Regular, 12.0)?,
            title: painter.plex(Weight::SemiBold, 13.0)?,
        };
        let first = if keyboard { step(&menu.items, None, true) } else { None };
        let target = painter.alpha_target()?;
        let title_w = menu.header.as_ref().map_or(0.0, |h| painter.measure(&h.title, &fonts.title).0);
        let mut s = Session {
            owner,
            menu,
            m: Metrics::new(painter.scale),
            painter,
            target,
            fonts,
            work,
            cursor: (pt.x, pt.y),
            status: menu.header.as_ref().map(|h| h.status.clone()).unwrap_or_default(),
            nav: Nav::new(first),
            levels: Vec::new(),
            pending: None,
            closing: None,
            last_frame: Instant::now(),
            title_w,
            opened: Instant::now(),
        };
        s.push(0)?;
        let root = s.levels[0].hwnd;
        unsafe {
            let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), GetModuleHandleW(None).ok().map(Into::into), 0)
                .inspect_err(|e| log(&format!("menu: no mouse hook, clicks outside won't close it: {e}")))
                .ok();
            HOOK.with(|h| *h.borrow_mut() = Hook { hook, root, ..Hook::default() });
        }
        s.take_focus();
        s.sync_hook();
        Ok(s)
    }

    /// Makes the menu the foreground window, so keys reach it and losing it closes the menu.
    /// The shell lets the app whose icon was clicked take the foreground once; should the
    /// overflow flyout take it back, sharing the input state of the window in front lets us
    /// take it again.
    fn take_focus(&self) {
        let root = self.levels[0].hwnd;
        unsafe {
            if !SetForegroundWindow(root).as_bool() {
                let front = GetWindowThreadProcessId(GetForegroundWindow(), None);
                let me = GetCurrentThreadId();
                if front != 0 && front != me && AttachThreadInput(me, front, true).as_bool() {
                    let _ = SetForegroundWindow(root);
                    let _ = AttachThreadInput(me, front, false);
                }
            }
            let _ = SetFocus(Some(root));
        }
    }

    /// The items of the open level `k`.
    fn items(&self, k: usize) -> &'a [Item] {
        let mut items: &'a [Item] = &self.menu.items;
        for lv in &self.levels[1..=k.min(self.levels.len().saturating_sub(1))] {
            items = items.get(lv.from).and_then(Item::children).unwrap_or(&[]);
        }
        items
    }

    /// The header's title width and its whole width with the status.
    fn header_width(&self) -> Option<f32> {
        self.menu.header.as_ref()?;
        let status = self.painter.measure(&self.status, &self.fonts.hint).0;
        Some(self.title_w + 8.0 * self.m.s + status)
    }

    /// Opens the next level: the submenu of `from` in the deepest level, or the menu itself.
    fn push(&mut self, from: usize) -> Result<(), String> {
        let k = self.levels.len();
        let items = if k == 0 {
            &self.menu.items[..]
        } else {
            self.items(k - 1).get(from).and_then(Item::children).ok_or("not a submenu")?
        };
        let widths: Vec<(f32, f32)> = items
            .iter()
            .map(|i| {
                let label = if i.label.is_empty() { 0.0 } else { self.painter.measure(&i.label, &self.fonts.label).0 };
                let hint = if i.hint.is_empty() { 0.0 } else { self.painter.measure(&i.hint, &self.fonts.hint).0 };
                (label, hint)
            })
            .collect();
        let header = if k == 0 { self.header_width() } else { None };
        let layout = Layout::new(&self.m, items, &widths, header);
        let size = (layout.w as i32, layout.h as i32);
        let (panel, side) = match self.levels.last() {
            None => place_at(self.cursor, size, self.work),
            Some(parent) => {
                let row_top = parent.panel.y + parent.layout.rows.get(from).map_or(0.0, |r| r.y) as i32;
                let gaps = (self.m.overlap as i32, self.m.pad as i32);
                place_beside(parent.panel, row_top, size, self.work, gaps, parent.side == Side::Right)
            }
        };
        let sh = self.m.shadow as i32;
        let win = panel.inflate(sh);
        let dib = Dib::new(win.w, win.h)?;
        let hwnd = create(self.owner, win, k > 0)?;
        let now = Instant::now();
        self.levels.push(Level {
            hwnd,
            dib,
            layout,
            widths,
            from,
            panel,
            side,
            shown: now,
            glide: None,
            tracking: false,
            dirty: true,
        });
        self.render(k);
        self.present(k, now);
        unsafe {
            let _ = ShowWindow(hwnd, if k == 0 { SW_SHOW } else { SW_SHOWNA });
        }
        Ok(())
    }

    /// Makes the open levels match the navigation: closes what it left, opens what it entered.
    fn sync(&mut self) {
        let n = &self.nav.levels;
        let mut k = 1;
        while k < self.levels.len() && k < n.len() && n[k - 1] == Some(self.levels[k].from) {
            k += 1;
        }
        self.levels.truncate(k.min(self.levels.len()));
        while self.levels.len() < self.nav.levels.len() {
            let Some(from) = self.nav.levels[self.levels.len() - 1] else { break };
            if let Err(e) = self.push(from) {
                log(&format!("menu: can't open a submenu: {e}"));
                break;
            }
        }
        self.nav.levels.truncate(self.levels.len());
        for lv in &mut self.levels {
            lv.dirty = true;
        }
        self.sync_hook();
    }

    fn sync_hook(&self) {
        let panels = self.levels.iter().map(|l| l.panel).collect();
        HOOK.with(|h| h.borrow_mut().panels = panels);
    }

    /// The submenu of level `k` follows its hot row now (after the mouse rested there).
    fn follow(&mut self, k: usize) {
        if k >= self.nav.levels.len() {
            return;
        }
        let row = self.nav.levels[k];
        self.nav.levels.truncate(k + 1);
        if row.is_some_and(|r| self.items(k).get(r).and_then(Item::children).is_some()) {
            self.nav.levels.push(None); // the same submenu, already open, stays
        }
        self.sync();
    }

    /// Applies a pending submenu change at once, before the keyboard acts on what's open.
    fn settle(&mut self) {
        match self.pending.take() {
            Some((k, _)) => self.follow(k),
            None => self.sync(),
        }
    }

    fn apply(&mut self, effect: Effect) {
        match effect {
            Effect::Nothing => {}
            Effect::Moved => {
                if let Some(lv) = self.levels.last_mut() {
                    lv.dirty = true;
                }
            }
            Effect::Opened | Effect::Closed => self.sync(),
            Effect::Pick(id) => self.close(Some(id)),
            Effect::Dismiss => self.close(None),
        }
    }

    fn close(&mut self, picked: Option<usize>) {
        if self.closing.is_none() {
            self.closing = Some((Instant::now(), picked));
            HOOK.with(|h| h.borrow_mut().closing = true);
        }
    }

    /// The row under a point of level `k`'s window.
    fn hit(&self, k: usize, lp: LPARAM) -> Option<usize> {
        let (x, y) = ((lp.0 & 0xFFFF) as i16 as f32, ((lp.0 >> 16) & 0xFFFF) as i16 as f32);
        let sh = self.m.shadow;
        self.levels[k].layout.hit(x - sh, y - sh)
    }

    /// Points the navigation of the levels before `k` at the rows their open submenus came from:
    /// the mouse is in level `k`, so its parents are what's open.
    fn enter(&mut self, k: usize) {
        if self.pending.is_some_and(|(p, _)| p < k) {
            self.pending = None;
        }
        let n = &mut self.nav.levels;
        for i in 0..k {
            let from = Some(self.levels[i + 1].from);
            if n.get(i) != Some(&from) {
                n.truncate(i);
                n.push(from);
                self.levels[i].dirty = true;
            }
        }
        if n.len() == k {
            n.push(None);
        }
    }

    fn mouse_move(&mut self, k: usize, row: Option<usize>) {
        let lv = &mut self.levels[k];
        if !lv.tracking {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: lv.hwnd,
                dwHoverTime: 0,
            };
            lv.tracking = unsafe { TrackMouseEvent(&mut tme) }.is_ok();
        }
        self.enter(k);
        let child = self.levels.get(k + 1).map(|l| l.from);
        let items = self.items(k);
        let row = row.filter(|&r| items.get(r).is_some_and(Item::selectable));
        if child.is_some() && row == child {
            // back on the row whose submenu is open: it stays
            if self.nav.levels[k] != row {
                self.nav.levels[k] = row;
                self.levels[k].dirty = true;
            }
            if self.nav.levels.len() == k + 1 {
                self.nav.levels.push(None);
            }
            if self.pending.is_some_and(|(p, _)| p == k) {
                self.pending = None;
            }
            return;
        }
        if self.nav.levels[k] == row && self.nav.levels.len() == k + 1 {
            return;
        }
        self.nav.levels.truncate(k + 1);
        self.nav.levels[k] = row;
        self.levels[k].dirty = true;
        let sub = row.is_some_and(|r| items[r].children().is_some());
        self.pending = (sub || child.is_some()).then(|| (k, Instant::now() + SUB_DELAY));
    }

    fn mouse_leave(&mut self, k: usize) {
        self.levels[k].tracking = false;
        let child_open = self.levels.len() > k + 1
            && self.nav.levels.len() > k + 1
            && self.nav.levels[k] == Some(self.levels[k + 1].from);
        if child_open || k >= self.nav.levels.len() {
            return;
        }
        if self.nav.levels[k].is_some() {
            self.nav.levels[k] = None;
            self.levels[k].dirty = true;
        }
        if self.pending.is_some_and(|(p, _)| p == k) && self.levels.len() == k + 1 {
            self.pending = None;
        }
    }

    fn click(&mut self, k: usize, row: Option<usize>, release: bool) {
        let Some(row) = row else { return };
        self.enter(k);
        let item = &self.items(k)[row];
        if !item.selectable() {
            return;
        }
        match item.kind {
            Kind::Sub(_) => {
                self.pending = None;
                self.nav.levels.truncate(k + 1);
                self.nav.levels[k] = Some(row);
                let effect = self.nav_open(k);
                self.apply(effect);
            }
            Kind::Command(id) if release => self.close(Some(id)),
            _ => {}
        }
    }

    fn nav_open(&mut self, k: usize) -> Effect {
        let root = &self.menu.items;
        self.nav.open(root, k, false)
    }

    fn key(&mut self, key: Key) {
        self.settle();
        let effect = self.nav.key(&self.menu.items, key);
        self.apply(effect);
    }

    /// Handles a message for one of the menu's windows; false: not ours, dispatch it.
    fn handle(&mut self, msg: &MSG) -> bool {
        let Some(k) = self.levels.iter().position(|l| l.hwnd == msg.hwnd) else { return false };
        let input = matches!(msg.message, WM_MOUSEFIRST..=WM_MOUSELAST | WM_KEYFIRST..=WM_KEYLAST | WM_MOUSELEAVE);
        if self.closing.is_some() && input {
            return true;
        }
        match msg.message {
            WM_MOUSEMOVE => {
                let row = self.hit(k, msg.lParam);
                self.mouse_move(k, row);
            }
            WM_MOUSELEAVE => self.mouse_leave(k),
            WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
                let row = self.hit(k, msg.lParam);
                self.click(k, row, false);
            }
            WM_LBUTTONUP | WM_RBUTTONUP => {
                let row = self.hit(k, msg.lParam);
                self.click(k, row, true);
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let key = match VIRTUAL_KEY(msg.wParam.0 as u16) {
                    VK_UP => Some(Key::Up),
                    VK_DOWN => Some(Key::Down),
                    VK_HOME | VK_PRIOR => Some(Key::Home),
                    VK_END | VK_NEXT => Some(Key::End),
                    VK_LEFT => Some(Key::Left),
                    VK_RIGHT => Some(Key::Right),
                    VK_RETURN | VK_SPACE => Some(Key::Enter),
                    VK_ESCAPE => Some(Key::Esc),
                    VK_MENU | VK_F10 | VK_LWIN | VK_RWIN => {
                        self.close(None);
                        None
                    }
                    _ => {
                        unsafe {
                            let _ = TranslateMessage(msg);
                        }
                        None
                    }
                };
                if let Some(key) = key {
                    self.key(key);
                }
            }
            WM_CHAR | WM_SYSCHAR => {
                if let Some(c) = char::from_u32(msg.wParam.0 as u32).filter(|c| c.is_alphanumeric()) {
                    self.key(Key::Char(c));
                }
            }
            WM_DISMISS if msg.wParam.0 == 1 && (self.opened.elapsed() < TAKE_FOCUS || tray_in_front()) => {
                self.take_focus()
            }
            WM_DISMISS => self.close(None),
            _ => return input,
        }
        true
    }

    fn set_status(&mut self, status: String) {
        if status == self.status {
            return;
        }
        self.status = status;
        let Some(header) = self.header_width() else { return };
        let lv = &mut self.levels[0];
        lv.dirty = true;
        let grown = Layout::new(&self.m, &self.menu.items, &lv.widths, Some(header));
        if grown.w <= lv.layout.w {
            return; // it never narrows: a ticking clock doesn't make the menu wobble
        }
        let size = (grown.w as i32, grown.h as i32);
        let (panel, _) = place_at(self.cursor, size, self.work);
        let win = panel.inflate(self.m.shadow as i32);
        match Dib::new(win.w, win.h) {
            Ok(dib) => lv.dib = dib,
            Err(e) => return log(&format!("menu: {e}")),
        }
        lv.layout = grown;
        lv.panel = panel;
        self.sync_hook();
    }

    /// Advances the highlights and the fades, redraws what changed; true while something moves.
    fn frame(&mut self, now: Instant) -> bool {
        let dt = now.duration_since(self.last_frame).as_secs_f32().min(0.05);
        self.last_frame = now;
        let mut moving = self.closing.is_some();
        for k in 0..self.levels.len() {
            let hot = self.hot(k).and_then(|r| self.levels[k].layout.rows.get(r)).map(|r| r.y);
            let lv = &mut self.levels[k];
            let opening = now.duration_since(lv.shown).as_secs_f32() < OPEN;
            let glide = glide(lv.glide, hot, dt);
            let gliding = glide.is_some_and(|g| hot.is_none_or(|y| (g.y - y).abs() > 0.01 || g.a < 1.0));
            if gliding || lv.glide.is_some() != glide.is_some() {
                lv.dirty = true;
            }
            lv.glide = glide;
            moving |= opening || gliding;
            let dirty = lv.dirty;
            if dirty {
                self.render(k);
            }
            if dirty || opening || self.closing.is_some() {
                self.present(k, now);
            }
        }
        moving
    }

    /// The row highlighted at level `k`, while it's in step with what's open.
    fn hot(&self, k: usize) -> Option<usize> {
        let n = &self.nav.levels;
        let in_step = k < n.len() && (k == 0 || n[k - 1] == Some(self.levels[k].from));
        if in_step { n[k] } else { None }
    }

    /// Redraws level `k`'s picture.
    fn render(&mut self, k: usize) {
        let hot = self.hot(k);
        let lv = &self.levels[k];
        let sh = self.m.shadow;
        let header = self.menu.header.as_ref().filter(|_| k == 0);
        let frame = draw::Frame {
            m: &self.m,
            layout: &lv.layout,
            items: self.items(k),
            header: header.map(|h| (h.title.as_str(), self.status.as_str(), self.title_w)),
            widths: &lv.widths,
            accent: self.menu.accent,
            highlight: lv.glide.map(|g| (g.y, g.a)),
            hot,
            at: (sh, sh),
        };
        let result = self.painter.draw_alpha(&self.target, &lv.dib, |c| draw::frame(c, &frame, &self.fonts));
        if let Err(e) = result {
            log(&format!("menu: {e}"));
        }
        self.levels[k].dirty = false;
    }

    /// Puts level `k`'s picture on screen where it is in its slide, as opaque as it is in its fade.
    fn present(&self, k: usize, now: Instant) {
        let fade = match self.closing {
            Some((since, _)) => 1.0 - (now.duration_since(since).as_secs_f32() / CLOSE).min(1.0),
            None => 1.0,
        };
        let lv = &self.levels[k];
        let t = ease_out((now.duration_since(lv.shown).as_secs_f32() / OPEN).min(1.0));
        let slide = (SLIDE * self.m.s * if k == 0 { 1.0 } else { 0.5 } * (1.0 - t)).round() as i32;
        let (dx, dy) = match lv.side {
            Side::Above => (0, -slide),
            Side::Below => (0, slide),
            Side::Left => (-slide, 0),
            Side::Right => (slide, 0),
        };
        let win = lv.panel.inflate(self.m.shadow as i32);
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: (fade * t * 255.0).round().clamp(1.0, 255.0) as u8,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let at = POINT { x: win.x + dx, y: win.y + dy };
        let size = SIZE { cx: lv.dib.w, cy: lv.dib.h };
        let from = POINT::default();
        let shown = unsafe {
            UpdateLayeredWindow(lv.hwnd, None, Some(&at), Some(&size), Some(lv.dib.dc), Some(&from), COLORREF(0), Some(&blend), ULW_ALPHA)
        };
        if let Err(e) = shown {
            log(&format!("menu: UpdateLayeredWindow: {e}"));
        }
    }

    fn run(&mut self, status: &mut dyn FnMut() -> Option<String>) -> Option<usize> {
        let mut asked = Instant::now();
        let mut quit = None;
        let picked = 'outer: loop {
            let mut msg = MSG::default();
            while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
                if msg.message == WM_QUIT {
                    quit = Some(msg.wParam.0 as i32);
                    break 'outer None;
                }
                if !self.handle(&msg) {
                    unsafe {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
            let now = Instant::now();
            if let Some((since, picked)) = self.closing
                && now.duration_since(since).as_secs_f32() >= CLOSE
            {
                break picked;
            }
            if let Some((k, at)) = self.pending
                && now >= at
            {
                self.pending = None;
                self.follow(k);
            }
            if self.menu.header.is_some() && now.duration_since(asked) >= STATUS_EVERY {
                asked = now;
                if let Some(s) = status() {
                    self.set_status(s);
                }
            }
            if self.frame(now) {
                if unsafe { DwmFlush() }.is_err() {
                    std::thread::sleep(Duration::from_millis(7));
                }
                continue;
            }
            let mut wait = STATUS_EVERY.saturating_sub(now.duration_since(asked));
            if let Some((_, at)) = self.pending {
                wait = wait.min(at.saturating_duration_since(now));
            }
            if self.closing.is_none() && now.duration_since(self.opened) < TAKE_FOCUS {
                if unsafe { GetForegroundWindow() } != self.levels[0].hwnd {
                    self.take_focus();
                }
                wait = wait.min(Duration::from_millis(30));
            }
            unsafe {
                MsgWaitForMultipleObjectsEx(None, wait.as_millis() as u32 + 1, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            }
        };
        self.levels.clear();
        unsafe {
            // the screen without the menu is up before a pick runs (a screenshot)
            let _ = DwmFlush();
        }
        self.unhook(quit.is_none());
        if let Some(code) = quit {
            unsafe { PostQuitMessage(code) };
        }
        picked
    }

    /// Removes the mouse hook once the presses it ate are released (or a moment has passed).
    fn unhook(&self, wait: bool) {
        let start = Instant::now();
        while wait && HOOK.with(|h| h.borrow().eaten.iter().any(|&e| e)) && start.elapsed() < Duration::from_millis(800) {
            unsafe {
                MsgWaitForMultipleObjectsEx(None, 20, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        PostQuitMessage(msg.wParam.0 as i32);
                        break;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        let hook = HOOK.with(|h| std::mem::take(&mut *h.borrow_mut())).hook;
        if let Some(hook) = hook {
            unsafe {
                let _ = UnhookWindowsHookEx(hook);
            }
        }
    }
}

/// The highlight a frame later: gliding toward the row at `target`, fading out without one.
fn glide(now: Option<Glide>, target: Option<f32>, dt: f32) -> Option<Glide> {
    match (now, target) {
        (None, None) => None,
        (Some(g), None) => {
            let a = g.a - dt / UNLIGHT;
            (a > 0.0).then_some(Glide { a, ..g })
        }
        (now, Some(y)) => {
            // from nothing (or nearly faded), it appears in place rather than flying in
            let g = now.filter(|g| g.a > 0.3).unwrap_or(Glide { y, a: now.map_or(0.0, |g| g.a) });
            let k = 1.0 - (-dt / GLIDE).exp();
            let mut next = Glide { y: g.y + (y - g.y) * k, a: (g.a + dt / (UNLIGHT * 0.6)).min(1.0) };
            if (next.y - y).abs() < 0.3 {
                next.y = y;
            }
            Some(next)
        }
    }
}

/// The taskbar or the tray's overflow flyout has the focus: the shell shuffling it after the
/// click that opened the menu, not the user going elsewhere (a click there is the hook's).
fn tray_in_front() -> bool {
    const SHELL: [&str; 4] =
        ["Shell_TrayWnd", "Shell_SecondaryTrayWnd", "NotifyIconOverflowWindow", "TopLevelWindowForOverflowXamlIsland"];
    let mut class = [0u16; 64];
    let n = unsafe { GetClassNameW(GetForegroundWindow(), &mut class) } as usize;
    let class = String::from_utf16_lossy(&class[..n]);
    SHELL.contains(&class.as_str())
}

fn create(owner: HWND, r: Rect, sub: bool) -> Result<HWND, String> {
    unsafe {
        let hinst = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = w!("glint_menu");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc); // fails harmlessly when already registered
        let mut ex = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW;
        if sub {
            ex |= WS_EX_NOACTIVATE;
        }
        let hwnd = CreateWindowExW(ex, class, w!("menu"), WS_POPUP, r.x, r.y, r.w, r.h, Some(owner), None, Some(hinst.into()), None)
            .map_err(|e| format!("CreateWindowEx: {e}"))?;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, sub as isize);
        let on = BOOL(1);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, &on as *const BOOL as _, 4);
        Ok(hwnd)
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let dismiss = |focus: bool| unsafe {
        let _ = PostMessageW(Some(hwnd), WM_DISMISS, WPARAM(focus as usize), LPARAM(0));
    };
    match msg {
        WM_MOUSEACTIVATE if unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } != 0 => LRESULT(MA_NOACTIVATE as isize),
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            dismiss(true);
            LRESULT(0)
        }
        WM_ACTIVATEAPP if wp.0 == 0 => {
            dismiss(true);
            LRESULT(0)
        }
        WM_CLOSE | WM_CANCELMODE | WM_DPICHANGED | WM_DISPLAYCHANGE => {
            dismiss(false);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_highlight_appears_in_place_glides_between_rows_and_fades() {
        let g = glide(None, Some(40.0), 0.016).unwrap();
        assert_eq!(g.y, 40.0, "from nothing it appears where it's needed");
        let mut g = Glide { y: 40.0, a: 1.0 };
        let mut steps = 0;
        while g.y != 70.0 {
            g = glide(Some(g), Some(70.0), 0.008).unwrap();
            assert!((40.0..=70.0).contains(&g.y));
            steps += 1;
        }
        assert!((5..40).contains(&steps), "a quick glide: {steps} frames at 120 Hz");
        let mut g = Some(g);
        for _ in 0..20 {
            g = glide(g, None, 0.008);
        }
        assert!(g.is_none(), "it fades out without a row");
    }

    #[test]
    fn the_hook_eats_a_press_outside_and_its_release_only() {
        let mut h = Hook { panels: vec![Rect { x: 0, y: 0, w: 100, h: 100 }], ..Hook::default() };
        assert!(!h.mouse(WM_LBUTTONDOWN, POINT { x: 50, y: 50 }), "inside: the menu's own");
        assert!(!h.mouse(WM_LBUTTONUP, POINT { x: 50, y: 50 }));
        assert!(h.mouse(WM_RBUTTONDOWN, POINT { x: 150, y: 50 }));
        assert!(!h.mouse(WM_MOUSEMOVE, POINT { x: 150, y: 50 }));
        assert!(h.mouse(WM_RBUTTONUP, POINT { x: 150, y: 50 }));
        assert!(!h.mouse(WM_RBUTTONUP, POINT { x: 150, y: 50 }), "a release not ours passes");
        h.closing = true;
        assert!(!h.mouse(WM_LBUTTONDOWN, POINT { x: 150, y: 50 }), "closing: presses pass");
    }
}
