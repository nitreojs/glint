use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Gdi::{
    BLACK_BRUSH, BitBlt, CombineRgn, CreateRectRgn, DeleteObject, ExtSelectClipRgn, FillRect, GdiFlush, GetStockObject,
    HBRUSH, HDC, RGN_AND, RGN_OR, RestoreDC, SRCCOPY, SaveDC, WHITE_BRUSH,
};

use super::effects::apply_effect;
use super::marks::{GRIPS, Mark, draw_mark, edge_point, numbered};
use super::text::Typesetter;
use super::toolbar::{BAR_BG, WHITE};
use super::{Drag, Editor, hull};
use crate::picker::DIM;
use crate::platform::gfx::{Canvas, Dib, Painter, rgba};
use crate::platform::rect::{Rect, ring};

const GRIP_EDGE: D2D1_COLOR_F = rgba(20, 20, 24, 0.9);

impl Editor {
    /// Brings `view` and the pill's placement up to date. `bright` is the frozen screen. Returns
    /// the pixels a moved pill changes.
    pub(crate) fn render(
        &mut self,
        bright: &Dib,
        painter: &Painter,
        font: &IDWriteTextFormat,
    ) -> Result<Vec<Rect>, String> {
        if self.view_dirty {
            self.render_view(bright, painter, None)?;
            self.view_dirty = false;
            self.patch = None;
        } else if let Some(part) = self.patch.take() {
            self.render_view(bright, painter, Some(part))?;
        }
        let want = self.pill_text();
        if self.pill.as_ref().map(|(_, item, text)| (*item, text)) == want.as_ref().map(|(item, text)| (*item, text)) {
            return Ok(Vec::new());
        }
        let old = self.pill.take().map(|(r, ..)| r);
        self.pill = want.map(|(item, text)| (self.place_pill(item, &text, painter, font), item, text));
        Ok(old.into_iter().chain(self.pill.as_ref().map(|(r, ..)| *r)).collect())
    }

    /// Redraws the view, or only its `part` (client coordinates) when the base is still good.
    pub(super) fn render_view(&mut self, bright: &Dib, painter: &Painter, part: Option<Rect>) -> Result<(), String> {
        let s = self.sel;
        let editing = match &self.drag {
            Some(Drag::Edit { index, .. }) if self.marks[*index].is_effect() => Some(*index),
            _ => None,
        };
        let committed =
            || self.marks.iter().enumerate().filter(move |&(i, m)| m.is_effect() && Some(i) != editing).map(|(_, m)| m);
        let fresh = self.base.as_ref().is_some_and(|(r, fx, _)| *r == s && fx.iter().eq(committed()));
        if !fresh {
            let mut base = Dib::new(s.w, s.h)?;
            unsafe {
                let _ = BitBlt(base.dc, 0, 0, s.w, s.h, Some(bright.dc), s.x, s.y, SRCCOPY);
                let _ = GdiFlush();
            }
            for m in committed() {
                apply_effect(&mut base, m, s, self.scale);
            }
            if let Some(cut) = &self.cutout {
                let px = base.pixels_mut();
                rows_outside(s, cut, |y, x0, x1| {
                    px[index(s, y, x0)..index(s, y, x1)].iter_mut().for_each(|p| *p = dim(*p))
                });
            }
            self.base = Some((s, committed().cloned().collect(), base));
        }
        let (_, _, base) = self.base.as_ref().expect("built above");
        let mut r = s;
        if self.view.as_ref().is_none_or(|v| (v.w, v.h) != (s.w, s.h)) {
            self.view = Some(Dib::new(s.w, s.h)?);
        } else if let Some(part) = part.filter(|_| fresh) {
            let Some(p) = part.intersect(&s) else { return Ok(()) };
            r = p;
        }
        let l = r.offset(-s.x, -s.y);
        let view = self.view.as_mut().expect("allocated above");
        unsafe {
            let _ = BitBlt(view.dc, l.x, l.y, l.w, l.h, Some(base.dc), l.x, l.y, SRCCOPY);
            let _ = GdiFlush();
        }
        let drawing = match &self.drag {
            Some(Drag::Mark { mark, .. }) => Some(mark),
            _ => None,
        };
        for m in drawing.into_iter().chain(editing.map(|i| &self.marks[i])).filter(|m| m.is_effect()) {
            apply_effect(view, m, s, self.scale);
        }
        let (shapes, scale, fonts) = (self.marks.iter().chain(drawing), self.scale, &self.fonts);
        painter.draw_on(view.dc, l.to_win(), |c| draw_shapes(c, shapes, (-r.x as f32, -r.y as f32), scale, fonts))?;
        if let Some(cut) = &self.cutout {
            unsafe {
                let _ = GdiFlush();
            }
            let (src, dst) = (base.pixels(), view.pixels_mut());
            rows_outside(r, cut, |y, x0, x1| {
                let span = index(s, y, x0)..index(s, y, x1);
                dst[span.clone()].copy_from_slice(&src[span])
            });
        }
        Ok(())
    }

