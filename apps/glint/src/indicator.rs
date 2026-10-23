//! On-screen recording indicator: viewfinder corner brackets around the recorded region, an
//! optional countdown bubble in its centre before the recording starts, and a small floating
//! pill with a pulsing red dot, the elapsed time and pause / stop / discard buttons.
//!
//! Only windows over the recorded area are excluded from screen capture: NVIDIA Instant
//! Replay and some other capture apps pause while any window with a display affinity is
//! visible. The frame sits outside the area unless pushed inside at monitor edges, the pill
//! looks for a spot outside it, and the countdown bubble closes before capture starts; what
//! still overlaps the area is left out or excluded per [`IndicatorInside`]. Excluding rules
//! out per-pixel-alpha layered windows (UpdateLayeredWindow) on Windows 10, so:
//! * brackets are click-through constant-alpha layered windows shaped by a window region;
//! * the pill is a DWM glass popup that Direct2D draws into with per-pixel alpha.

use std::cell::RefCell;
use std::f32::consts::TAU;
use std::time::{Duration, Instant};

use glint_core::config::IndicatorInside;
use glint_core::geom::rect::Rect;
use glint_core::log::log;
use glint_media::rect::WinRect;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, TRUE, WPARAM};
use windows::Win32::Graphics::Direct2D::ID2D1HwndRenderTarget;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmFlush, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, w};

use crate::platform::gfx::{Dib, Painter, animate, ease_out, make_glass, rgba};
use glint_shell::monitors;

const FADE_IN_MS: f32 = 160.0;
const FADE_OUT_MS: f32 = 120.0;
const PULSE_S: f32 = 1.6;
const TIMER_ID: usize = 1;
/// Fast timer that runs only while a hover transition is in flight.
const ANIM_TIMER_ID: usize = 2;
const HOVER_MS: f32 = 140.0;
const COUNT_TIMER_ID: usize = 3;
const COUNT_POP_MS: f32 = 220.0;
/// Bracket colors (BGR): light brackets over dark content, dark over bright content.
const LIGHT: (COLORREF, COLORREF) = (COLORREF(0x00F4F4F4), COLORREF(0x00303030));
const DARK: (COLORREF, COLORREF) = (COLORREF(0x00181614), COLORREF(0x00B0B0B0));
/// Opacity of the solid corner brackets and of the dashed sides between them.
const CORNER_ALPHA: u8 = 255;
const DASH_ALPHA: u8 = 150;

pub(crate) const STOP: usize = 0;
pub(crate) const DISCARD: usize = 1;
pub(crate) const PAUSE: usize = 2;

pub(crate) struct Indicator {
    /// Brackets and dashed sides with their full opacity.
    marks: Vec<(HWND, u8)>,
    pill: Option<HWND>,
    /// Where the pill goes once recording, and whether it's excluded from capture; None when hidden.
    pill_at: Option<(Rect, bool)>,
    count: Option<HWND>,
    owner: HWND,
    msg: u32,
}

struct CountState {
    owner: HWND,
    msg: u32,
    painter: Painter,
    font: IDWriteTextFormat,
    rt: ID2D1HwndRenderTarget,
    size: f32,
    n: u32,
    changed: Instant,
    fade: f32,
}

struct PillState {
    owner: HWND,
    msg: u32,
    painter: Painter,
    font: IDWriteTextFormat,
    rt: ID2D1HwndRenderTarget,
    layout: Layout,
    started: Instant,
    paused_for: Duration,
    paused_at: Option<Instant>,
    hover: Option<Button>,
    /// Hover progress of [pause, stop, discard], 0 (idle) ..= 1 (hovered).
    hover_t: [f32; 3],
    last_frame: Instant,
    /// The fast timer and 1 ms timer resolution are active.
    animating: bool,
    fade: f32,
}

impl PillState {
    fn elapsed(&self) -> Duration {
        let paused = self.paused_for + self.paused_at.map_or(Duration::ZERO, |p| p.elapsed());
        self.started.elapsed().saturating_sub(paused)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Button {
    /// Pause or resume.
    Pause,
    /// Stop and save.
    Stop,
    /// Stop and delete the file.
    Discard,
}

const BUTTONS: [Button; 3] = [Button::Pause, Button::Stop, Button::Discard];

impl Button {
    fn wparam(self) -> usize {
        match self {
            Button::Pause => PAUSE,
            Button::Stop => STOP,
            Button::Discard => DISCARD,
        }
    }
}

/// Pill geometry, in pixels: [pad | dot | gap | m:ss | gap | ❚❚ | ■ | ✕ | pad]. The buttons are
/// square hit areas whose hover circles are concentric with the pill's rounded end.
struct Layout {
    w: f32,
    h: f32,
    dot: f32,
    text_x: f32,
    time_w: f32,
    button: f32,
    top: f32,
    xs: [f32; 3],
}

impl Layout {
    fn new(p: &Painter, time_w: f32) -> Layout {
        let (h, button, dot) = (p.px(28.0), p.px(22.0), p.px(8.0));
        let top = ((h - button) / 2.0).floor();
        let text_x = p.px(12.0) + dot + p.px(8.0);
        let pause_x = text_x + time_w + p.px(6.0);
        let stop_x = pause_x + button + p.px(1.0);
        let discard_x = stop_x + button + p.px(1.0);
        Layout { w: discard_x + button + top, h, dot, text_x, time_w, button, top, xs: [pause_x, stop_x, discard_x] }
    }

    fn hit(&self, x: f32, y: f32) -> Option<Button> {
        if y < self.top || y >= self.top + self.button {
            return None;
        }
        BUTTONS.into_iter().zip(self.xs).find(|&(_, bx)| x >= bx && x < bx + self.button).map(|(b, _)| b)
    }
}

thread_local! {
    static PILL: RefCell<Option<PillState>> = const { RefCell::new(None) };
    static COUNT: RefCell<Option<CountState>> = const { RefCell::new(None) };
    /// Pre-rendered bracket/dash images, painted on WM_PAINT.
    static MARKS: RefCell<Vec<(HWND, Dib)>> = const { RefCell::new(Vec::new()) };
}

fn register(class: PCWSTR, proc_: WNDPROC, cursor: PCWSTR) {
    unsafe {
        let Ok(hinst) = GetModuleHandleW(None) else { return };
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: proc_,
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, cursor).unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc); // fails harmlessly when already registered
    }
}

