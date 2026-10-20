//! On-screen recording indicator: viewfinder corner brackets around the recorded region and a
//! small floating pill with a pulsing red dot, the elapsed time and a stop button.
//!
//! Every window here is excluded from screen capture, so none of it appears in the video
//! even where it overlaps the region (e.g. when recording a whole monitor). On Windows 10
//! that rules out per-pixel-alpha layered windows (UpdateLayeredWindow), so:
//! * brackets are click-through constant-alpha layered windows shaped by a window region;
//! * the pill is a DWM glass popup that Direct2D draws into with per-pixel alpha.

use std::cell::RefCell;
use std::f32::consts::TAU;
use std::time::Instant;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, TRUE, WPARAM};
use windows::Win32::Graphics::Direct2D::ID2D1HwndRenderTarget;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, w};

use crate::platform::gfx::{Dib, Painter, animate, make_glass, rgba};
use crate::platform::log::log;
use crate::platform::rect::Rect;

const FADE_IN_MS: f32 = 160.0;
const FADE_OUT_MS: f32 = 120.0;
const PULSE_S: f32 = 1.6;
const TIMER_ID: usize = 1;
/// Fast timer that runs only while a hover transition is in flight.
const ANIM_TIMER_ID: usize = 2;
const HOVER_MS: f32 = 140.0;
/// Bracket colors (BGR): light brackets over dark content, dark over bright content.
const LIGHT: (COLORREF, COLORREF) = (COLORREF(0x00F4F4F4), COLORREF(0x00303030));
const DARK: (COLORREF, COLORREF) = (COLORREF(0x00181614), COLORREF(0x00B0B0B0));
/// Opacity of the solid corner brackets and of the dashed sides between them.
const CORNER_ALPHA: u8 = 255;
const DASH_ALPHA: u8 = 150;

pub(crate) struct Indicator {
    /// Brackets and dashed sides with their full opacity.
    marks: Vec<(HWND, u8)>,
    pill: HWND,
}

struct PillState {
    owner: HWND,
    msg: u32,
    painter: Painter,
    font: IDWriteTextFormat,
    rt: ID2D1HwndRenderTarget,
    layout: Layout,
    started: Instant,
    hover: Option<Button>,
    /// Hover progress of [stop, discard], 0 (idle) ..= 1 (hovered).
    hover_t: [f32; 2],
    last_frame: Instant,
    /// The fast timer and 1 ms timer resolution are active.
    animating: bool,
    fade: f32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Button {
    /// Stop and save.
    Stop,
    /// Stop and delete the file.
    Discard,
}

/// Pill geometry, in pixels: [pad | dot | gap | m:ss | gap | ■ | ✕ | pad]. The buttons are
/// square hit areas whose hover circles are concentric with the pill's rounded end.
struct Layout {
    w: f32,
    h: f32,
    dot: f32,
    text_x: f32,
    time_w: f32,
    button: f32,
    top: f32,
    stop_x: f32,
    discard_x: f32,
}

impl Layout {
    fn new(p: &Painter, time_w: f32) -> Layout {
        let (h, button, dot) = (p.px(28.0), p.px(22.0), p.px(8.0));
        let top = ((h - button) / 2.0).floor();
        let text_x = p.px(12.0) + dot + p.px(8.0);
        let stop_x = text_x + time_w + p.px(6.0);
        let discard_x = stop_x + button + p.px(1.0);
        Layout { w: discard_x + button + top, h, dot, text_x, time_w, button, top, stop_x, discard_x }
    }