    /// The whole client area with every mark: what a re-pick shows.
    pub(crate) fn flattened(&self, bright: &Dib, painter: &Painter) -> Result<Dib, String> {
        let a = self.area;
        let mut out = Dib::new(a.w, a.h)?;
        unsafe {
            let _ = BitBlt(out.dc, 0, 0, a.w, a.h, Some(bright.dc), a.x, a.y, SRCCOPY);
            let _ = GdiFlush();
        }
        for m in self.marks.iter().filter(|m| m.is_effect()) {
            apply_effect(&mut out, m, a, self.scale);
        }
        painter
            .draw(&out, |c| draw_shapes(c, self.marks.iter(), (-a.x as f32, -a.y as f32), self.scale, &self.fonts))?;
        unsafe {
            let _ = GdiFlush();
        }
        Ok(out)
    }

    /// Paints the editor's part of `rp` over `dim`, composed off-screen so each window pixel is written once.
    pub(crate) fn paint(
        &mut self,
        hdc: HDC,
        rp: Rect,
        dim: &Dib,
        painter: &Painter,
        font: &IDWriteTextFormat,
    ) -> Result<(), String> {
        let extent = self.extent();
        let hit = extent[1..].iter().filter(|r| r.intersect(&rp).is_some());
        let area = hit.fold(extent[0].intersect(&rp), |a, r| Some(a.map_or(*r, |a| hull(a, *r))));
        let Some(area) = area else { return Ok(()) };
        let (bw, bh) = self.back.as_ref().map_or((0, 0), |b| (b.w, b.h));
        if bw < area.w || bh < area.h {
            self.back = Some(Dib::new(bw.max(area.w), bh.max(area.h))?);
        }
        let back = self.back.as_ref().expect("allocated above");
        unsafe {
            let _ = BitBlt(back.dc, 0, 0, area.w, area.h, Some(dim.dc), area.x, area.y, SRCCOPY);
            self.draw(back.dc, area, rp, painter, font)?;
            let clip = CreateRectRgn(0, 0, 0, 0);
            for r in &extent {
                let part = CreateRectRgn(r.x, r.y, r.right(), r.bottom());
                CombineRgn(Some(clip), Some(clip), Some(part), RGN_OR);
                let _ = DeleteObject(part.into());
            }
            let saved = SaveDC(hdc);
            ExtSelectClipRgn(hdc, Some(clip), RGN_AND);
            let _ = DeleteObject(clip.into());
            let _ = BitBlt(hdc, area.x, area.y, area.w, area.h, Some(back.dc), 0, 0, SRCCOPY);
            let _ = RestoreDC(hdc, saved);
        }
        Ok(())
    }