/// A topmost, non-activating popup (not yet shown). Call [`exclude_from_capture`] once its
/// transparency mode is set up: on a layered window the call fails until then.
fn popup(class: PCWSTR, ex: WINDOW_EX_STYLE, r: Rect) -> Option<HWND> {
    unsafe {
        let hinst = GetModuleHandleW(None).ok()?;
        let hwnd = CreateWindowExW(
            ex | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            w!("glint recording"),
            WS_POPUP,
            r.x,
            r.y,
            r.w,
            r.h,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .ok()?;
        let on = TRUE;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, &on as *const BOOL as _, 4);
        Some(hwnd)
    }
}

fn exclude_from_capture(hwnd: HWND) {
    unsafe {
        if let Err(e) = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) {
            log(&format!("indicator: SetWindowDisplayAffinity failed: {e}"));
        }
    }
}

/// How an indicator window appears while recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shown {
    /// Clear of the recorded area.
    Plain,
    /// Over the recorded area, excluded from capture.
    Excluded,
    /// Over the recorded area, left out.
    Hidden,
}

/// How a window covering `piece` appears given the recorded `regions` (desktop px).
fn shown(piece: Rect, regions: &[Rect], inside: IndicatorInside) -> Shown {
    if !regions.iter().any(|r| r.intersect(&piece).is_some()) {
        return Shown::Plain;
    }
    match inside {
        IndicatorInside::Show => Shown::Excluded,
        IndicatorInside::Hide => Shown::Hidden,
    }
}

/// The `parts` (relative to `pos`) of a frame piece that are drawn, each with its 1 px rim,
/// and whether the piece must be excluded from capture.
fn kept_parts(pos: Rect, parts: &[Rect], regions: &[Rect], inside: IndicatorInside) -> (Vec<Rect>, bool) {
    let mut kept = Vec::with_capacity(parts.len());
    let mut exclude = false;
    for p in parts {
        match shown(p.inflate(1).offset(pos.x, pos.y), regions, inside) {
            Shown::Plain => kept.push(*p),
            Shown::Excluded => {
                kept.push(*p);
                exclude = true;
            }
            Shown::Hidden => {}
        }
    }
    (kept, exclude)
}

/// Squared distance between the nearest points of `a` and `b`.
fn distance_sq(a: &Rect, b: &Rect) -> i64 {
    let dx = (a.x - b.right()).max(b.x - a.right()).max(0) as i64;
    let dy = (a.y - b.bottom()).max(b.y - a.bottom()).max(0) as i64;
    dx * dx + dy * dy
}

/// Where the `size` pill goes for the frame `outer` around `capture`: `gap` beside the frame
/// (above, below, left, right) on its monitor `home`, then on the other `screens` nearest
/// first, at the first spot clear of the frame; failing that, inside the area's top-left
/// corner, excluded from capture or hidden per `inside`.
fn pill_spot(
    capture: Rect,
    outer: Rect,
    home: Rect,
    screens: &[Rect],
    (w, h): (i32, i32),
    gap: i32,
    inside: IndicatorInside,
) -> (Rect, Shown) {
    let recorded: Vec<Rect> = std::iter::once(&home).chain(screens).filter(|s| capture.intersect(s).is_some()).copied().collect();
    let regions: Vec<Rect> = recorded.iter().filter_map(|s| capture.intersect(s)).collect();
    let framed: Vec<Rect> = recorded.iter().filter_map(|s| outer.intersect(s)).collect();
    let mut others: Vec<&Rect> = screens.iter().filter(|s| **s != home).collect();
    others.sort_by_key(|s| distance_sq(s, &capture));
    for s in std::iter::once(&home).chain(others) {
        let f = outer.intersect(&s.inflate(-1)).unwrap_or(outer);
        let beside = [
            (f.x, f.y - 1 - gap - h),
            (f.x, f.bottom() + 1 + gap),
            (f.x - 1 - gap - w, f.y),
            (f.right() + 1 + gap, f.y),
        ];
        for (x, y) in beside {
            let spot = clamp_into(x, y, w, h, *s);
            if !framed.iter().any(|r| r.intersect(&spot).is_some()) {
                return (spot, Shown::Plain);
            }
        }
    }
    let spot = clamp_into(capture.x + 2 * gap, capture.y + 2 * gap, w, h, home);
    (spot, shown(spot, &regions, inside))
}

/// Keeps a `w`x`h` box at (x, y) inside `mon`.
fn clamp_into(x: i32, y: i32, w: i32, h: i32, mon: Rect) -> Rect {
    Rect { x: x.clamp(mon.x, (mon.right() - w).max(mon.x)), y: y.clamp(mon.y, (mon.bottom() - h).max(mon.y)), w, h }
}

