use glint_core::wide::wide;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_SIZE_U, D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::HDC;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::core::w;

use super::{Canvas, Dib, rgba};

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

    /// IBM Plex Sans (the settings window's face) at `size` 96-DPI pixels, single line,
    /// vertically centered.
    pub fn plex(&self, weight: super::Weight, size: f32) -> Result<IDWriteTextFormat, String> {
        super::fonts::format(&self.dwrite, weight, size * self.scale)
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
        self.draw_on(dib.dc, RECT { left: 0, top: 0, right: dib.w, bottom: dib.h }, f)
    }

    pub fn draw_on(&self, hdc: HDC, r: RECT, f: impl FnOnce(&Canvas)) -> Result<(), String> {
        let rt = &self.dc_target;
        unsafe {
            rt.BindDC(hdc, &r).map_err(|e| format!("BindDC: {e}"))?;
        }
        self.paint(rt, false, 1.0, f)
    }

    /// A software target drawing with per-pixel alpha into DIBs, for layered windows
    /// (`UpdateLayeredWindow`): unlike [`Painter::window_target`] it has no GPU device to make,
    /// so a window drawn with it shows at once.
    pub fn alpha_target(&self) -> Result<ID2D1DCRenderTarget, String> {
        unsafe {
            let rt = self
                .factory
                .CreateDCRenderTarget(&D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    usage: D2D1_RENDER_TARGET_USAGE_NONE,
                    minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
                })
                .map_err(|e| format!("CreateDCRenderTarget: {e}"))?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            Ok(rt)
        }
    }

    /// Redraws `dib` from transparent through `rt` (an [`Painter::alpha_target`]).
    pub fn draw_alpha(&self, rt: &ID2D1DCRenderTarget, dib: &Dib, f: impl FnOnce(&Canvas)) -> Result<(), String> {
        unsafe {
            rt.BindDC(dib.dc, &RECT { left: 0, top: 0, right: dib.w, bottom: dib.h })
                .map_err(|e| format!("BindDC: {e}"))?;
        }
        self.paint(rt, true, 1.0, f)
    }

    /// A render target drawing straight into `hwnd` with per-pixel alpha. The window must
    /// be a DWM "glass" window (see [`make_glass`]) for the alpha to show through. Drawing
    /// doesn't wait for the display: callers pace themselves (see [`super::animate`]), and the
    /// thread is free between frames.
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
                        presentOptions: D2D1_PRESENT_OPTIONS_IMMEDIATELY,
                    },
                )
                .map_err(|e| format!("CreateHwndRenderTarget: {e}"))?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            Ok(rt)
        }
    }

    /// Redraws a window target from scratch; every color's alpha is scaled by `fade`.
    pub fn render(&self, rt: &ID2D1HwndRenderTarget, fade: f32, f: impl FnOnce(&Canvas)) -> Result<(), String> {
        self.paint(rt, true, fade, f)
    }

    fn paint(&self, rt: &ID2D1RenderTarget, clear: bool, fade: f32, f: impl FnOnce(&Canvas)) -> Result<(), String> {
        unsafe {
            let brush = rt.CreateSolidColorBrush(&rgba(0, 0, 0, 0.0), None).map_err(|e| format!("brush: {e}"))?;
            let round = self
                .factory
                .CreateStrokeStyle(
                    &D2D1_STROKE_STYLE_PROPERTIES {
                        startCap: D2D1_CAP_STYLE_ROUND,
                        endCap: D2D1_CAP_STYLE_ROUND,
                        dashCap: D2D1_CAP_STYLE_ROUND,
                        lineJoin: D2D1_LINE_JOIN_ROUND,
                        miterLimit: 10.0,
                        dashStyle: D2D1_DASH_STYLE_SOLID,
                        dashOffset: 0.0,
                    },
                    None,
                )
                .map_err(|e| format!("CreateStrokeStyle: {e}"))?;
            rt.BeginDraw();
            if clear {
                rt.Clear(Some(&rgba(0, 0, 0, 0.0)));
            }
            f(&Canvas { rt, brush, fade, factory: &self.factory, dwrite: &self.dwrite, round });
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
