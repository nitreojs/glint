use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_CLOSED,
    D2D1_FIGURE_END_OPEN,
};
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::{DWRITE_MEASURING_MODE_NATURAL, IDWriteTextFormat};
use windows_numerics::Vector2;

use crate::platform::wide::wide;

pub(crate) struct Canvas<'a> {
    pub(super) rt: &'a ID2D1RenderTarget,
    pub(super) brush: ID2D1SolidColorBrush,
    pub(super) fade: f32,
    pub(super) factory: &'a ID2D1Factory,
    /// Round caps and joins, for strokes drawn by hand.
    pub(super) round: ID2D1StrokeStyle,
}

fn rect_f(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

impl Canvas<'_> {
    fn color(&self, c: D2D1_COLOR_F) {
        unsafe { self.brush.SetColor(&D2D1_COLOR_F { a: c.a * self.fade, ..c }) };
    }

    pub(crate) fn round_rect(&self, x: f32, y: f32, w: f32, h: f32, radius: f32, color: D2D1_COLOR_F) {
        self.color(color);
        unsafe {
            let rr = D2D1_ROUNDED_RECT { rect: rect_f(x, y, w, h), radiusX: radius, radiusY: radius };
            self.rt.FillRoundedRectangle(&rr, &self.brush);
        }
    }

    pub(crate) fn line(&self, (x0, y0): (f32, f32), (x1, y1): (f32, f32), width: f32, color: D2D1_COLOR_F) {
        self.color(color);
        unsafe {
            self.rt.DrawLine(Vector2 { X: x0, Y: y0 }, Vector2 { X: x1, Y: y1 }, &self.brush, width, None);
        }
    }

    /// An open path through `points` with round caps and joins, drawn as one shape so a
    /// translucent stroke doesn't darken where it overlaps itself.
    pub(crate) fn polyline(&self, points: &[(f32, f32)], width: f32, color: D2D1_COLOR_F) {
        if let Some(path) = self.path(points, false) {
            self.color(color);
            unsafe { self.rt.DrawGeometry(&path, &self.brush, width, &self.round) };
        }
    }

    /// A filled closed polygon.
    pub(crate) fn polygon(&self, points: &[(f32, f32)], color: D2D1_COLOR_F) {
        if let Some(path) = self.path(points, true) {
            self.color(color);
            unsafe { self.rt.FillGeometry(&path, &self.brush, None) };
        }
    }

    fn path(&self, points: &[(f32, f32)], closed: bool) -> Option<ID2D1PathGeometry> {
        let (&(x0, y0), rest) = points.split_first()?;
        unsafe {
            let path = self.factory.CreatePathGeometry().ok()?;
            let sink = path.Open().ok()?;
            let begin = if closed { D2D1_FIGURE_BEGIN_FILLED } else { D2D1_FIGURE_BEGIN_HOLLOW };
            sink.BeginFigure(Vector2 { X: x0, Y: y0 }, begin);
            if rest.is_empty() {
                sink.AddLine(Vector2 { X: x0, Y: y0 }); // a dot: the round caps draw it
            }
            for &(x, y) in rest {
                sink.AddLine(Vector2 { X: x, Y: y });
            }
            sink.EndFigure(if closed { D2D1_FIGURE_END_CLOSED } else { D2D1_FIGURE_END_OPEN });
            sink.Close().ok()?;
            Some(path)
        }
    }

    /// Rectangle outline whose stroke is centred on the edges.
    pub(crate) fn rect_outline(&self, x: f32, y: f32, w: f32, h: f32, width: f32, color: D2D1_COLOR_F) {
        self.color(color);
        unsafe { self.rt.DrawRectangle(&rect_f(x, y, w, h), &self.brush, width, None) };
    }

    pub(crate) fn circle(&self, (x, y): (f32, f32), r: f32, color: D2D1_COLOR_F) {
        self.color(color);
        let e = D2D1_ELLIPSE { point: Vector2 { X: x, Y: y }, radiusX: r, radiusY: r };
        unsafe { self.rt.FillEllipse(&e, &self.brush) };
    }

    pub(crate) fn ring(&self, (x, y): (f32, f32), r: f32, width: f32, color: D2D1_COLOR_F) {
        self.color(color);
        let e = D2D1_ELLIPSE { point: Vector2 { X: x, Y: y }, radiusX: r, radiusY: r };
        unsafe { self.rt.DrawEllipse(&e, &self.brush, width, None) };
    }

    /// Draws `text` left-aligned and vertically centered in the box `[x, y, w, h]`.
    pub(crate) fn text(&self, text: &str, font: &IDWriteTextFormat, [x, y, w, h]: [f32; 4], color: D2D1_COLOR_F) {
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