/// Average luminance (0..1) of the screen inside `r`.
fn luminance_under(r: Rect) -> f32 {
    let Ok(dib) = Dib::new(r.w, r.h) else { return 0.0 };
    unsafe {
        let screen = GetDC(None);
        let _ = BitBlt(dib.dc, 0, 0, r.w, r.h, Some(screen), r.x, r.y, SRCCOPY);
        ReleaseDC(None, screen);
        let _ = GdiFlush();
    }
    let px = dib.pixels();
    let sum: f32 = px
        .iter()
        .map(|&p| {
            let (r, g, b) = ((p >> 16) & 255, (p >> 8) & 255, p & 255);
            (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0
        })
        .sum();
    sum / px.len().max(1) as f32
}

/// A frame piece (corner bracket or dashed side): a window shaped to `parts` plus a 1 px rim
/// around each, at opacity `alpha` once faded in. Parts over the recorded `regions` are left
/// out or excluded from capture per `inside`.
fn mark(pos: Rect, parts: &[Rect], alpha: u8, regions: &[Rect], inside: IndicatorInside) -> Option<(HWND, u8)> {
    let (parts, exclude) = kept_parts(pos, parts, regions, inside);
    if pos.w <= 2 || pos.h <= 2 || parts.is_empty() {
        return None;
    }
    let (fill, rim) = if luminance_under(pos) > 0.55 { DARK } else { LIGHT };
    let hwnd = popup(w!("glint_mark"), WS_EX_LAYERED | WS_EX_TRANSPARENT, pos)?;
    let dib = Dib::new(pos.w, pos.h).ok()?;
    unsafe {
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA);
        if exclude {
            exclude_from_capture(hwnd);
        }
        let rgn = CreateRectRgn(0, 0, 0, 0);
        let (rim_brush, fill_brush) = (CreateSolidBrush(rim), CreateSolidBrush(fill));
        let whole = Rect { x: 0, y: 0, w: pos.w, h: pos.h }.to_win();
        FillRect(dib.dc, &whole, rim_brush);
        for a in parts {
            let outer = a.inflate(1);
            let part = CreateRectRgn(outer.x, outer.y, outer.right(), outer.bottom());
            CombineRgn(Some(rgn), Some(rgn), Some(part), RGN_OR);
            let _ = DeleteObject(part.into());
            FillRect(dib.dc, &a.to_win(), fill_brush);
        }
        let _ = DeleteObject(rim_brush.into());
        let _ = DeleteObject(fill_brush.into());
        SetWindowRgn(hwnd, Some(rgn), false); // the window owns the region now
        MARKS.with(|m| m.borrow_mut().push((hwnd, dib)));
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    Some((hwnd, alpha))
}

impl Indicator {
    /// `capture` and `monitors` (the recorded outputs) are in desktop coordinates. The pill's
    /// buttons post `msg` to `owner` with WPARAM [`STOP`], [`DISCARD`] or [`PAUSE`]. With a
    /// `countdown`, a bubble showing it replaces the pill until [`Indicator::start`]; clicking
    /// it posts [`DISCARD`], and [`Indicator::end_countdown`] must close it before capture
    /// starts. `inside` decides what overlaps the recorded area. Fades in before returning.
    pub(crate) fn show(
        capture: Rect,
        monitors: &[Rect],
        owner: HWND,
        msg: u32,
        countdown: u32,
        inside: IndicatorInside,
    ) -> Result<Indicator, String> {
        let painter = Painter::new()?;
        let px = |v: f32| painter.px(v) as i32;
        register(w!("glint_mark"), Some(mark_proc), IDC_ARROW);
        register(w!("glint_pill"), Some(pill_proc), IDC_ARROW);
        register(w!("glint_count"), Some(count_proc), IDC_HAND);

        // The frame sits just outside the region; where that is off the monitors it is pushed
        // inside, where it is left out or excluded from capture.
        let (arm, th, gap) = (px(20.0), px(3.0), px(4.0));
        let (dash, space, inset) = (px(8.0), px(6.0), arm + px(6.0));
        let outer = capture.inflate(gap + th);
        let (corners, sides) = frame(outer, monitors, inset);
        let Some(&anchor) = corners.first() else { return Err("the recorded region is on no monitor".into()) };
        let size = arm + 2; // + rim on both sides
        let regions: Vec<Rect> = monitors.iter().filter_map(|m| capture.intersect(m)).collect();
        let mut marks = Vec::with_capacity(corners.len() + sides.len());
        for c in &corners {
            let x = if c.right { c.x + 1 - size } else { c.x - 1 };
            let y = if c.bottom { c.y + 1 - size } else { c.y - 1 };
            let near = |far: bool, len: i32| if far { size - 1 - len } else { 1 };
            let horizontal = Rect { x: near(c.right, arm), y: near(c.bottom, th), w: arm, h: th };
            let vertical = Rect { x: near(c.right, th), y: near(c.bottom, arm), w: th, h: arm };
            let pos = Rect { x, y, w: size, h: size };
            marks.extend(mark(pos, &[horizontal, vertical], CORNER_ALPHA, &regions, inside));
        }
        // Dashed sides between the brackets.
        for s in &sides {
            let len = s.to - s.from;
            let across = if s.far { s.at - th - 1 } else { s.at - 1 };
            let pos = if s.horizontal {
                Rect { x: s.from - 1, y: across, w: len + 2, h: th + 2 }
            } else {
                Rect { x: across, y: s.from - 1, w: th + 2, h: len + 2 }
            };
            let parts: Vec<Rect> = dashes(len, dash, space)
                .map(|off| if s.horizontal {
                    Rect { x: 1 + off, y: 1, w: dash, h: th }
                } else {
                    Rect { x: 1, y: 1 + off, w: th, h: dash }
                })
                .collect();
            marks.extend(mark(pos, &parts, DASH_ALPHA, &regions, inside));
        }

        let home = monitors[anchor.monitor];
        let screens: Vec<Rect> = match monitors::enumerate() {
            Ok(outputs) if !outputs.is_empty() => outputs.iter().map(|o| o.rect).collect(),
            _ => monitors.to_vec(),
        };

        let font = painter.font(13.0)?;
        let time_w = painter.measure("88:88", &font).0.ceil();
        let layout = Layout::new(&painter, time_w);
        let size = (layout.w.ceil() as i32, layout.h as i32);
        let pill_at = match pill_spot(capture, outer, home, &screens, size, px(6.0), inside) {
            (pos, Shown::Plain) => Some((pos, false)),
            (pos, Shown::Excluded) => Some((pos, true)),
            (_, Shown::Hidden) => {
                log("indicator: no room for the pill outside the recorded area; hidden");
                None
            }
        };
        let mut ind = Indicator { marks, pill: None, pill_at, count: None, owner, msg };
        if countdown > 0 {
            let (cx, cy) = (capture.x + capture.w / 2, capture.y + capture.h / 2);
            let on = monitors.iter().copied().find(|m| m.contains(cx, cy)).unwrap_or(home);
            let d = px(104.0);
            ind.count = Some(open_count(clamp_into(cx - d / 2, cy - d / 2, d, d, on), countdown, owner, msg)?);
        } else if let Some((pos, exclude)) = pill_at {
            ind.pill = Some(open_pill(painter, font, layout, pos, exclude, owner, msg)?);
        }
        animate(FADE_IN_MS, |t| {
            set_fade(&ind.marks, t);
            set_pill_fade(t);
            set_count_fade(t);
        });
        Ok(ind)
    }

    pub(crate) fn count(&self, n: u32) {
        COUNT.with(|c| {
            if let Ok(mut g) = c.try_borrow_mut()
                && let Some(st) = g.as_mut()
            {
                st.n = n;
                st.changed = Instant::now();
            }
        });
        render_count();
    }

    /// Fades out and closes the countdown bubble, and waits until the composed desktop no
    /// longer shows it, so capture can start without excluding it.
    pub(crate) fn end_countdown(&mut self) {
        if let Some(count) = self.count.take() {
            animate(FADE_OUT_MS, |t| set_count_fade(1.0 - t));
            close_count(count);
            unsafe {
                let _ = DwmFlush();
            }
        }
    }

    /// Shows the pill after a countdown; call [`Indicator::end_countdown`] first.
    pub(crate) fn start(&mut self) -> Result<(), String> {
        if self.pill.is_none()
            && let Some((pos, exclude)) = self.pill_at
        {
            let painter = Painter::new()?;
            let font = painter.font(13.0)?;
            let layout = Layout::new(&painter, painter.measure("88:88", &font).0.ceil());
            self.pill = Some(open_pill(painter, font, layout, pos, exclude, self.owner, self.msg)?);
            animate(FADE_IN_MS, set_pill_fade);
        }
        Ok(())
    }

    pub(crate) fn set_paused(&self, on: bool) {
        PILL.with(|p| {
            if let Ok(mut g) = p.try_borrow_mut()
                && let Some(st) = g.as_mut()
            {
                match (on, st.paused_at) {
                    (true, None) => st.paused_at = Some(Instant::now()),
                    (false, Some(at)) => {
                        st.paused_for += at.elapsed();
                        st.paused_at = None;
                    }
                    _ => {}
                }
            }
        });
        render_pill();
    }
}

fn open_pill(
    painter: Painter,
    font: IDWriteTextFormat,
    layout: Layout,
    pos: Rect,
    exclude: bool,
    owner: HWND,
    msg: u32,
) -> Result<HWND, String> {
    let pill = popup(w!("glint_pill"), WINDOW_EX_STYLE(0), pos).ok_or("can't create the indicator window")?;
    make_glass(pill);
    if exclude {
        exclude_from_capture(pill);
    }
    let rt = painter.window_target(pill, pos.w, pos.h)?;
    PILL.with(|p| {
        *p.borrow_mut() = Some(PillState {
            owner,
            msg,
            painter,
            font,
            rt,
            layout,
            started: Instant::now(),
            paused_for: Duration::ZERO,
            paused_at: None,
            hover: None,
            hover_t: [0.0; 3],
            last_frame: Instant::now(),
            animating: false,
            fade: 0.0,
        })
    });
    unsafe {
        let _ = ShowWindow(pill, SW_SHOWNOACTIVATE);
        render_pill(); // fully transparent until the fade starts
        SetTimer(Some(pill), TIMER_ID, 33, None);
    }
    Ok(pill)
}

fn open_count(pos: Rect, n: u32, owner: HWND, msg: u32) -> Result<HWND, String> {
    let painter = Painter::new()?;
    let font = painter.font(46.0)?;
    let hwnd = popup(w!("glint_count"), WINDOW_EX_STYLE(0), pos).ok_or("can't create the countdown window")?;
    make_glass(hwnd);
    let rt = match painter.window_target(hwnd, pos.w, pos.h) {
        Ok(rt) => rt,
        Err(e) => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return Err(e);
        }
    };
    COUNT.with(|c| {
        *c.borrow_mut() =
            Some(CountState { owner, msg, painter, font, rt, size: pos.w as f32, n, changed: Instant::now(), fade: 0.0 })
    });
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        render_count();
        SetTimer(Some(hwnd), COUNT_TIMER_ID, 16, None);
    }
    Ok(hwnd)
}

