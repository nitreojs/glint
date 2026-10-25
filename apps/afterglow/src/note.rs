//! The note over everything when a clip is saved, like nvidia's instant replay: a pill in a
//! corner (or at the top or bottom middle) of the recorded monitor that slides in from its edge,
//! says what happened and slides back out. A new note at the same place morphs the one that's up
//! (its width eases to the new text, the new icon draws itself in); one at another place sends it
//! out as it comes in. Clicks go through it, it never takes focus, and it's kept out of capture,
//! so the buffer never has it; glint's screenshots let it in ([`set_capturable`]).
//!
//! It's a DWM glass window (per-pixel alpha from Direct2D) that's also layered, for the fade and
//! the click-through: capture exclusion refuses windows drawn with `UpdateLayeredWindow`. The
//! window stays put with room for the slide and longer text; each frame is redrawn inside it, once
//! per composition while something moves (see [`pace`]).
//!
//! Behind the text is frosted glass: what's on screen under the window, blurred and tinted dark so
//! the text reads on any frame. DWM's own blur (the accent policy) can't be had in a pill's shape
//! on Windows 10, as it ignores window regions, so the note copies the screen under itself (being
//! kept out of capture, it isn't in the copy), blurs it and fills the pill with it: once when it
//! comes up and again when it changes, never per frame.

use std::cell::RefCell;
use std::f32::consts::TAU;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::thread::Thread;
use std::time::{Duration, Instant};

use glint_core::geom::rect::Rect;
use glint_core::log::log;
use crate::app::AMBER;
use crate::config::Place;
use glint_shell::gfx::{Canvas, Dib, Painter, ease_out, make_glass, rgba};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_PROPERTIES, D2D1_ROUNDED_RECT, ID2D1Bitmap, ID2D1HwndRenderTarget,
};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CAPTUREBLT, GetDC, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, ReleaseDC, SRCCOPY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

/// Wakes a still note when it's time to leave.
const TIMER: usize = 1;
/// Posted by the pacer once per composition while a note moves.
const WM_FRAME: u32 = WM_APP + 1;
/// Seconds to slide in (easing out), to morph into new text, and to slide back out (easing in).
const ENTER: f32 = 0.32;
const MORPH: f32 = 0.3;
const EXIT: f32 = 0.24;
/// Seconds an icon takes to draw itself in, and the saving spinner's turns per second.
const ICON_IN: f32 = 0.45;
const SPIN: f32 = 1.2;
/// How far (96-DPI px) the pill travels from its edge, and how small it is out there.
const SLIDE: f32 = 28.0;
const SHRINK: f32 = 0.92;
/// Room (96-DPI px) beside the pill for longer text to grow into without resizing the window.
const ROOM: f32 = 160.0;
/// How far (96-DPI px) the old text rolls up, and the new text rolls in from, in a morph.
const ROLL: f32 = 6.0;
/// How long "saved" stays; "saving" stays until it's replaced (or this long, should a save hang).
const SHOWN: Duration = Duration::from_millis(2600);
const SAVING: Duration = Duration::from_secs(20);
/// The blur's reach (96-DPI px): each of its three box passes averages this far either way.
const BLUR: f32 = 8.0;

/// The tint over the blur, and how opaque it is; without a blur it's nearly opaque, so the text
/// still reads over a bright frame.
const TINT: [u8; 3] = [16, 16, 20];
const TINT_BLURRED: f32 = 0.7;
const TINT_PLAIN: f32 = 0.92;
const YELLOW: [u8; 3] = [0xFB, 0xBF, 0x24];
const RED: [u8; 3] = [0xE5, 0x48, 0x4D];

/// What a note tells, which picks its icon.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A save is being written: a spinning arc.
    Saving,
    /// A check in an amber disc.
    Saved,
    /// Something to know (nothing to save yet, a clip cut short): an exclamation triangle.
    Warning,
    /// Something went wrong: a cross in a red disc.
    Failed,
    /// Just showing where notes go: a small dot.
    Info,
}

/// What a pill says and the icon it says it with.
#[derive(Clone)]
struct Look {
    title: String,
    detail: String,
    kind: Kind,
    /// The pill's width for this text, in physical pixels.
    w: f32,
}

impl Look {
    fn same(&self, other: &Look) -> bool {
        (&self.title, &self.detail, self.kind) == (&other.title, &other.detail, other.kind)
    }
}

