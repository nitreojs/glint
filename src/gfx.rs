//! Drawing helpers shared by the overlay and the recording indicator: 32-bit DIB surfaces,
//! Direct2D/DirectWrite rendering (anti-aliased pills and text) into DIBs or straight into
//! DWM glass windows, and vsync-paced fades.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F,
    D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::{DwmExtendFrameIntoClientArea, DwmFlush};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::core::w;
use windows_numerics::Vector2;

use crate::util::wide;

/// Top-down 32-bit DIB selected into its own memory DC.
pub struct Dib {
    pub dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    pub bits: *mut u32,
    pub w: i32,
    pub h: i32,
}

impl Dib {
    pub fn new(w: i32, h: i32) -> Result<Dib, String> {
        unsafe {
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let screen = GetDC(None);
            let mut bits = std::ptr::null_mut();
            let bmp = CreateDIBSection(Some(screen), &bmi, DIB_RGB_COLORS, &mut bits, None, 0);
            let dc = CreateCompatibleDC(Some(screen));
            ReleaseDC(None, screen);
            let bmp = bmp.map_err(|e| {
                let _ = DeleteDC(dc);
                format!("CreateDIBSection {w}x{h}: {e}")
            })?;
            let old = SelectObject(dc, bmp.into());
            Ok(Dib { dc, bmp, old, bits: bits as *mut u32, w, h })
        }
    }

    pub fn pixels(&self) -> &[u32] {
        unsafe { std::slice::from_raw_parts(self.bits, (self.w * self.h) as usize) }
    }

    pub fn pixels_mut(&mut self) -> &mut [u32] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize) }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

pub const fn rgba(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a }
}

pub struct Painter {
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    /// Draws over opaque images (screenshots); destination alpha is ignored. Software
    /// rendered: these surfaces are small, and a GPU DC target pays a readback per draw.
    dc_target: ID2D1DCRenderTarget,
    /// Physical pixels per 96-DPI pixel.
    pub scale: f32,
}

impl Painter {
    pub fn new() -> Result<Painter, String> {
        unsafe {
            let factory: ID2D1Factory =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).map_err(|e| format!("D2D1CreateFactory: {e}"))?;
            let dwrite: IDWriteFactory =
                DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).map_err(|e| format!("DWriteCreateFactory: {e}"))?;
            let dc_target = factory
                .CreateDCRenderTarget(&D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    usage: D2D1_RENDER_TARGET_USAGE_NONE,
                    minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
                })
                .map_err(|e| format!("CreateDCRenderTarget: {e}"))?;
            Ok(Painter { factory, dwrite, dc_target, scale: GetDpiForSystem() as f32 / 96.0 })
        }
    }

    /// Scales a 96-DPI length to physical pixels.
    pub fn px(&self, v: f32) -> f32 {
        (v * self.scale).round()
    }

    /// Segoe UI Semibold at `size` 96-DPI pixels, single line, vertically centered.
    pub fn font(&self, size: f32) -> Result<IDWriteTextFormat, String> {
        unsafe {
            let f = self
                .dwrite
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_SEMI_BOLD,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size * self.scale,
                    w!("en-us"),
                )
                .map_err(|e| format!("CreateTextFormat: {e}"))?;
            let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
            let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            Ok(f)
        }
    }

    /// Rendered width and line height of `text`.
    pub fn measure(&self, text: &str, font: &IDWriteTextFormat) -> (f32, f32) {
        let s = wide(text);
        unsafe {
            let Ok(layout) = self.dwrite.CreateTextLayout(&s[..s.len() - 1], font, 10_000.0, 10_000.0) else {
                return (0.0, 0.0);
            };
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = layout.GetMetrics(&mut m);
            (m.widthIncludingTrailingWhitespace, m.height)
        }
    }

    /// Draws onto an opaque DIB (e.g. a copy of the screenshot behind a pill).
    pub fn draw(&self, dib: &Dib, f: impl FnOnce(&Canvas)) -> Result<(), String> {
        let rt = &self.dc_target;
        unsafe {
            rt.BindDC(dib.dc, &RECT { left: 0, top: 0, right: dib.w, bottom: dib.h }).map_err(|e| format!("BindDC: {e}"))?;
        }
        Self::paint(rt, false, 1.0, f)
    }

    /// A render target drawing straight into `hwnd` with per-pixel alpha. The window must
    /// be a DWM "glass" window (see [`make_glass`]) for the alpha to show through.
    pub fn window_target(&self, hwnd: HWND, w: i32, h: i32) -> Result<ID2D1HwndRenderTarget, String> {
        unsafe {
            let rt = self
                .factory
                .CreateHwndRenderTarget(
                    &D2D1_RENDER_TARGET_PROPERTIES {
                        pixelFormat: D2D1_PIXEL_FORMAT {
                            format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                        },
                        dpiX: 96.0,
                        dpiY: 96.0,
                        ..Default::default()
                    },
                    &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                        hwnd,
                        pixelSize: D2D_SIZE_U { width: w as u32, height: h as u32 },
                        presentOptions: D2D1_PRESENT_OPTIONS_NONE,
                    },
                )
                .map_err(|e| format!("CreateHwndRenderTarget: {e}"))?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            Ok(rt)
        }
    }

    /// Redraws a window target from scratch; every color's alpha is scaled by `fade`.
    pub fn render(&self, rt: &ID2D1HwndRenderTarget, fade: f32, f: impl FnOnce(&Canvas)) -> Result<(), String> {
        Self::paint(rt, true, fade, f)
    }

    fn paint(rt: &ID2D1RenderTarget, clear: bool, fade: f32, f: impl FnOnce(&Canvas)) -> Result<(), String> {
        unsafe {
            let brush = rt.CreateSolidColorBrush(&rgba(0, 0, 0, 0.0), None).map_err(|e| format!("brush: {e}"))?;
            rt.BeginDraw();
            if clear {
                rt.Clear(Some(&rgba(0, 0, 0, 0.0)));
            }
            f(&Canvas { rt, brush, fade });
            rt.EndDraw(None, None).map_err(|e| format!("EndDraw: {e}"))
        }
    }
}