fn close_count(hwnd: HWND) {
    unsafe {
        let _ = KillTimer(Some(hwnd), COUNT_TIMER_ID);
    }
    COUNT.with(|c| c.borrow_mut().take());
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

impl Drop for Indicator {
    fn drop(&mut self) {
        if let Some(pill) = self.pill {
            unsafe {
                let _ = KillTimer(Some(pill), TIMER_ID);
            }
        }
        animate(FADE_OUT_MS, |t| {
            set_fade(&self.marks, 1.0 - t);
            set_pill_fade(1.0 - t);
            set_count_fade(1.0 - t);
        });
        if let Some(count) = self.count.take() {
            close_count(count);
        }
        if let Some(st) = PILL.with(|p| p.borrow_mut().take())
            && st.animating
        {
            unsafe {
                timeEndPeriod(1);
            }
        }
        unsafe {
            for h in self.marks.iter().map(|m| m.0).chain(self.pill) {
                let _ = DestroyWindow(h);
            }
        }
        MARKS.with(|m| m.borrow_mut().clear());
    }
}

/// Offsets of evenly spaced `dash`-long dashes centered along `len` pixels.
fn dashes(len: i32, dash: i32, space: i32) -> impl Iterator<Item = i32> {
    let n = ((len + space) / (dash + space)).max(0);
    let start = (len - (n * dash + (n - 1).max(0) * space)) / 2;
    (0..n).map(move |i| start + i * (dash + space))
}

/// A corner bracket at frame corner (x, y), drawn on `monitors[monitor]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Corner {
    x: i32,
    y: i32,
    right: bool,
    bottom: bool,
    monitor: usize,
}

