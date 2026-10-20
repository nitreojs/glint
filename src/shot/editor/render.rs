use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Gdi::{BitBlt, FillRect, GdiFlush, HBRUSH, HDC, RectVisible, SRCCOPY};

use super::effects::apply_effect;
use super::marks::{GRIPS, Mark, draw_mark, edge_point};
use super::toolbar::{BAR_BG, WHITE};
use super::{Drag, Editor};
use crate::platform::gfx::{Dib, Painter, rgba};
use crate::platform::rect::{Rect, ring};

const GRIP_EDGE: D2D1_COLOR_F = rgba(20, 20, 24, 0.9);

impl Editor {
    /// Brings `view` and the pill's placement up to date. `bright` is the frozen screen.
    pub(crate) fn render(&mut self, bright: &Dib, painter: &Painter, font: &IDWriteTextFormat) -> Result<(), String> {
        if self.view_dirty {
            self.render_view(bright, painter)?;
            self.view_dirty = false;
        }
        let want = self.pill_text();
        if self.pill.as_ref().map(|(_, item, text)| (*item, text)) != want.as_ref().map(|(item, text)| (*item, text)) {
            self.pill = want.map(|(item, text)| (self.place_pill(item, &text, painter, font), item, text));
        }
        Ok(())
    }

    fn render_view(&mut self, bright: &Dib, painter: &Painter) -> Result<(), String> {
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
            self.base = Some((s, committed().cloned().collect(), base));
        }
        let (_, _, base) = self.base.as_ref().expect("built above");
        if self.view.as_ref().is_none_or(|v| (v.w, v.h) != (s.w, s.h)) {
            self.view = Some(Dib::new(s.w, s.h)?);
        }
        let view = self.view.as_mut().expect("allocated above");
        unsafe {
            let _ = BitBlt(view.dc, 0, 0, s.w, s.h, Some(base.dc), 0, 0, SRCCOPY);
            let _ = GdiFlush();
        }
        let drawing = match &self.drag {
            Some(Drag::Mark { mark, .. }) => Some(mark),
            _ => None,
        };
        for m in drawing.into_iter().chain(editing.map(|i| &self.marks[i])).filter(|m| m.is_effect()) {
            apply_effect(view, m, s, self.scale);
        }
        let (shift, scale) = ((-s.x as f32, -s.y as f32), self.scale);
        let shapes = self.marks.iter().chain(drawing).filter(|m| !m.is_effect());
        painter.draw(view, |c| {
            for m in shapes {
                draw_mark(c, m, shift, scale);
            }
        })
    }

    /// Paints the selection (with marks), its handles, the picked mark's handles, the toolbar and the pill.
    pub(crate) fn paint(
        &self,
        hdc: HDC,
        painter: &Painter,
        font: &IDWriteTextFormat,
        white: HBRUSH,
        black: HBRUSH,
    ) -> Result<(), String> {
        unsafe {
            let s = self.sel;
            if let Some(v) = &self.view {
                let _ = BitBlt(hdc, s.x, s.y, s.w, s.h, Some(v.dc), 0, 0, SRCCOPY);
            }
            // 1 px white inside 1 px black, outside the shot, like the picker.
            for (outer, inner, brush) in [(s.inflate(1), s, white), (s.inflate(2), s.inflate(1), black)] {
                for e in ring(outer, inner) {
                    FillRect(hdc, &e, brush);
                }
            }
            // Handles at the corners and edge midpoints.
            let hs = self.px(7.0) as i32;
            for e in GRIPS {
                let (x, y) = edge_point(s, e);
                let (x, y) = (x as i32, y as i32);
                let outer = Rect { x: x - hs / 2 - 1, y: y - hs / 2 - 1, w: hs + 2, h: hs + 2 };
                FillRect(hdc, &outer.to_win(), black);
                FillRect(hdc, &outer.inflate(-1).to_win(), white);
            }
            let visible = |r: Rect| RectVisible(hdc, &r.to_win()).as_bool();
            if let Some(m) = self.picked_mark() {
                if let Mark::Stroke { .. } = m {
                    dashed_box(hdc, self.outline(m), white, black, self.px(4.0) as i32);
                }
                let (r, edge) = (4.5 * self.scale, 1.5 * self.scale);
                for (_, q) in m.grips() {
                    let gb = self.grip_box(q);
                    if visible(gb) {
                        let at = (q.0 - gb.x as f32, q.1 - gb.y as f32);
                        painter.draw_on(hdc, gb.to_win(), |c| {
                            c.circle(at, r + edge, GRIP_EDGE);
                            c.circle(at, r, WHITE);
                        })?;
                    }
                }
            }
            if visible(self.bar) {
                painter.draw_on(hdc, self.bar.to_win(), |c| self.draw_bar(c))?;
            }
            if let Some((r, _, text)) = &self.pill
                && visible(*r)
            {
                let (w, h, pad) = (r.w as f32, r.h as f32, self.px(10.0));
                painter.draw_on(hdc, r.to_win(), |c| {
                    c.round_rect(0.0, 0.0, w, h, h / 2.0, BAR_BG);
                    c.text(text, font, [pad, 0.0, w - pad, h], WHITE);
                })?;
            }
        }
        Ok(())
    }

    /// The finished shot: the selection with every mark, as 0x00RRGGBB rows.
    pub(crate) fn pixels(&mut self, bright: &Dib, painter: &Painter) -> Result<(Vec<u32>, u32, u32), String> {
        self.drag = None;
        self.render_view(bright, painter)?;
        let v = self.view.as_ref().expect("rendered above");
        Ok((v.pixels().to_vec(), v.w as u32, v.h as u32))
    }
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