/// The pill's measures, in physical pixels.
#[derive(Clone, Copy)]
struct Dims {
    h: f32,
    pad: f32,
    icon: f32,
    gap: f32,
    slide: f32,
    room: f32,
    roll: f32,
    blur: usize,
    margin: i32,
    s: f32,
}

impl Dims {
    fn of(p: &Painter) -> Dims {
        Dims {
            h: p.px(52.0),
            pad: p.px(16.0),
            icon: p.px(18.0),
            gap: p.px(10.0),
            slide: p.px(SLIDE),
            room: p.px(ROOM),
            roll: p.px(ROLL),
            blur: p.px(BLUR) as usize,
            margin: p.px(24.0) as i32,
            s: p.scale,
        }
    }
}

struct Note {
    hwnd: HWND,
    painter: Painter,
    rt: ID2D1HwndRenderTarget,
    big: IDWriteTextFormat,
    small: IDWriteTextFormat,
    dims: Dims,
    place: Place,
    /// The work area it's placed in, and the window (desktop pixels) it's drawn in.
    area: Rect,
    win: Rect,
    /// The screen behind the window, blurred; whether the note is kept out of capture, which it
    /// must be to look again (or it would see itself).
    behind: Option<ID2D1Bitmap>,
    excluded: bool,
    /// When it came up (or would have, for a note coming back as it left), and when it leaves,
    /// in seconds after that.
    since: Instant,
    hold: f32,
    /// The look it's morphing from, and since when.
    from: Option<(Look, Instant)>,
    /// The look it shows, and since when (its icon draws itself in from then).
    look: Look,
    look_at: Instant,
}

/// What a note did on a frame.
enum Life {
    Moving,
    /// Holding still; seconds until it leaves.
    Still(f32),
    Gone,
}

thread_local! {
    /// The notes up: the last is the current one, any before it are on their way out.
    static NOTES: RefCell<Vec<Note>> = const { RefCell::new(Vec::new()) };
}

/// Lets screen capture see the notes that are up (`on`), or keeps them out again; whether one is up.
pub fn set_capturable(on: bool) -> bool {
    NOTES.with(|n| {
        let mut n = n.borrow_mut();
        for note in n.iter_mut() {
            note.exclude(!on);
        }
        !n.is_empty()
    })
}

/// Shows `title` and `detail` at `place` on `monitor` (desktop coordinates). A note already up
/// at the same place turns into this one; one elsewhere leaves as this one comes in.
pub fn show(monitor: Rect, place: Place, kind: Kind, title: &str, detail: &str) {
    let area = work_area(monitor);
    let hold = if kind == Kind::Saving { SAVING } else { SHOWN }.as_secs_f32();
    let now = Instant::now();
    let morphed = NOTES.with(|n| match n.borrow_mut().last_mut() {
        Some(note) if note.place == place && note.area == area => {
            let look = look(&note.painter, (&note.big, &note.small), note.dims, kind, title, detail);
            note.morph(look, hold, now);
            true
        }
        Some(note) => {
            note.leave(now);
            false
        }
        None => false,
    });
    if !morphed {
        match open(area, place, kind, title, detail, hold) {
            Ok(note) => NOTES.with(|n| n.borrow_mut().push(note)),
            Err(e) => log(&format!("can't show the note: {e}")),
        }
    }
    tick();
}

/// Sends the note that's up on its way out, if there is one.
pub fn hide() {
    let now = Instant::now();
    NOTES.with(|n| {
        if let Some(note) = n.borrow_mut().last_mut() {
            note.leave(now);
        }
    });
    tick();
}

fn look(p: &Painter, (big, small): (&IDWriteTextFormat, &IDWriteTextFormat), d: Dims, kind: Kind, title: &str, detail: &str) -> Look {
    let (tw, _) = p.measure(title, big);
    let (dw, _) = p.measure(detail, small);
    Look { title: title.to_owned(), detail: detail.to_owned(), kind, w: (d.pad * 2.0 + d.icon + d.gap + tw.max(dw)).ceil() }
}