/// A dashed side along frame edge `at` (a y for horizontal sides, an x for vertical ones)
/// over `from..to`; `far` marks bottom/right edges, whose line lies before `at`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Side {
    horizontal: bool,
    far: bool,
    at: i32,
    from: i32,
    to: i32,
}

/// Frame pieces for `outer` across `monitors`: each monitor shows its part of the frame,
/// pushed 1 px inside its edges, without the edges it shares with a neighbor. Each corner
/// bracket sits on the monitor whose part comes closest to that corner (TL, TR, BL, BR), and
/// dashed sides keep `inset` clear of the brackets.
fn frame(outer: Rect, monitors: &[Rect], inset: i32) -> (Vec<Corner>, Vec<Side>) {
    let parts: Vec<(usize, Rect, Rect)> = monitors
        .iter()
        .enumerate()
        .filter_map(|(i, m)| Some((i, outer.intersect(m)?, outer.intersect(&m.inflate(-1))?)))
        .collect();
    let mut corners = Vec::with_capacity(4);
    for (right, bottom) in [(false, false), (true, false), (false, true), (true, true)] {
        let corner = |r: &Rect| (if right { r.right() } else { r.x }, if bottom { r.bottom() } else { r.y });
        let (tx, ty) = corner(&outer);
        let best = parts.iter().min_by_key(|(_, _, f)| {
            let (x, y) = corner(f);
            (x - tx) as i64 * (x - tx) as i64 + (y - ty) as i64 * (y - ty) as i64
        });
        if let Some((monitor, _, f)) = best {
            let (x, y) = corner(f);
            corners.push(Corner { x, y, right, bottom, monitor: *monitor });
        }
    }

    let mut sides = Vec::new();
    for &(i, q, f) in &parts {
        for (horizontal, far) in [(true, false), (true, true), (false, false), (false, true)] {
            let edge = |r: &Rect, far: bool| match (horizontal, far) {
                (true, false) => r.y,
                (true, true) => r.bottom(),
                (false, false) => r.x,
                (false, true) => r.right(),
            };
            let span = |r: &Rect| if horizontal { (r.x, r.right()) } else { (r.y, r.bottom()) };
            let mut spans = vec![span(&q)];
            for (j, m) in monitors.iter().enumerate() {
                if j != i
                    && let Some(n) = outer.intersect(m)
                    && edge(&n, !far) == edge(&q, far)
                {
                    spans = subtract(&spans, span(&n));
                }
            }
            let at = edge(&f, far);
            for c in &corners {
                let (on_line, along, toward_far) = if horizontal {
                    (c.bottom == far && c.y == at, c.x, c.right)
                } else {
                    (c.right == far && c.x == at, c.y, c.bottom)
                };
                if on_line {
                    spans = subtract(&spans, if toward_far { (along - inset, along) } else { (along, along + inset) });
                }
            }
            let (lo, hi) = span(&f);
            for (a, b) in spans {
                let (from, to) = (a.max(lo), b.min(hi));
                if to > from {
                    sides.push(Side { horizontal, far, at, from, to });
                }
            }
        }
    }
    (corners, sides)
}

/// `spans` (half-open intervals) minus `c0..c1`.
fn subtract(spans: &[(i32, i32)], (c0, c1): (i32, i32)) -> Vec<(i32, i32)> {
    let mut out = Vec::with_capacity(spans.len() + 1);
    for &(a, b) in spans {
        if c1 <= a || c0 >= b {
            out.push((a, b));
            continue;
        }
        if a < c0 {
            out.push((a, c0));
        }
        if c1 < b {
            out.push((c1, b));
        }
    }
    out
}