/// Turns a borderless popup into a DWM "glass" surface: its client area is composited with
/// per-pixel alpha. Unlike layered windows, these can still be excluded from capture.
pub fn make_glass(hwnd: HWND) {
    let all = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
    unsafe {
        let _ = DwmExtendFrameIntoClientArea(hwnd, &all);
    }
}

pub struct Canvas<'a> {
    rt: &'a ID2D1RenderTarget,
    brush: ID2D1SolidColorBrush,
    fade: f32,
}

fn rect_f(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

impl Canvas<'_> {
    fn color(&self, c: D2D1_COLOR_F) {
        unsafe { self.brush.SetColor(&D2D1_COLOR_F { a: c.a * self.fade, ..c }) };
    }

    pub fn round_rect(&self, x: f32, y: f32, w: f32, h: f32, radius: f32, color: D2D1_COLOR_F) {
        self.color(color);
        unsafe {
            let rr = D2D1_ROUNDED_RECT { rect: rect_f(x, y, w, h), radiusX: radius, radiusY: radius };
            self.rt.FillRoundedRectangle(&rr, &self.brush);
        }
    }

    pub fn line(&self, (x0, y0): (f32, f32), (x1, y1): (f32, f32), width: f32, color: D2D1_COLOR_F) {
        self.color(color);
        unsafe {
            self.rt.DrawLine(Vector2 { X: x0, Y: y0 }, Vector2 { X: x1, Y: y1 }, &self.brush, width, None);
        }
    }

    /// Draws `text` left-aligned and vertically centered in the box `[x, y, w, h]`.
    pub fn text(&self, text: &str, font: &IDWriteTextFormat, [x, y, w, h]: [f32; 4], color: D2D1_COLOR_F) {
        let s = wide(text);
        unsafe {
            self.color(color);
            self.rt.DrawText(
                &s[..s.len() - 1],
                font,
                &rect_f(x, y, w, h),
                &self.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }
}

pub fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// Runs `step(progress)` once per display refresh for `ms` milliseconds (progress 0..=1,
/// eased), blocking the calling thread.
pub fn animate(ms: f32, mut step: impl FnMut(f32)) {
    let start = std::time::Instant::now();
    loop {
        let t = (start.elapsed().as_secs_f32() * 1000.0 / ms).min(1.0);
        step(ease_out(t));
        if t >= 1.0 {
            break;
        }
        if unsafe { DwmFlush() }.is_err() {
            std::thread::sleep(std::time::Duration::from_millis(7));
        }
    }
}
