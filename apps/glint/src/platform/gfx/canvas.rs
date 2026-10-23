use glint_core::wide::wide;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED,
    D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_OVERHANG_METRICS, IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout,
};
use windows_numerics::Vector2;

pub(crate) struct Canvas<'a> {
    pub(super) rt: &'a ID2D1RenderTarget,
    pub(super) brush: ID2D1SolidColorBrush,
    pub(super) fade: f32,
    pub(super) factory: &'a ID2D1Factory,
    pub(super) dwrite: &'a IDWriteFactory,
    /// Round caps and joins, for strokes drawn by hand.
    pub(super) round: ID2D1StrokeStyle,
}

fn rect_f(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

const LAYOUT_BOX: f32 = 1024.0;

// overhang metrics are distances from the layout box (max width × max height) to the ink
fn ink_from_overhang(o: &DWRITE_OVERHANG_METRICS, (w, h): (f32, f32)) -> [f32; 4] {
    [-o.left, -o.top, w + o.right, h + o.bottom]
}

fn centred_origin((cx, cy): (f32, f32), [l, t, r, b]: [f32; 4]) -> (f32, f32) {
    (cx - (l + r) / 2.0, cy - (t + b) / 2.0)
}

fn ink_box(layout: &IDWriteTextLayout) -> Option<[f32; 4]> {
    unsafe {
        let o = layout.GetOverhangMetrics().ok()?;
        Some(ink_from_overhang(&o, (layout.GetMaxWidth(), layout.GetMaxHeight())))
    }
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

    pub(crate) fn fill(&self, x: f32, y: f32, w: f32, h: f32, color: D2D1_COLOR_F) {
        self.color(color);
        unsafe { self.rt.FillRectangle(&rect_f(x, y, w, h), &self.brush) };
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

    /// Draws `w`×`h` premultiplied 0xAARRGGBB pixels from `px` (rows `stride` pixels apart) into
    /// the box `[x, y, bw, bh]`, scaled to fit it.
    pub(crate) fn image(&self, px: &[u32], stride: usize, (w, h): (u32, u32), [x, y, bw, bh]: [f32; 4]) {
        if w == 0 || h == 0 || px.len() < (h as usize - 1) * stride + w as usize {
            return;
        }
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        unsafe {
            let size = D2D_SIZE_U { width: w, height: h };
            let Ok(bitmap) = self.rt.CreateBitmap(size, Some(px.as_ptr() as _), (stride * 4) as u32, &props) else {
                return;
            };
            let mode = D2D1_BITMAP_INTERPOLATION_MODE_LINEAR;
            self.rt.DrawBitmap(&bitmap, Some(&rect_f(x, y, bw, bh)), self.fade, mode, None);
        }
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

    pub(crate) fn text_layout(&self, layout: &IDWriteTextLayout, (x, y): (f32, f32), color: D2D1_COLOR_F) {
        self.color(color);
        unsafe { self.rt.DrawTextLayout(Vector2 { X: x, Y: y }, layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE) };
    }

    pub(crate) fn text_centred(&self, text: &str, font: &IDWriteTextFormat, centre: (f32, f32), color: D2D1_COLOR_F) {
        let s: Vec<u16> = text.encode_utf16().collect();
        let Ok(layout) = (unsafe { self.dwrite.CreateTextLayout(&s, font, LAYOUT_BOX, LAYOUT_BOX) }) else { return };
        if let Some(ink) = ink_box(&layout) {
            self.text_layout(&layout, centred_origin(centre, ink), color);
        }
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::Graphics::DirectWrite::*;
    use windows::core::w;

    use super::*;

    #[test]
    fn ink_box_is_centred_on_the_point() {
        let o = DWRITE_OVERHANG_METRICS { left: -10.0, top: -40.0, right: -900.0, bottom: -914.0 };
        let ink = ink_from_overhang(&o, (LAYOUT_BOX, LAYOUT_BOX));
        assert_eq!(ink, [10.0, 40.0, 124.0, 110.0]);
        assert_eq!(centred_origin((50.0, 50.0), ink), (-17.0, -25.0));
        let (x, y) = centred_origin((50.0, 50.0), ink);
        assert_eq!(((ink[0] + ink[2]) / 2.0 + x, (ink[1] + ink[3]) / 2.0 + y), (50.0, 50.0));
    }

    #[test]
    fn digit_ink_is_found_inside_the_line() {
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.unwrap();
        let font = unsafe {
            dwrite.CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                100.0,
                w!("en-us"),
            )
        }
        .unwrap();
        let s: Vec<u16> = "8".encode_utf16().collect();
        let layout = unsafe { dwrite.CreateTextLayout(&s, &font, LAYOUT_BOX, LAYOUT_BOX) }.unwrap();
        let [l, t, r, b] = ink_box(&layout).unwrap();
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut m) }.unwrap();
        assert!(t > 10.0 && b < m.height, "the digit's ink sits inside its line box: {t} {b} {}", m.height);
        assert!((60.0..80.0).contains(&(b - t)), "a lining digit is about cap height: {}", b - t);
        assert!(l >= 0.0 && r <= m.width + 1.0 && r - l > 30.0, "{l} {r} {}", m.width);
    }
}