    /// Draws the editor onto `dc`, which shows client `area`; overlays not reaching `rp` are skipped.
    unsafe fn draw(
        &self,
        dc: HDC,
        area: Rect,
        rp: Rect,
        painter: &Painter,
        font: &IDWriteTextFormat,
    ) -> Result<(), String> {
        let local = |r: Rect| r.offset(-area.x, -area.y);
        let visible = |r: Rect| r.intersect(&rp).is_some();
        unsafe {
            let (white, black) = (HBRUSH(GetStockObject(WHITE_BRUSH).0), HBRUSH(GetStockObject(BLACK_BRUSH).0));
            let s = local(self.sel);
            if let Some(v) = &self.view {
                let _ = BitBlt(dc, s.x, s.y, s.w, s.h, Some(v.dc), 0, 0, SRCCOPY);
            }
            if self.selected {
                // 1 px white inside 1 px black, outside the shot, like the picker.
                for (outer, inner, brush) in [(s.inflate(1), s, white), (s.inflate(2), s.inflate(1), black)] {
                    for e in ring(outer, inner) {
                        FillRect(dc, &e, brush);
                    }
                }
                // Handles at the corners and edge midpoints.
                let hs = self.px(7.0) as i32;
                for e in GRIPS {
                    let (x, y) = edge_point(s, e);
                    let (x, y) = (x as i32, y as i32);
                    let outer = Rect { x: x - hs / 2 - 1, y: y - hs / 2 - 1, w: hs + 2, h: hs + 2 };
                    FillRect(dc, &outer.to_win(), black);
                    FillRect(dc, &outer.inflate(-1).to_win(), white);
                }
            }
            if let Some(m) = self.picked_mark() {
                if let Mark::Stroke { .. } | Mark::Text { .. } | Mark::Step { .. } = m {
                    dashed_box(dc, local(self.outline(m)), white, black, self.px(4.0) as i32);
                }
                if let Some(c) = self.caret_rect() {
                    FillRect(dc, &local(c).inflate(1).to_win(), black);
                    FillRect(dc, &local(c).to_win(), white);
                }
                let (r, edge) = (4.5 * self.scale, 1.5 * self.scale);
                for (_, q) in m.grips() {
                    let gb = self.grip_box(q);
                    if visible(gb) {
                        let at = (q.0 - gb.x as f32, q.1 - gb.y as f32);
                        painter.draw_on(dc, local(gb).to_win(), |c| {
                            c.circle(at, r + edge, GRIP_EDGE);
                            c.circle(at, r, WHITE);
                        })?;
                    }
                }
            }
            if visible(self.bar) {
                painter.draw_on(dc, local(self.bar).to_win(), |c| self.draw_bar(c))?;
            }
            if let Some((r, _, text)) = &self.pill
                && visible(*r)
            {
                let (w, h, pad) = (r.w as f32, r.h as f32, self.px(10.0));
                painter.draw_on(dc, local(*r).to_win(), |c| {
                    c.round_rect(0.0, 0.0, w, h, h / 2.0, BAR_BG);
                    c.text(text, font, [pad, 0.0, w - pad, h], WHITE);
                })?;
            }
        }
        Ok(())
    }

    /// The finished shot as 0xAARRGGBB rows, the client rect it covers, and whether its alpha is
    /// meaningful (a cutout: transparent outside the windows); otherwise the top byte is garbage.
    pub(crate) fn pixels(&mut self, bright: &Dib, painter: &Painter) -> Result<(Vec<u32>, Rect, bool), String> {
        self.commit();
        self.drag = None;
        self.render_view(bright, painter, None)?;
        let v = self.view.as_ref().expect("rendered above");
        let out = self.output_rect().intersect(&self.sel).unwrap_or(self.sel);
        let (x0, x1) = ((out.x - self.sel.x) as usize, (out.right() - self.sel.x) as usize);
        let rows = (out.y - self.sel.y) as usize..(out.bottom() - self.sel.y) as usize;
        let stride = v.w as usize;
        let mut px = Vec::with_capacity(out.w as usize * out.h as usize);
        for y in rows {
            px.extend_from_slice(&v.pixels()[y * stride + x0..y * stride + x1]);
        }
        let alpha = self.cutout.as_ref().map(|cut| cut_alpha(&mut px, out, cut)).is_some();
        Ok((px, out, alpha))
    }
}

/// Draws the shapes among `marks` (effects are skipped), numbering step markers in order.
fn draw_shapes<'a>(
    c: &Canvas,
    marks: impl Iterator<Item = &'a Mark>,
    shift: (f32, f32),
    scale: f32,
    fonts: &Typesetter,
) {
    for (m, step) in numbered(marks.filter(|m| !m.is_effect())) {
        draw_mark(c, m, shift, scale, fonts, step);
    }
}