fn set_fade(marks: &[(HWND, u8)], fade: f32) {
    for &(h, alpha) in marks {
        unsafe {
            let _ = SetLayeredWindowAttributes(h, COLORREF(0), (fade * alpha as f32) as u8, LWA_ALPHA);
        }
    }
}

fn set_pill_fade(fade: f32) {
    PILL.with(|p| {
        if let Ok(mut g) = p.try_borrow_mut()
            && let Some(st) = g.as_mut()
        {
            st.fade = fade;
        }
    });
    render_pill();
}

fn set_count_fade(fade: f32) {
    COUNT.with(|c| {
        if let Ok(mut g) = c.try_borrow_mut()
            && let Some(st) = g.as_mut()
        {
            st.fade = fade;
        }
    });
    render_count();
}

/// Moves hover progress toward the hovered button; returns whether anything is still moving.
fn advance_hover(st: &mut PillState) -> bool {
    let now = Instant::now();
    let step = (now - st.last_frame).as_secs_f32() * 1000.0 / HOVER_MS;
    st.last_frame = now;
    let mut moving = false;
    for (t, button) in st.hover_t.iter_mut().zip(BUTTONS) {
        let target = if st.hover == Some(button) { 1.0 } else { 0.0 };
        *t = if *t < target { (*t + step).min(target) } else { (*t - step).max(target) };
        moving |= *t != target;
    }
    moving
}

/// Redraws the pill: clock, pulse, hover transitions and current fade. Returns whether a
/// hover transition is still running.
fn render_pill() -> bool {
    PILL.with(|p| {
        let Ok(mut g) = p.try_borrow_mut() else { return false };
        let Some(st) = g.as_mut() else { return false };
        let moving = advance_hover(st);
        let px = |v: f32| st.painter.px(v);
        let l = &st.layout;
        let paused = st.paused_at.is_some();
        let secs = st.elapsed().as_secs();
        let clock = format!("{}:{:02}", secs / 60, secs % 60);
        let wave = (st.started.elapsed().as_secs_f32() * TAU / PULSE_S).cos();
        let white = |a: f32| rgba(255, 255, 255, a);
        let _ = st.painter.render(&st.rt, st.fade, |c| {
            c.round_rect(0.0, 0.0, l.w, l.h, l.h / 2.0, rgba(16, 16, 20, 0.88));
            let (dot, clock_alpha) =
                if paused { (rgba(255, 159, 10, 0.95), 0.6 + 0.36 * wave) } else { (rgba(255, 69, 58, 0.6 + 0.4 * wave), 0.96) };
            c.round_rect(px(12.0), (l.h - l.dot) / 2.0, l.dot, l.dot, l.dot / 2.0, dot);
            c.text(&clock, &st.font, [l.text_x, 0.0, l.time_w + 2.0, l.h], white(clock_alpha));
            for ((button, x), &h) in BUTTONS.into_iter().zip(l.xs).zip(&st.hover_t) {
                let e = h * h * (3.0 - 2.0 * h); // smoothstep
                let (cx, cy) = (x + l.button / 2.0, l.top + l.button / 2.0);
                if e > 0.0 {
                    // The hover circle grows out of the glyph while fading in.
                    let ring = if button == Button::Discard { rgba(255, 69, 58, 0.9 * e) } else { white(0.16 * e) };
                    let r = l.button / 2.0 * (0.6 + 0.4 * e);
                    c.round_rect(cx - r, cy - r, r * 2.0, r * 2.0, r, ring);
                }
                let a = 0.8 + 0.2 * e;
                match button {
                    Button::Pause if paused => {
                        let (w, h) = (px(4.5), px(5.5));
                        c.polygon(&[(cx - w * 0.7, cy - h), (cx - w * 0.7, cy + h), (cx + w * 1.3, cy)], white(a));
                    }
                    Button::Pause => {
                        let (w, h, gap) = (px(3.0), px(10.0), px(2.0));
                        c.round_rect(cx - gap / 2.0 - w, cy - h / 2.0, w, h, px(1.0), white(a));
                        c.round_rect(cx + gap / 2.0, cy - h / 2.0, w, h, px(1.0), white(a));
                    }
                    Button::Stop => {
                        let s = px(10.0);
                        c.round_rect(cx - s / 2.0, cy - s / 2.0, s, s, px(2.0), white(a));
                    }
                    Button::Discard => {
                        let (r, w) = (px(4.5), px(1.8));
                        c.line((cx - r, cy - r), (cx + r, cy + r), w, white(a));
                        c.line((cx - r, cy + r), (cx + r, cy - r), w, white(a));
                    }
                }
            }
        });
        moving
    })
}

fn render_count() {
    COUNT.with(|c| {
        let Ok(g) = c.try_borrow() else { return };
        let Some(st) = g.as_ref() else { return };
        let pop = ease_out((st.changed.elapsed().as_secs_f32() * 1000.0 / COUNT_POP_MS).min(1.0));
        let text = st.n.to_string();
        let r = st.size / 2.0;
        let _ = st.painter.render(&st.rt, st.fade, |c| {
            c.circle((r, r), r - 1.0, rgba(16, 16, 20, 0.88));
            c.ring((r, r), r - st.painter.px(5.0), st.painter.px(2.0), rgba(255, 69, 58, 0.35 + 0.5 * pop));
            let dy = st.painter.px(6.0) * (1.0 - pop);
            c.text_centred(&text, &st.font, (r, r + dy), rgba(255, 255, 255, 0.96 * pop));
        });
    });
}