fn open(area: Rect, place: Place, kind: Kind, title: &str, detail: &str, hold: f32) -> Result<Note, String> {
    let painter = Painter::new()?;
    let (big, small) = (painter.font(14.0)?, painter.font(11.5)?);
    let dims = Dims::of(&painter);
    let look = look(&painter, (&big, &small), dims, kind, title, detail);
    let win = window_rect(area, place, look.w, dims);
    let hwnd = window(win)?;
    make_glass(hwnd);
    let rt = match painter.window_target(hwnd, win.w, win.h) {
        Ok(rt) => rt,
        Err(e) => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return Err(e);
        }
    };
    unsafe {
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA);
    }
    let now = Instant::now();
    let mut note = Note {
        hwnd,
        painter,
        rt,
        big,
        small,
        dims,
        place,
        area,
        win,
        behind: None,
        excluded: false,
        since: now,
        hold,
        from: None,
        look,
        look_at: now,
    };
    // the first look behind comes before the window shows, so it's the screen even if exclusion fails
    note.look_behind();
    note.exclude(true); // only takes once the layered window has its transparency set
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    Ok(note)
}

/// `monitor` without the taskbar, so notes at the bottom clear it.
fn work_area(monitor: Rect) -> Rect {
    unsafe {
        let centre = POINT { x: monitor.x + monitor.w / 2, y: monitor.y + monitor.h / 2 };
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(MonitorFromPoint(centre, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() {
            return monitor;
        }
        let r = info.rcWork;
        Rect::from_points(r.left, r.top, r.right, r.bottom)
    }
}

fn top(place: Place) -> bool {
    matches!(place, Place::TopLeft | Place::Top | Place::TopRight)
}

/// Where across its window the pill sits: 0 at the left, 1 at the right.
fn align(place: Place) -> f32 {
    match place {
        Place::TopLeft | Place::BottomLeft => 0.0,
        Place::Top | Place::Bottom => 0.5,
        Place::TopRight | Place::BottomRight => 1.0,
    }
}

/// The window a pill `w` wide is drawn in at `place`: with room beside it for longer text, and
/// toward its edge for the slide.
fn window_rect(area: Rect, place: Place, w: f32, d: Dims) -> Rect {
    let slide = d.slide as i32;
    let r = place.at(area, ((w + d.room).ceil() as i32, d.h as i32), d.margin);
    let y = if top(place) { r.y - slide } else { r.y };
    Rect { y, h: r.h + slide, ..r }
}

/// Where a note is in its life: how far it's pulled back toward its edge (0 in place, 1 a whole
/// slide away), how big it is and how opaque.
#[derive(Debug, PartialEq)]
struct Pose {
    lift: f32,
    scale: f32,
    alpha: f32,
}

/// A note's pose `t` seconds after it came up, if it's still up; it starts leaving at `hold`.
fn pose(t: f32, hold: f32) -> Option<Pose> {
    if t >= hold + EXIT {
        None
    } else if t >= hold {
        let q = (t - hold) / EXIT;
        let e = q * q;
        Some(Pose { lift: e, scale: 1.0 - (1.0 - SHRINK) / 2.0 * e, alpha: 1.0 - q })
    } else {
        let e = ease_out((t / ENTER).clamp(0.0, 1.0));
        Some(Pose { lift: 1.0 - e, scale: SHRINK + (1.0 - SHRINK) * e, alpha: e })
    }
}

/// The pill's top in its window, `lift` of the way toward its edge; the window has `slide`
/// pixels of room on that side.
fn pill_y(top: bool, lift: f32, slide: f32) -> f32 {
    if top { slide * (1.0 - lift) } else { slide * lift }
}

/// How far into a fresh entrance a note is that's `t` seconds in and leaves at `hold`: one on its
/// way out comes back from about where it is.
fn rejoin(t: f32, hold: f32) -> f32 {
    if t < hold { t } else { ENTER * (1.0 - ((t - hold) / EXIT).min(1.0)) }
}

/// When a note `t` seconds in should have started leaving to be on its way out from about where
/// it is now.
fn leave_at(t: f32, hold: f32) -> f32 {
    if t >= hold { hold } else { t - EXIT * (1.0 - (t / ENTER).min(1.0)) }
}

/// How far something taking `length` seconds has come `t` seconds in, 0 to 1.
fn progress(t: f32, length: f32) -> f32 {
    (t / length).clamp(0.0, 1.0)
}

fn lerp(a: f32, b: f32, k: f32) -> f32 {
    a + (b - a) * k
}

fn since(now: Instant, then: Instant) -> f32 {
    now.saturating_duration_since(then).as_secs_f32()
}

/// Box-blurs each row of `src` (`w` × `h` 0x__RRGGBB pixels) `r` either way, edges clamped, and
/// writes the result transposed (`h` × `w`) into `dst`, opaque; twice makes a 2D box blur.
fn blur_rows(src: &[u32], dst: &mut [u32], (w, h): (usize, usize), r: usize) {
    let n = (2 * r + 1) as u32;
    let channels = |p: u32| [p & 0xFF, (p >> 8) & 0xFF, (p >> 16) & 0xFF];
    for (y, row) in src.chunks_exact(w).take(h).enumerate() {
        let at = |i: isize| row[i.clamp(0, w as isize - 1) as usize];
        let mut sum = [0u32; 3];
        for i in -(r as isize)..=r as isize {
            let c = channels(at(i));
            (0..3).for_each(|k| sum[k] += c[k]);
        }
        for x in 0..w {
            dst[x * h + y] = 0xFF00_0000 | ((sum[2] / n) << 16) | ((sum[1] / n) << 8) | (sum[0] / n);
            let (gone, next) = (channels(at(x as isize - r as isize)), channels(at(x as isize + r as isize + 1)));
            (0..3).for_each(|k| sum[k] = sum[k] + next[k] - gone[k]);
        }
    }
}

/// Blurs `px` (`w` × `h`) with three box passes `r` either way, close to a gaussian; it comes
/// out opaque.
fn blur(px: &mut [u32], (w, h): (usize, usize), r: usize) {
    let mut turned = vec![0; px.len()];
    for _ in 0..3 {
        blur_rows(px, &mut turned, (w, h), r);
        blur_rows(&turned, px, (h, w), r);
    }
}

impl Note {
    /// The pill's width now: eased toward the new text while it morphs.
    fn width(&self, now: Instant) -> f32 {
        match &self.from {
            Some((from, at)) => lerp(from.w, self.look.w, ease_out(progress(since(now, *at), MORPH))),
            None => self.look.w,
        }
    }

    /// Keeps the note out of capture (`on`) or lets it in.
    fn exclude(&mut self, on: bool) {
        let affinity = if on { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
        match unsafe { SetWindowDisplayAffinity(self.hwnd, affinity) } {
            Ok(()) => self.excluded = on,
            Err(e) => {
                self.excluded = false;
                log(&format!("note: SetWindowDisplayAffinity failed: {e}"));
            }
        }
    }

    /// Copies and blurs the screen behind the window, a blur's reach wider all round so the
    /// edges blur like the rest. Done when the note comes up and when it changes, not per frame.
    fn look_behind(&mut self) {
        let (r, win) = (self.dims.blur, self.win);
        let (w, h) = (win.w as usize + 2 * r, win.h as usize + 2 * r);
        let Ok(mut dib) = Dib::new(w as i32, h as i32).inspect_err(|e| log(&format!("note: {e}"))) else { return };
        unsafe {
            let screen = GetDC(None);
            let copied = BitBlt(dib.dc, 0, 0, w as i32, h as i32, Some(screen), win.x - r as i32, win.y - r as i32, SRCCOPY | CAPTUREBLT);
            ReleaseDC(None, screen);
            if copied.is_err() {
                return;
            }
        }
        blur(dib.pixels_mut(), (w, h), r);
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        let size = D2D_SIZE_U { width: win.w as u32, height: win.h as u32 };
        let inner = dib.pixels()[r * w + r..].as_ptr().cast();
        self.behind = unsafe { self.rt.CreateBitmap(size, Some(inner), (w * 4) as u32, &props) }.ok();
    }

    fn leave(&mut self, now: Instant) {
        self.hold = leave_at(since(now, self.since), self.hold);
    }

    /// Turns into `look` and stays `hold` seconds more, coming back first if it was leaving. The
    /// screen behind is looked at again, as a save can take a while.
    fn morph(&mut self, look: Look, hold: f32, now: Instant) {
        let t = rejoin(since(now, self.since), self.hold);
        self.since = now - Duration::from_secs_f32(t);
        self.hold = t + hold;
        if look.same(&self.look) {
            return;
        }
        let from = Look { w: self.width(now), ..std::mem::replace(&mut self.look, look) };
        self.from = Some((from, now));
        self.look_at = now;
        self.fit();
        if self.excluded {
            self.look_behind();
        }
    }

    /// Widens the window when the pill outgrows it.
    fn fit(&mut self) {
        if self.look.w <= self.win.w as f32 {
            return;
        }
        let win = window_rect(self.area, self.place, self.look.w, self.dims);
        unsafe {
            let flags = SWP_NOACTIVATE | SWP_NOZORDER;
            if let Err(e) = SetWindowPos(self.hwnd, None, win.x, win.y, win.w, win.h, flags) {
                log(&format!("note: SetWindowPos failed: {e}"));
                return;
            }
            if let Err(e) = self.rt.Resize(&D2D_SIZE_U { width: win.w as u32, height: win.h as u32 }) {
                log(&format!("note: Resize failed: {e}"));
            }
        }
        self.win = win;
    }

    /// Draws the frame for `now`.
    fn paint(&mut self, now: Instant) -> Life {
        let t = since(now, self.since);
        let Some(pose) = pose(t, self.hold) else { return Life::Gone };
        let u = self.from.as_ref().map_or(1.0, |(_, at)| progress(since(now, *at), MORPH));
        if u >= 1.0 {
            self.from = None;
        }
        let appear = progress(since(now, self.look_at), ICON_IN);
        let d = self.dims;
        let w = self.width(now);
        let x = align(self.place) * (self.win.w as f32 - w);
        let y = pill_y(top(self.place), pose.lift, d.slide);
        let (k, cx, cy) = (pose.scale, x + w / 2.0, y + d.h / 2.0);
        let drawn = self.painter.render(&self.rt, 1.0, |c| {
            let (hw, hh) = (w * k / 2.0, d.h * k / 2.0);
            self.frost(c, D2D_RECT_F { left: cx - hw, top: cy - hh, right: cx + hw, bottom: cy + hh });
            // a Matrix3x2: scaled by k about the pill's centre
            let m: [f32; 6] = [k, 0.0, 0.0, k, cx * (1.0 - k), cy * (1.0 - k)];
            unsafe { c.rt.SetTransform((&raw const m).cast()) };
            self.draw(c, (x, y, w), u, appear, t);
        });
        if let Err(e) = drawn {
            log(&format!("note: {e}"));
            return Life::Gone;
        }
        unsafe {
            let byte = (pose.alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
            let _ = SetLayeredWindowAttributes(self.hwnd, COLORREF(0), byte, LWA_ALPHA);
        }
        let busy = self.from.is_some() || appear < 1.0 || self.look.kind == Kind::Saving;
        if t < ENTER || t >= self.hold || busy {
            Life::Moving
        } else {
            Life::Still(self.hold - t)
        }
    }

    /// Fills the pill's shape `at` (window pixels) with the blurred screen behind it.
    fn frost(&self, c: &Canvas, at: D2D_RECT_F) {
        let Some(behind) = &self.behind else { return };
        let identity: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let radius = (at.bottom - at.top) / 2.0;
        unsafe {
            c.rt.SetTransform((&raw const identity).cast());
            if let Ok(brush) = c.rt.CreateBitmapBrush(behind, None, None) {
                c.rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: at, radiusX: radius, radiusY: radius }, &brush);
            }
        }
    }

    /// The pill at `x`, `y`, `w` wide, `u` of the way through a morph, its icon `appear` of the
    /// way drawn in, `t` seconds after it came up.
    fn draw(&self, c: &Canvas, (x, y, w): (f32, f32, f32), u: f32, appear: f32, t: f32) {
        let d = self.dims;
        let k = ease_out(u);
        let tint = if self.behind.is_some() { TINT_BLURRED } else { TINT_PLAIN };
        c.round_rect(x, y, w, d.h, d.h / 2.0, rgba(TINT[0], TINT[1], TINT[2], tint));
        c.round_rect_outline([x + 0.5, y + 0.5, w - 1.0, d.h - 1.0], (d.h - 1.0) / 2.0, 1.0, rgba(255, 255, 255, 0.12));

        let from = self.from.as_ref().map(|(from, _)| from);
        let centre = (x + d.pad + d.icon / 2.0, y + d.h / 2.0);
        if let Some(from) = from {
            let shrunk = Dims { icon: d.icon * (1.0 - 0.4 * k), ..d };
            icon(c, from.kind, centre, shrunk, 1.0, t, 1.0 - (u / 0.5).min(1.0));
        }
        icon(c, self.look.kind, centre, d, appear, t, 1.0);

        let tx = x + d.pad + d.icon + d.gap;
        let clip = D2D_RECT_F { left: tx, top: y, right: x + w - d.pad / 2.0, bottom: y + d.h };
        unsafe { c.rt.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
        if let Some(from) = from {
            self.words(c, from, (tx, y - d.roll * k), 1.0 - (u / 0.5).min(1.0));
        }
        self.words(c, &self.look, (tx, y + d.roll * (1.0 - k)), ((u - 0.35) / 0.65).clamp(0.0, 1.0));
        unsafe { c.rt.PopAxisAlignedClip() };
    }

    /// `look`'s two lines, starting at `x` in a pill whose top is `y`.
    fn words(&self, c: &Canvas, look: &Look, (x, y): (f32, f32), alpha: f32) {
        if alpha <= 0.0 {
            return;
        }
        let h = self.dims.h;
        c.text(&look.title, &self.big, [x, y + h * 0.16, look.w, h * 0.42], rgba(255, 255, 255, 0.96 * alpha));
        c.text(&look.detail, &self.small, [x, y + h * 0.54, look.w, h * 0.32], rgba(255, 255, 255, 0.62 * alpha));
    }
}

/// `kind`'s icon, `d.icon` across, centred on `(cx, cy)`: its shape grows in and its mark draws
/// itself over the first `appear` (0 to 1) of the way; `t` (seconds) turns the spinner.
fn icon(c: &Canvas, kind: Kind, (cx, cy): (f32, f32), d: Dims, appear: f32, t: f32, alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let (r, s) = (d.icon / 2.0, d.s);
    let grow = ease_out((appear / 0.5).min(1.0));
    let mark = ease_out(((appear - 0.35) / 0.65).clamp(0.0, 1.0));
    let at = |px: f32, py: f32| (cx + px * r, cy + py * r);
    let color = |[r, g, b]: [u8; 3]| rgba(r, g, b, alpha);
    let ink = color(TINT);
    match kind {
        Kind::Info => c.circle((cx, cy), r * 0.42 * grow, color(AMBER)),
        Kind::Saving => {
            let track = r - s;
            c.ring((cx, cy), track, 2.0 * s, rgba(AMBER[0], AMBER[1], AMBER[2], 0.22 * alpha * grow));
            let (start, sweep) = (t * SPIN * TAU, TAU * 0.28 * grow);
            let arc: Vec<_> = (0..=16)
                .map(|i| start + sweep * i as f32 / 16.0)
                .map(|a| (cx + track * a.cos(), cy + track * a.sin()))
                .collect();
            c.polyline(&arc, 2.0 * s, color(AMBER));
        }
        Kind::Saved => {
            c.circle((cx, cy), r * grow, color(AMBER));
            c.polyline(&partial(&[at(-0.42, 0.02), at(-0.12, 0.32), at(0.44, -0.3)], mark), 2.2 * s, ink);
        }
        Kind::Failed => {
            c.circle((cx, cy), r * grow, color(RED));
            let white = rgba(255, 255, 255, alpha);
            c.polyline(&partial(&[at(-0.32, -0.32), at(0.32, 0.32)], (mark * 2.0).min(1.0)), 2.2 * s, white);
            if mark > 0.5 {
                c.polyline(&partial(&[at(0.32, -0.32), at(-0.32, 0.32)], mark * 2.0 - 1.0), 2.2 * s, white);
            }
        }
        Kind::Warning => {
            let corners = [at(0.0, -0.84 * grow), at(0.92 * grow, 0.74 * grow), at(-0.92 * grow, 0.74 * grow)];
            c.polygon(&corners, color(YELLOW));
            // a stroke the same colour rounds the corners
            c.polyline(&[corners[0], corners[1], corners[2], corners[0]], 2.4 * s, color(YELLOW));
            c.polyline(&partial(&[at(0.0, -0.34), at(0.0, 0.16)], mark), 2.0 * s, ink);
            c.circle(at(0.0, 0.46), 1.2 * s * mark, ink);
        }
    }
}

/// The first `k` (0 to 1, by length) of the path through `points`; nothing at 0.
fn partial(points: &[(f32, f32)], k: f32) -> Vec<(f32, f32)> {
    if k <= 0.0 || points.is_empty() {
        return Vec::new();
    }
    let gap = |(x0, y0): (f32, f32), (x1, y1): (f32, f32)| (x1 - x0).hypot(y1 - y0);
    let mut left = points.windows(2).map(|p| gap(p[0], p[1])).sum::<f32>() * k.min(1.0);
    let mut path = vec![points[0]];
    for p in points.windows(2) {
        if left <= 0.0 {
            break;
        }
        let l = gap(p[0], p[1]);
        if left >= l {
            path.push(p[1]);
            left -= l;
        } else {
            let f = left / l;
            path.push((lerp(p[0].0, p[1].0, f), lerp(p[0].1, p[1].1, f)));
            break;
        }
    }
    path
}

fn window(at: Rect) -> Result<HWND, String> {
    unsafe {
        let hinst = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = w!("afterglow_note");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc); // fails harmlessly when already registered
        let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        CreateWindowExW(ex, class, w!("afterglow"), WS_POPUP, at.x, at.y, at.w, at.h, None, None, Some(hinst.into()), None)
            .map_err(|e| format!("CreateWindowEx: {e}"))
    }
}

/// Draws every note's frame for now, closes the ones that are gone, and keeps the frames coming
/// while any moves, or wakes a still one when it's time to leave.
fn tick() {
    let now = Instant::now();
    let moving = NOTES.with(|n| {
        let mut n = n.borrow_mut();
        let mut moving = false;
        n.retain_mut(|note| match note.paint(now) {
            Life::Moving => {
                moving = true;
                true
            }
            Life::Still(left) => {
                unsafe { SetTimer(Some(note.hwnd), TIMER, (left * 1000.0).ceil() as u32 + 1, None) };
                true
            }
            Life::Gone => {
                unsafe {
                    let _ = DestroyWindow(note.hwnd);
                }
                false
            }
        });
        n.first().filter(|_| moving).map(|note| note.hwnd)
    });
    pace(moving);
}

/// The window the pacer wakes with [`WM_FRAME`], as an `isize`; 0 while every note is still.
static PACED: AtomicIsize = AtomicIsize::new(0);
/// Whether a frame is already waiting, so a busy thread doesn't pile them up.
static QUEUED: AtomicBool = AtomicBool::new(false);
/// Wakes [`PACED`] once per composition (the display's refresh rate); sleeps parked while it's 0.
static PACER: LazyLock<Thread> = LazyLock::new(|| {
    let spawned = std::thread::Builder::new().name("note pacer".into()).spawn(|| loop {
        let h = PACED.load(Ordering::Relaxed);
        if h == 0 {
            std::thread::park();
            continue;
        }
        if unsafe { DwmFlush() }.is_err() {
            std::thread::sleep(Duration::from_millis(7));
        }
        if PACED.load(Ordering::Relaxed) == h && !QUEUED.swap(true, Ordering::Relaxed) {
            let posted = unsafe { PostMessageW(Some(HWND(h as _)), WM_FRAME, WPARAM(0), LPARAM(0)) };
            if posted.is_err() {
                QUEUED.store(false, Ordering::Relaxed);
            }
        }
    });
    spawned.expect("the note pacer thread").thread().clone()
});

/// Has `hwnd` woken after every composition, or stops (`None`).
fn pace(hwnd: Option<HWND>) {
    let h = hwnd.map_or(0, |h| h.0 as isize);
    if PACED.swap(h, Ordering::Relaxed) == h || h == 0 {
        return;
    }
    QUEUED.store(false, Ordering::Relaxed); // a frame posted to a window since closed never comes
    PACER.unpark();
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_FRAME => {
            QUEUED.store(false, Ordering::Relaxed);
            tick();
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER => {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER);
            }
            tick();
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn it_slides_in_from_its_edge_holds_and_slides_back_out() {
        let slide = 28.0;
        let at = |t, top| pose(t, 2.0).map(|p| (pill_y(top, p.lift, slide), p.alpha));
        // top places come down from above, bottom ones up from below; both end where they're placed
        assert_eq!(at(0.0, true), Some((0.0, 0.0)));
        assert_eq!(at(0.0, false), Some((slide, 0.0)));
        let (y, a) = at(ENTER / 2.0, true).unwrap();
        assert!(close(y, slide * 0.875) && close(a, 0.875), "eased out: most of the way there by half time");
        assert!(close(at(ENTER / 2.0, false).unwrap().0, slide * 0.125));
        assert_eq!(at(ENTER, true), Some((slide, 1.0)));
        assert_eq!(at(ENTER, false), Some((0.0, 1.0)));
        assert_eq!(pose(1.0, 2.0), Some(Pose { lift: 0.0, scale: 1.0, alpha: 1.0 }));
        // leaving eases in: barely moved by half time, while already half faded
        let (y, a) = at(2.0 + EXIT / 2.0, true).unwrap();
        assert!(close(y, slide * 0.75) && close(a, 0.5));
        assert!(close(at(2.0 + EXIT / 2.0, false).unwrap().0, slide * 0.25));
        assert_eq!(at(2.0 + EXIT, true), None);
    }

    #[test]
    fn it_grows_in_and_shrinks_a_little_on_the_way_out() {
        assert!(close(pose(0.0, 2.0).unwrap().scale, SHRINK));
        assert!(close(pose(ENTER, 2.0).unwrap().scale, 1.0));
        let out = pose(2.0 + EXIT * 0.999, 2.0).unwrap().scale;
        assert!(out < 1.0 && out > SHRINK, "{out}");
    }

    #[test]
    fn a_note_on_its_way_out_comes_back_from_where_it_is() {
        assert_eq!(rejoin(1.0, 2.0), 1.0, "one that isn't leaving carries on");
        let t = rejoin(2.0 + EXIT / 2.0, 2.0);
        assert!(close(t, ENTER / 2.0));
        let (back, out) = (pose(t, 9.0).unwrap(), pose(2.0 + EXIT / 2.0, 2.0).unwrap());
        assert!((back.lift - out.lift).abs() < 0.2 && back.alpha >= out.alpha);
        assert_eq!(rejoin(5.0, 2.0), 0.0, "one that's gone comes in from the start");
    }

    #[test]
    fn a_note_sent_away_leaves_from_where_it_is() {
        assert_eq!(leave_at(1.0, 2.0), 1.0, "a still one starts leaving now");
        assert_eq!(leave_at(2.1, 2.0), 2.0, "one already leaving carries on");
        let t = ENTER / 2.0;
        let (out, was) = (pose(t, leave_at(t, 2.0)).unwrap(), pose(t, 2.0).unwrap());
        assert!((out.lift - was.lift).abs() < 0.2 && (out.alpha - was.alpha).abs() < 0.5);
        assert_eq!(pose(0.0, leave_at(0.0, 2.0)), None, "one that hadn't shown yet is gone");
    }

    #[test]
    fn a_morph_eases_the_width_to_the_new_text() {
        assert_eq!(progress(0.0, MORPH), 0.0);
        assert_eq!(progress(MORPH, MORPH), 1.0);
        assert_eq!(progress(MORPH * 2.0, MORPH), 1.0);
        let w = lerp(100.0, 200.0, ease_out(progress(MORPH / 2.0, MORPH)));
        assert!(close(w, 187.5), "{w}");
    }

    #[test]
    fn a_mark_draws_itself_in_along_its_path() {
        let check = [(0.0, 0.0), (3.0, 4.0), (9.0, -4.0)]; // legs 5 and 10 long
        assert!(partial(&check, 0.0).is_empty());
        assert_eq!(partial(&check, 1.0 / 3.0), vec![(0.0, 0.0), (3.0, 4.0)]);
        let half = partial(&check, 0.5);
        assert_eq!(half.len(), 3);
        assert!(close(half[2].0, 4.5) && close(half[2].1, 2.0), "{half:?}");
        assert_eq!(partial(&check, 1.0), check.to_vec());
    }

    #[test]
    fn the_blur_spreads_a_spot_evenly_and_keeps_flat_colour_flat() {
        let (w, h) = (9, 7);
        let mut flat = vec![0x0012_3456; w * h];
        blur(&mut flat, (w, h), 2);
        assert!(flat.iter().all(|&p| p == 0xFF12_3456), "opaque, and the same colour");

        let mut spot = vec![0; w * h];
        spot[3 * w + 4] = 0x00FF_FFFF;
        blur(&mut spot, (w, h), 1);
        let red = |x: usize, y: usize| (spot[y * w + x] >> 16) & 0xFF;
        assert!(red(4, 3) > red(3, 3) && red(3, 3) > red(2, 3), "it fades away from the spot");
        assert_eq!((red(3, 3), red(4, 2)), (red(5, 3), red(4, 4)), "evenly each way");
        assert_eq!(red(0, 0), 0, "and only so far");
    }
}