fn dim(p: u32) -> u32 {
    let rb = (((p & 0x00FF_00FF) * DIM) >> 8) & 0x00FF_00FF;
    let g = (((p & 0x0000_FF00) * DIM) >> 8) & 0x0000_FF00;
    rb | g
}

/// Where client pixel `(x, y)` sits in rows showing `r`.
fn index(r: Rect, y: i32, x: i32) -> usize {
    (y - r.y) as usize * r.w as usize + (x - r.x) as usize
}

/// Calls `f(y, x0, x1)` for each run of pixels in `r` outside every cutout rect (client coordinates).
fn rows_outside(r: Rect, cutout: &[Rect], mut f: impl FnMut(i32, i32, i32)) {
    let mut spans: Vec<(i32, i32)> = Vec::new();
    for y in r.y..r.bottom() {
        spans.clear();
        spans.extend(cutout.iter().filter(|c| y >= c.y && y < c.bottom()).map(|c| (c.x, c.right())));
        spans.sort_unstable();
        let mut x = r.x;
        for &(a, b) in spans.iter().chain([(r.right(), r.right())].iter()) {
            let a = a.clamp(r.x, r.right());
            if a > x {
                f(y, x, a);
            }
            x = x.max(b.clamp(r.x, r.right()));
        }
    }
}

/// Opaque inside the cutout rects, fully transparent elsewhere; `px` shows `r`.
fn cut_alpha(px: &mut [u32], r: Rect, cutout: &[Rect]) {
    px.iter_mut().for_each(|p| *p |= 0xFF00_0000);
    rows_outside(r, cutout, |y, x0, x1| {
        px[index(r, y, x0)..index(r, y, x1)].iter_mut().for_each(|p| *p &= 0x00FF_FFFF)
    });
}

unsafe fn dashed_box(hdc: HDC, r: Rect, white: HBRUSH, black: HBRUSH, dash: i32) {
    let outer = r.inflate(1);
    let step = (2 * dash) as usize;
    unsafe {
        for e in ring(outer, r) {
            FillRect(hdc, &e, black);
        }
        for x in (outer.x..outer.right()).step_by(step) {
            let w = dash.min(outer.right() - x);
            for y in [outer.y, r.bottom()] {
                FillRect(hdc, &Rect { x, y, w, h: 1 }.to_win(), white);
            }
        }
        for y in (outer.y..outer.bottom()).step_by(step) {
            let h = dash.min(outer.bottom() - y);
            for x in [outer.x, r.right()] {
                FillRect(hdc, &Rect { x, y, w: 1, h }.to_win(), white);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cutout_keeps_only_the_windows_opaque() {
        let r = Rect { x: 10, y: 20, w: 6, h: 3 };
        let cut = [Rect { x: 8, y: 20, w: 4, h: 2 }, Rect { x: 13, y: 21, w: 2, h: 5 }];
        let mut px = vec![0x0012_3456; 18];
        cut_alpha(&mut px, r, &cut);
        let alpha: Vec<u32> = px.iter().map(|p| p >> 24).collect();
        #[rustfmt::skip]
        assert_eq!(alpha, [
            255, 255, 0, 0, 0, 0,
            255, 255, 0, 255, 255, 0,
            0, 0, 0, 255, 255, 0,
        ]);
        assert!(px.iter().all(|p| p & 0x00FF_FFFF == 0x12_3456), "colours stay");
    }

    #[test]
    fn overlapping_cutouts_leave_no_gaps() {
        let r = Rect { x: 0, y: 0, w: 10, h: 1 };
        let mut runs = Vec::new();
        rows_outside(r, &[Rect { x: 2, y: 0, w: 5, h: 1 }, Rect { x: 3, y: 0, w: 2, h: 1 }], |y, a, b| {
            runs.push((y, a, b))
        });
        assert_eq!(runs, [(0, 0, 2), (0, 7, 10)]);
        let mut runs = Vec::new();
        rows_outside(Rect { x: 5, y: 7, w: 4, h: 2 }, &[Rect { x: 6, y: 8, w: 1, h: 1 }], |y, a, b| {
            runs.push((y, a, b))
        });
        assert_eq!(runs, [(7, 5, 9), (8, 5, 6), (8, 7, 9)]);
    }
}