/// Runs the fast animation timer (with 1 ms timer resolution, so it actually ticks every
/// ~8 ms instead of every 15.6 ms) until hover transitions settle.
fn set_animating(hwnd: HWND, on: bool) {
    PILL.with(|p| {
        let Ok(mut g) = p.try_borrow_mut() else { return };
        let Some(st) = g.as_mut() else { return };
        if st.animating == on {
            return;
        }
        st.animating = on;
        unsafe {
            if on {
                st.last_frame = Instant::now();
                timeBeginPeriod(1);
                SetTimer(Some(hwnd), ANIM_TIMER_ID, 8, None);
            } else {
                let _ = KillTimer(Some(hwnd), ANIM_TIMER_ID);
                timeEndPeriod(1);
            }
        }
    });
}

fn point(lp: LPARAM) -> (f32, f32) {
    ((lp.0 & 0xFFFF) as i16 as f32, ((lp.0 >> 16) & 0xFFFF) as i16 as f32)
}

/// Updates the hovered button; returns whether it changed.
fn set_hover(hover: Option<Button>) -> bool {
    PILL.with(|p| {
        let Ok(mut g) = p.try_borrow_mut() else { return false };
        let Some(st) = g.as_mut() else { return false };
        let changed = st.hover != hover;
        st.hover = hover;
        changed
    })
}

unsafe extern "system" fn mark_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                MARKS.with(|m| {
                    if let Some((_, dib)) = m.borrow().iter().find(|(h, _)| *h == hwnd) {
                        let _ = BitBlt(hdc, 0, 0, dib.w, dib.h, Some(dib.dc), 0, 0, SRCCOPY);
                    }
                });
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

unsafe extern "system" fn pill_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wp.0 == TIMER_ID => {
            render_pill();
        }
        WM_TIMER if wp.0 == ANIM_TIMER_ID => {
            if !render_pill() {
                set_animating(hwnd, false);
            }
        }
        WM_PAINT => unsafe {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            render_pill();
        },
        WM_ERASEBKGND => return LRESULT(1),
        WM_MOUSEMOVE => {
            let (x, y) = point(lp);
            let hit = PILL.with(|p| p.try_borrow().ok().and_then(|g| g.as_ref().and_then(|s| s.layout.hit(x, y))));
            let mut tme = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            if set_hover(hit) {
                set_animating(hwnd, true);
            }
        }
        WM_MOUSELEAVE => {
            if set_hover(None) {
                set_animating(hwnd, true);
            }
        }
        WM_SETCURSOR => unsafe {
            let over = PILL.with(|p| p.try_borrow().ok().is_some_and(|g| g.as_ref().is_some_and(|s| s.hover.is_some())));
            let _ = SetCursor(LoadCursorW(None, if over { IDC_HAND } else { IDC_ARROW }).ok());
            return LRESULT(1);
        },
        WM_LBUTTONUP => {
            let (x, y) = point(lp);
            let target = PILL.with(|p| {
                p.try_borrow().ok().and_then(|g| g.as_ref().and_then(|s| s.layout.hit(x, y).map(|b| (s.owner, s.msg, b))))
            });
            if let Some((owner, msg, button)) = target {
                unsafe {
                    let _ = PostMessageW(Some(owner), msg, WPARAM(button.wparam()), LPARAM(0));
                }
            }
        }
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
    LRESULT(0)
}

