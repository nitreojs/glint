use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U;
use windows::Win32::Graphics::Direct2D::ID2D1HwndRenderTarget;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, w};

use super::pill::{Pill, draw_pill};
use super::{DIM, RING, rgn};
use crate::platform::gfx::{Painter, make_glass};
use crate::platform::log::log;
use crate::platform::rect::{Rect, ring};

const DIM_ALPHA: f32 = (256 - DIM) as f32;

const SHADE: isize = 0;
const OUTLINE: isize = 1;
const GLASS: isize = 2;

struct Window(HWND);

impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}

struct Glass {
    rt: ID2D1HwndRenderTarget,
    window: Window,
    shown: Option<Rect>,
    text: String,
}

pub(super) struct Live {
    origin: (i32, i32),
    size: (i32, i32),
    dim: Window,
    outline: Window,
    hole: Option<Rect>,
    hints: Vec<Glass>,
    label: Glass,
    fade: f32,
}

impl Live {
    pub(super) fn new(
        area: Rect,
        hints: &[Rect],
        hint: &str,
        painter: &Painter,
        font: &IDWriteTextFormat,
    ) -> Result<Live, String> {
        register();
        let origin = (area.x, area.y);
        let layered = WS_EX_LAYERED | WS_EX_TRANSPARENT;
        let dim = popup(area, layered, SHADE)?;
        let outline = popup(Rect { w: 1, h: 1, ..area }, layered, OUTLINE)?;
        unsafe {
            let _ = SetLayeredWindowAttributes(dim.0, COLORREF(0), 0, LWA_ALPHA);
            let _ = SetLayeredWindowAttributes(outline.0, COLORREF(0), 0, LWA_ALPHA);
            let _ = ShowWindow(dim.0, SW_SHOWNOACTIVATE);
            let _ = UpdateWindow(dim.0);
        }
        let mut pills = Vec::with_capacity(hints.len());
        for &r in hints {
            let mut g = Glass::new(painter)?;
            g.place(Some((r, hint)), origin, painter, font, 0.0);
            pills.push(g);
        }
        let label = Glass::new(painter)?;
        Ok(Live { origin, size: (area.w, area.h), dim, outline, hole: None, hints: pills, label, fade: 0.0 })
    }

    pub(super) fn set_fade(&mut self, t: f32, painter: &Painter, font: &IDWriteTextFormat) {
        self.fade = t;
        unsafe {
            let _ = SetLayeredWindowAttributes(self.dim.0, COLORREF(0), (t * DIM_ALPHA) as u8, LWA_ALPHA);
            let _ = SetLayeredWindowAttributes(self.outline.0, COLORREF(0), (t * 255.0) as u8, LWA_ALPHA);
        }
        for g in self.hints.iter().chain([&self.label]) {
            g.render(painter, font, t);
        }
    }

    pub(super) fn sync(
        &mut self,
        sel: Option<Rect>,
        label: Option<&Pill>,
        painter: &Painter,
        font: &IDWriteTextFormat,
    ) {
        let hole = sel.map(|s| s.inflate(RING));
        if hole != self.hole {
            self.hole = hole;
            unsafe {
                let shape = CreateRectRgn(0, 0, self.size.0, self.size.1);
                if let Some(h) = hole {
                    cut(shape, h);
                }
                SetWindowRgn(self.dim.0, Some(shape), true);
                let _ = UpdateWindow(self.dim.0);
                match hole {
                    Some(h) => {
                        let shape = CreateRectRgn(0, 0, h.w, h.h);
                        cut(shape, Rect { x: 0, y: 0, w: h.w, h: h.h }.inflate(-RING));
                        SetWindowRgn(self.outline.0, Some(shape), false);
                        let flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW;
                        let (x, y) = (self.origin.0 + h.x, self.origin.1 + h.y);
                        let _ = SetWindowPos(self.outline.0, None, x, y, h.w, h.h, flags);
                        let _ = UpdateWindow(self.outline.0);
                    }
                    None => {
                        let _ = ShowWindow(self.outline.0, SW_HIDE);
                    }
                }
            }
        }
        let at = label.map(|l| (l.rect, l.text.as_str()));
        self.label.place(at, self.origin, painter, font, self.fade);
    }