    fn hit(&self, x: f32, y: f32) -> Option<Button> {
        if y < self.top || y >= self.top + self.button {
            return None;
        }
        [(Button::Stop, self.stop_x), (Button::Discard, self.discard_x)]
            .into_iter()
            .find(|&(_, bx)| x >= bx && x < bx + self.button)
            .map(|(b, _)| b)
    }
}

thread_local! {
    static PILL: RefCell<Option<PillState>> = const { RefCell::new(None) };
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
/// around each, at opacity `alpha` once faded in.
fn mark(pos: Rect, parts: &[Rect], alpha: u8) -> Option<(HWND, u8)> {
    if pos.w <= 2 || pos.h <= 2 || parts.is_empty() {
        return None;
    }
    let (fill, rim) = if luminance_under(pos) > 0.55 { DARK } else { LIGHT };
    let hwnd = popup(w!("glint_mark"), WS_EX_LAYERED | WS_EX_TRANSPARENT, pos)?;
    let dib = Dib::new(pos.w, pos.h).ok()?;
    unsafe {
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA);
        exclude_from_capture(hwnd);
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
    /// buttons post `msg` to `owner` with WPARAM 0 (stop and save) or 1 (discard). Fades in
    /// before returning.
    pub(crate) fn show(capture: Rect, monitors: &[Rect], owner: HWND, msg: u32) -> Result<Indicator, String> {
        let painter = Painter::new()?;
        let px = |v: f32| painter.px(v) as i32;
        register(w!("glint_mark"), Some(mark_proc), IDC_ARROW);
        register(w!("glint_pill"), Some(pill_proc), IDC_ARROW);

        // The frame sits just outside the region; where that is off the monitors it is pushed
        // inside (it is excluded from capture, so it never shows up in the video).
        let (arm, th, gap) = (px(20.0), px(3.0), px(4.0));
        let (dash, space, inset) = (px(8.0), px(6.0), arm + px(6.0));
        let outer = capture.inflate(gap + th);
        let (corners, sides) = frame(outer, monitors, inset);
        let Some(&anchor) = corners.first() else { return Err("the recorded region is on no monitor".into()) };
        let size = arm + 2; // + rim on both sides
        let mut marks = Vec::with_capacity(corners.len() + sides.len());
        for c in &corners {
            let x = if c.right { c.x + 1 - size } else { c.x - 1 };
            let y = if c.bottom { c.y + 1 - size } else { c.y - 1 };
            let near = |far: bool, len: i32| if far { size - 1 - len } else { 1 };
            let horizontal = Rect { x: near(c.right, arm), y: near(c.bottom, th), w: arm, h: th };
            let vertical = Rect { x: near(c.right, th), y: near(c.bottom, arm), w: th, h: arm };
            marks.extend(mark(Rect { x, y, w: size, h: size }, &[horizontal, vertical], CORNER_ALPHA));
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
            marks.extend(mark(pos, &parts, DASH_ALPHA));
        }

        let monitor = monitors[anchor.monitor];
        let f = outer.intersect(&monitor.inflate(-1)).unwrap_or(outer);

        let font = painter.font(13.0)?;
        let time_w = painter.measure("88:88", &font).0.ceil();
        let layout = Layout::new(&painter, time_w);
        let (pw, ph) = (layout.w.ceil() as i32, layout.h as i32);
        let (mut x, mut y) = (f.x, f.y - 1 - px(6.0) - ph);
        if y < monitor.y {
            y = f.bottom() + 1 + px(6.0);
        }
        if y + ph > monitor.bottom() {
            (x, y) = (capture.x + px(12.0), capture.y + px(12.0));
        }
        let pos = clamp_into(x, y, pw, ph, monitor);
        let pill = popup(w!("glint_pill"), WINDOW_EX_STYLE(0), pos).ok_or("can't create the indicator window")?;
        make_glass(pill);
        exclude_from_capture(pill);
        let rt = painter.window_target(pill, pw, ph)?;
        PILL.with(|p| {
            *p.borrow_mut() = Some(PillState {
                owner,
                msg,
                painter,
                font,
                rt,
                layout,
                started: Instant::now(),
                hover: None,
                hover_t: [0.0; 2],
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
        animate(FADE_IN_MS, |t| set_fade(&marks, t));
        Ok(Indicator { marks, pill })
    }
}

impl Drop for Indicator {
    fn drop(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.pill), TIMER_ID);
        }
        animate(FADE_OUT_MS, |t| set_fade(&self.marks, 1.0 - t));
        if let Some(st) = PILL.with(|p| p.borrow_mut().take())
            && st.animating
        {
            unsafe {
                timeEndPeriod(1);
            }
        }
        unsafe {
            for h in self.marks.iter().map(|m| m.0).chain([self.pill]) {
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
    PILL.with(|p| {
        if let Ok(mut g) = p.try_borrow_mut()
            && let Some(st) = g.as_mut()
        {
            st.fade = fade;
        }
    });
    render_pill();
}

/// Moves hover progress toward the hovered button; returns whether anything is still moving.
fn advance_hover(st: &mut PillState) -> bool {
    let now = Instant::now();
    let step = (now - st.last_frame).as_secs_f32() * 1000.0 / HOVER_MS;
    st.last_frame = now;
    let mut moving = false;
    for (t, button) in st.hover_t.iter_mut().zip([Button::Stop, Button::Discard]) {
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
        let t = st.started.elapsed().as_secs_f32();
        let secs = t as u64;
        let clock = format!("{}:{:02}", secs / 60, secs % 60);
        let pulse = 0.6 + 0.4 * (t * TAU / PULSE_S).cos();
        let white = |a: f32| rgba(255, 255, 255, a);
        let _ = st.painter.render(&st.rt, st.fade, |c| {
            c.round_rect(0.0, 0.0, l.w, l.h, l.h / 2.0, rgba(16, 16, 20, 0.88));
            c.round_rect(px(12.0), (l.h - l.dot) / 2.0, l.dot, l.dot, l.dot / 2.0, rgba(255, 69, 58, pulse));
            c.text(&clock, &st.font, [l.text_x, 0.0, l.time_w + 2.0, l.h], white(0.96));
            for ((button, x), &h) in [(Button::Stop, l.stop_x), (Button::Discard, l.discard_x)].into_iter().zip(&st.hover_t) {
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
                    let _ = PostMessageW(Some(owner), msg, WPARAM((button == Button::Discard) as usize), LPARAM(0));
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
}