unsafe extern "system" fn count_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wp.0 == COUNT_TIMER_ID => render_count(),
        WM_PAINT => unsafe {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            render_count();
        },
        WM_ERASEBKGND => return LRESULT(1),
        WM_LBUTTONUP => {
            let target = COUNT.with(|c| c.try_borrow().ok().and_then(|g| g.as_ref().map(|s| (s.owner, s.msg))));
            if let Some((owner, msg)) = target {
                unsafe {
                    let _ = PostMessageW(Some(owner), msg, WPARAM(DISCARD), LPARAM(0));
                }
            }
        }
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
    LRESULT(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: Rect = Rect { x: 0, y: 0, w: 3440, h: 1440 };
    const LEFT: Rect = Rect { x: -1920, y: 540, w: 1920, h: 1080 };
    const TOP: Rect = Rect { x: 365, y: -1440, w: 2560, h: 1440 };

    fn c(x: i32, y: i32, right: bool, bottom: bool, monitor: usize) -> Corner {
        Corner { x, y, right, bottom, monitor }
    }
    fn s(horizontal: bool, far: bool, at: i32, from: i32, to: i32) -> Side {
        Side { horizontal, far, at, from, to }
    }

    #[test]
    fn one_monitor() {
        let (corners, sides) = frame(Rect { x: 100, y: 100, w: 200, h: 100 }, &[MAIN], 10);
        let tl = c(100, 100, false, false, 0);
        assert_eq!(corners, [tl, c(300, 100, true, false, 0), c(100, 200, false, true, 0), c(300, 200, true, true, 0)]);
        assert_eq!(
            sides,
            [
                s(true, false, 100, 110, 290),
                s(true, true, 200, 110, 290),
                s(false, false, 100, 110, 190),
                s(false, true, 300, 110, 190),
            ]
        );
    }

    #[test]
    fn pushed_inside_the_monitor() {
        let (corners, sides) = frame(Rect { x: -5, y: -5, w: 100, h: 100 }, &[MAIN], 10);
        assert_eq!(corners[0], c(1, 1, false, false, 0));
        assert_eq!(corners[3], c(95, 95, true, true, 0));
        assert_eq!(sides[0], s(true, false, 1, 11, 85));
    }

    #[test]
    fn side_by_side_with_a_gap_corner() {
        let (corners, sides) = frame(Rect { x: -1000, y: 600, w: 2000, h: 900 }, &[MAIN, LEFT, TOP], 10);
        let (tl, bl) = (c(-1000, 600, false, false, 1), c(-1000, 1500, false, true, 1));
        assert_eq!(corners, [tl, c(1000, 600, true, false, 0), bl, c(1000, 1439, true, true, 0)]);
        assert_eq!(
            sides,
            [
                s(true, false, 600, 1, 990),
                s(true, true, 1439, 1, 990),
                s(false, true, 1000, 610, 1429),
                s(true, false, 600, -990, -1),
                s(true, true, 1500, -990, -1),
                s(false, false, -1000, 610, 1490),
                s(false, true, -1, 1440, 1500),
            ]
        );
    }

    #[test]
    fn stacked_monitors_share_no_edge() {
        let (corners, sides) = frame(Rect { x: 1000, y: -500, w: 1000, h: 1000 }, &[MAIN, LEFT, TOP], 10);
        let (tl, tr) = (c(1000, -500, false, false, 2), c(2000, -500, true, false, 2));
        assert_eq!(corners, [tl, tr, c(1000, 500, false, true, 0), c(2000, 500, true, true, 0)]);
        assert_eq!(
            sides,
            [
                s(true, true, 500, 1010, 1990),
                s(false, false, 1000, 1, 490),
                s(false, true, 2000, 1, 490),
                s(true, false, -500, 1010, 1990),
                s(false, false, 1000, -490, -1),
                s(false, true, 2000, -490, -1),
            ]
        );
    }

    #[test]
    fn subtract_spans() {
        assert_eq!(subtract(&[(0, 10)], (3, 5)), [(0, 3), (5, 10)]);
        assert_eq!(subtract(&[(0, 10)], (-5, 5)), [(5, 10)]);
        assert_eq!(subtract(&[(0, 10)], (10, 20)), [(0, 10)]);
        assert_eq!(subtract(&[(0, 4), (6, 10)], (2, 8)), [(0, 2), (8, 10)]);
        assert_eq!(subtract(&[(0, 10)], (0, 10)), []);
    }

    #[test]
    fn only_pieces_over_the_region_are_excluded_or_hidden() {
        let regions = [Rect { x: 100, y: 100, w: 200, h: 100 }];
        let (hide, show) = (IndicatorInside::Hide, IndicatorInside::Show);
        for inside in [hide, show] {
            assert_eq!(shown(Rect { x: 0, y: 0, w: 50, h: 50 }, &regions, inside), Shown::Plain);
            assert_eq!(shown(Rect { x: 300, y: 100, w: 10, h: 10 }, &regions, inside), Shown::Plain);
            assert_eq!(shown(Rect { x: 290, y: 190, w: 20, h: 20 }, &[], inside), Shown::Plain);
        }
        assert_eq!(shown(Rect { x: 290, y: 190, w: 20, h: 20 }, &regions, hide), Shown::Hidden);
        assert_eq!(shown(Rect { x: 290, y: 190, w: 20, h: 20 }, &regions, show), Shown::Excluded);
    }

    #[test]
    fn a_bracket_pushed_inside_keeps_its_arm_outside() {
        let regions = [Rect { x: 0, y: 100, w: 400, h: 300 }];
        let pos = Rect { x: 0, y: 92, w: 22, h: 22 };
        let (h, v) = (Rect { x: 1, y: 1, w: 20, h: 3 }, Rect { x: 1, y: 1, w: 3, h: 20 });
        assert_eq!(kept_parts(pos, &[h, v], &regions, IndicatorInside::Hide), (vec![h], false));
        assert_eq!(kept_parts(pos, &[h, v], &regions, IndicatorInside::Show), (vec![h, v], true));
        let away = [Rect { x: 100, y: 200, w: 10, h: 10 }];
        assert_eq!(kept_parts(pos, &[h, v], &away, IndicatorInside::Hide), (vec![h, v], false));
    }

    #[test]
    fn the_pill_finds_a_spot_outside_the_region() {
        let spot = |capture: Rect, screens: &[Rect], inside| pill_spot(capture, capture.inflate(7), MAIN, screens, (150, 32), 6, inside);
        let hide = IndicatorInside::Hide;
        let pill = |x, y| Rect { x, y, w: 150, h: 32 };
        // Above the frame, else below it, else beside it.
        assert_eq!(spot(Rect { x: 100, y: 200, w: 400, h: 300 }, &[MAIN], hide), (pill(93, 154), Shown::Plain));
        assert_eq!(spot(Rect { x: 100, y: 0, w: 400, h: 300 }, &[MAIN], hide), (pill(93, 314), Shown::Plain));
        assert_eq!(spot(Rect { x: 400, y: 0, w: 3040, h: 1440 }, &[MAIN], hide), (pill(236, 1), Shown::Plain));
        // A whole monitor: on a neighbor, else inside per the setting.
        assert_eq!(spot(MAIN, &[MAIN, LEFT, TOP], hide), (pill(-150, 540), Shown::Plain));
        assert_eq!(spot(MAIN, &[MAIN], hide), (pill(12, 12), Shown::Hidden));
        assert_eq!(spot(MAIN, &[MAIN], IndicatorInside::Show), (pill(12, 12), Shown::Excluded));
    }
}