    pub(super) fn hide(&self) {
        for h in [&self.dim, &self.outline, &self.label.window].into_iter().chain(self.hints.iter().map(|g| &g.window))
        {
            unsafe {
                let _ = ShowWindow(h.0, SW_HIDE);
            }
        }
    }
}

impl Glass {
    fn new(painter: &Painter) -> Result<Glass, String> {
        let window = popup(Rect { x: 0, y: 0, w: 1, h: 1 }, WINDOW_EX_STYLE(0), GLASS)?;
        make_glass(window.0);
        let rt = painter.window_target(window.0, 1, 1)?;
        Ok(Glass { rt, window, shown: None, text: String::new() })
    }

    fn place(
        &mut self,
        at: Option<(Rect, &str)>,
        origin: (i32, i32),
        painter: &Painter,
        font: &IDWriteTextFormat,
        fade: f32,
    ) {
        let Some((r, text)) = at else {
            if self.shown.take().is_some() {
                unsafe {
                    let _ = ShowWindow(self.window.0, SW_HIDE);
                }
            }
            return;
        };
        let resized = self.shown.is_none_or(|s| (s.w, s.h) != (r.w, r.h));
        let redraw = resized || self.text != text;
        if self.shown != Some(r) {
            let flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW;
            unsafe {
                let _ = SetWindowPos(self.window.0, None, origin.0 + r.x, origin.1 + r.y, r.w, r.h, flags);
            }
            self.shown = Some(r);
        }
        if resized && let Err(e) = unsafe { self.rt.Resize(&D2D_SIZE_U { width: r.w as u32, height: r.h as u32 }) } {
            log(&format!("picker pill resize failed: {e}"));
        }
        if redraw {
            text.clone_into(&mut self.text);
            self.render(painter, font, fade);
        }
    }

    fn render(&self, painter: &Painter, font: &IDWriteTextFormat, fade: f32) {
        let Some(r) = self.shown else { return };
        if let Err(e) =
            painter.render(&self.rt, fade, |c| draw_pill(c, painter, font, &self.text, r.w as f32, r.h as f32))
        {
            log(&format!("picker pill render failed: {e}"));
        }
    }
}

unsafe fn cut(shape: HRGN, hole: Rect) {
    unsafe {
        let h = rgn(hole);
        CombineRgn(Some(shape), Some(shape), Some(h), RGN_DIFF);
        let _ = DeleteObject(h.into());
    }
}

const CLASS: windows::core::PCWSTR = w!("glint_live");

fn register() {
    unsafe {
        let Ok(hinst) = GetModuleHandleW(None) else { return };
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc); // fails harmlessly when already registered
    }
}

fn popup(r: Rect, ex: WINDOW_EX_STYLE, role: isize) -> Result<Window, String> {
    unsafe {
        let hinst = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let hwnd = CreateWindowExW(
            ex | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            CLASS,
            w!("glint"),
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
        .map_err(|e| format!("CreateWindowEx: {e}"))?;
        let on = TRUE;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, &on as *const BOOL as _, 4);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, role);
        Ok(Window(hwnd))
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let role = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if role != GLASS {
                    FillRect(hdc, &ps.rcPaint, HBRUSH(GetStockObject(BLACK_BRUSH).0));
                }
                if role == OUTLINE {
                    let mut rc = RECT::default();
                    let _ = GetClientRect(hwnd, &mut rc);
                    let r = Rect::from_win(&rc);
                    for s in ring(r.inflate(-1), r.inflate(-2)) {
                        FillRect(hdc, &s, HBRUSH(GetStockObject(WHITE_BRUSH).0));
                    }
                }
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
