use glint_core::geom::outline::{inward, outer};
use glint_core::geom::rect::Rect;
use glint_core::log::log;
use glint_media::rect::WinRect;
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
use glint_shell::gfx::{Painter, make_glass};
use crate::platform::rect::ring;

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
    /// One per chosen rect, then the selection's; the chosen ones share an outline, the selection's
    /// stays whole over them.
    outlines: Vec<Window>,
    lit: (Vec<Rect>, Option<Rect>),
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
        unsafe {
            let _ = SetLayeredWindowAttributes(dim.0, COLORREF(0), 0, LWA_ALPHA);
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
        let size = (area.w, area.h);
        Ok(Live { origin, size, dim, outlines: Vec::new(), lit: (Vec::new(), None), hints: pills, label, fade: 0.0 })
    }

    pub(super) fn set_fade(&mut self, t: f32, painter: &Painter, font: &IDWriteTextFormat) {
        self.fade = t;
        unsafe {
            let _ = SetLayeredWindowAttributes(self.dim.0, COLORREF(0), (t * DIM_ALPHA) as u8, LWA_ALPHA);
            for o in &self.outlines {
                let _ = SetLayeredWindowAttributes(o.0, COLORREF(0), (t * 255.0) as u8, LWA_ALPHA);
            }
        }
        for g in self.hints.iter().chain([&self.label]) {
            g.render(painter, font, t);
        }
    }

    /// Shows `chosen` and `sel` lit, with their outlines on `monitors` (client coordinates).
    pub(super) fn sync(
        &mut self,
        chosen: &[Rect],
        sel: Option<Rect>,
        label: Option<&Pill>,
        monitors: &[Rect],
        painter: &Painter,
        font: &IDWriteTextFormat,
    ) {
        if (chosen, sel) != (self.lit.0.as_slice(), self.lit.1) {
            self.lit = (chosen.to_vec(), sel);
            let lit: Vec<Rect> = chosen.iter().copied().chain(sel).collect();
            // outline windows: around each rect, or inside it on sides at the edge of its monitors
            let rings: Vec<Rect> = lit.iter().map(|&l| outer(l, inward(l, monitors, RING), RING)).collect();
            while self.outlines.len() < lit.len() {
                let at = Rect { x: self.origin.0, y: self.origin.1, w: 1, h: 1 };
                match popup(at, WS_EX_LAYERED | WS_EX_TRANSPARENT, OUTLINE) {
                    Ok(o) => {
                        unsafe {
                            let _ = SetLayeredWindowAttributes(o.0, COLORREF(0), (self.fade * 255.0) as u8, LWA_ALPHA);
                        }
                        self.outlines.push(o);
                    }
                    Err(e) => {
                        log(&format!("picker outline failed: {e}"));
                        break;
                    }
                }
            }
            unsafe {
                let shape = CreateRectRgn(0, 0, self.size.0, self.size.1);
                for &r in &rings {
                    cut(shape, r);
                }
                SetWindowRgn(self.dim.0, Some(shape), true);
                let _ = UpdateWindow(self.dim.0);
                for (i, o) in self.outlines.iter().enumerate() {
                    let Some(&h) = rings.get(i) else {
                        let _ = ShowWindow(o.0, SW_HIDE);
                        continue;
                    };
                    let shape = CreateRectRgn(0, 0, h.w, h.h);
                    let is_sel = i == chosen.len();
                    let lit_inside = if is_sel { &rings[i..] } else { &rings[..chosen.len()] };
                    for other in lit_inside {
                        cut(shape, other.inflate(-RING).offset(-h.x, -h.y));
                    }
                    if let Some(&s) = rings.get(chosen.len()).filter(|_| !is_sel) {
                        cut(shape, s.offset(-h.x, -h.y));
                    }
                    SetWindowRgn(o.0, Some(shape), false);
                    // Below the shade (they never overlap), above the screen and under the pills.
                    let flags = SWP_NOACTIVATE | SWP_SHOWWINDOW;
                    let (x, y) = (self.origin.0 + h.x, self.origin.1 + h.y);
                    let _ = SetWindowPos(o.0, Some(self.dim.0), x, y, h.w, h.h, flags);
                    let _ = UpdateWindow(o.0);
                }
            }
        }
        let at = label.map(|l| (l.rect, l.text.as_str()));
        self.label.place(at, self.origin, painter, font, self.fade);
    }

    /// Takes the hint pills away for good.
    pub(super) fn drop_hints(&mut self) {
        self.hints.clear();
    }

    pub(super) fn hide(&self) {
        let glass = self.hints.iter().chain([&self.label]).map(|g| &g.window);
        for h in [&self.dim].into_iter().chain(&self.outlines).chain(glass) {
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
