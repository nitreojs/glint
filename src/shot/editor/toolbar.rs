use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;

use super::Editor;
use super::marks::{COLORS, TOOLS, Tool, arrow};
use crate::platform::gfx::{Canvas, Painter, rgba};
use crate::platform::rect::Rect;

pub(super) const BAR_BG: D2D1_COLOR_F = rgba(16, 16, 20, 0.92);
pub(super) const WHITE: D2D1_COLOR_F = rgba(255, 255, 255, 0.96);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Item {
    Tool(Tool),
    Color(usize),
    Size(usize),
    Undo,
    Redo,
    Copy,
    Save,
    Done,
    Close,
}

impl Item {
    pub(super) fn tip(self) -> Option<&'static str> {
        Some(match self {
            Item::Tool(t) => TOOLS.iter().find(|(tool, ..)| *tool == t).map(|(_, tip, _)| *tip)?,
            Item::Color(_) => return None,
            Item::Size(_) => "size  ·  [ ]",
            Item::Undo => "undo  ·  Ctrl+Z",
            Item::Redo => "redo  ·  Ctrl+Y",
            Item::Copy => "copy  ·  Ctrl+C",
            Item::Save => "save  ·  Ctrl+S",
            Item::Done => "done  ·  Enter",
            Item::Close => "cancel  ·  Esc",
        })
    }
}

/// Toolbar groups, left to right; a separator goes between groups.
fn groups() -> [Vec<Item>; 5] {
    [
        TOOLS.iter().map(|(t, ..)| Item::Tool(*t)).collect(),
        (0..COLORS.len()).map(Item::Color).collect(),
        (0..3).map(Item::Size).collect(),
        vec![Item::Undo, Item::Redo],
        vec![Item::Copy, Item::Save, Item::Done, Item::Close],
    ]
}

impl Editor {
    /// Lays the toolbar out under the selection, or above it, or inside it when neither fits.
    pub(super) fn place_bar(&mut self) {
        let (cell, pad, gap) = (self.px(32.0) as i32, self.px(4.0) as i32, self.px(9.0) as i32);
        let groups = groups();
        let n: i32 = groups.iter().map(|g| g.len() as i32).sum();
        let w = pad * 2 + n * cell + gap * (groups.len() as i32 - 1);
        let h = cell + pad * 2;
        let margin = self.px(10.0) as i32;
        let b = self.bounds;
        let x = (self.sel.x + self.sel.w / 2 - w / 2).clamp(b.x + margin, (b.right() - w - margin).max(b.x));
        let below = self.sel.bottom() + margin;
        let above = self.sel.y - margin - h;
        let y = if below + h <= b.bottom() - margin {
            below
        } else if above >= b.y + margin {
            above
        } else {
            self.sel.bottom() - margin - h
        };
        let bar = Rect { x, y, w, h };
        if bar == self.bar && !self.cells.is_empty() {
            return;
        }
        self.bar = bar;
        self.cells.clear();
        let mut cx = x + pad;
        for (gi, group) in groups.iter().enumerate() {
            if gi > 0 {
                cx += gap;
            }
            for &item in group {
                self.cells.push((item, Rect { x: cx, y: y + pad, w: cell, h: cell }));
                cx += cell;
            }
        }
        self.pill = None;
    }

    /// Tooltip for the hovered button, on the side of the bar away from the selection.
    pub(super) fn place_pill(&self, item: Item, text: &str, painter: &Painter, font: &IDWriteTextFormat) -> Rect {
        let b = self.bar;
        let cell = self.cells.iter().find(|(i, _)| *i == item).map(|(_, r)| *r).unwrap_or(b);
        let (tw, _) = painter.measure(text, font);
        let (w, h) = ((tw.ceil() + self.px(20.0)) as i32, self.px(26.0) as i32);
        let gap = self.px(6.0) as i32;
        let below_sel = b.y >= self.sel.bottom();
        let mut y = if below_sel { b.bottom() + gap } else { b.y - gap - h };
        if y < self.bounds.y || y + h > self.bounds.bottom() {
            y = if below_sel { b.y - gap - h } else { b.bottom() + gap };
        }
        let x = (cell.x + cell.w / 2 - w / 2).clamp(self.bounds.x, (self.bounds.right() - w).max(self.bounds.x));
        Rect { x, y, w, h }
    }

    pub(super) fn draw_bar(&self, c: &Canvas) {
        let (b, s) = (self.bar, self.scale);
        let (pressed, size, color) = (self.pressed(), self.current(), self.shown_color());
        let (can_undo, can_redo) = (!self.history.is_empty(), !self.future.is_empty());
        let groups = groups();
        let (w, h) = (b.w as f32, b.h as f32);
        c.round_rect(0.0, 0.0, w, h, 10.0 * s, BAR_BG);
        // Separators between groups.
        let mut i = 0;
        for g in &groups[..groups.len() - 1] {
            i += g.len();
            let (_, before) = self.cells[i - 1];
            let x = (before.right() - b.x) as f32 + (4.0 * s).round() + 0.5;
            c.line((x, h * 0.28), (x, h * 0.72), 1.0, rgba(255, 255, 255, 0.16));
        }
        for &(item, r) in &self.cells {
            let (x, y) = ((r.x - b.x) as f32, (r.y - b.y) as f32);
            let (cw, ch) = (r.w as f32, r.h as f32);
            let active = match item {
                Item::Tool(t) => self.tool == Some(t),
                Item::Size(n) => size.is_some_and(|(tool, v)| tool.strength().presets[n] == v),
                _ => false,
            };
            let bg = if pressed == Some(item) {
                0.26
            } else if active {
                0.18
            } else if self.hover == Some(item) {
                0.10
            } else {
                0.0
            };
            if bg > 0.0 {
                c.round_rect(x + 2.0 * s, y + 2.0 * s, cw - 4.0 * s, ch - 4.0 * s, 7.0 * s, rgba(255, 255, 255, bg));
            }
            let enabled = match item {
                Item::Undo => can_undo,
                Item::Redo => can_redo,
                Item::Size(_) => size.is_some(),
                _ => true,
            };
            let ink = if enabled { WHITE } else { rgba(255, 255, 255, 0.3) };
            draw_icon(c, item, (x + cw / 2.0, y + ch / 2.0), s, ink, color);
        }
    }
}

fn draw_icon(
    c: &Canvas,
    item: Item,
    (x, y): (f32, f32),
    s: f32,
    ink: D2D1_COLOR_F,
    color: usize,
) {
    let p = |dx: f32, dy: f32| (x + dx * s, y + dy * s);
    match item {
        Item::Tool(Tool::Pen) => {
            c.polyline(&[p(-5.0, 5.0), p(5.0, -5.0)], 2.0 * s, ink);
            c.polygon(&[p(-6.5, 6.5), p(-5.5, 3.5), p(-3.5, 5.5)], ink);
        }
        Item::Tool(Tool::Highlighter) => {
            c.polyline(&[p(-5.0, 3.0), p(5.0, -3.0)], 6.0 * s, D2D1_COLOR_F { a: 0.45, ..ink });
            c.polyline(&[p(-6.0, 7.0), p(6.0, 7.0)], 1.5 * s, ink);
        }
        Item::Tool(Tool::Arrow) => arrow(c, p(-5.5, 5.5), p(6.0, -6.0), 1.8 * s, ink),
        Item::Tool(Tool::Rectangle) => c.rect_outline(x - 6.5 * s, y - 5.0 * s, 13.0 * s, 10.0 * s, 1.6 * s, ink),
        Item::Tool(Tool::Blur) => {
            c.circle((x, y), 7.0 * s, D2D1_COLOR_F { a: 0.22, ..ink });
            c.circle((x, y), 5.0 * s, D2D1_COLOR_F { a: 0.45, ..ink });
            c.circle((x, y), 2.8 * s, ink);
        }
        Item::Tool(Tool::Pixelate) => {
            let q = 4.0 * s;
            for row in 0..3 {
                for col in 0..3 {
                    let a = if (row + col) % 2 == 0 { 1.0 } else { 0.35 };
                    let (qx, qy) = (x - 6.0 * s + col as f32 * q, y - 6.0 * s + row as f32 * q);
                    c.round_rect(qx, qy, q - 0.6 * s, q - 0.6 * s, 0.6 * s, D2D1_COLOR_F { a: a * ink.a, ..ink });
                }
            }
        }
        Item::Color(i) => {
            c.circle((x, y), 6.0 * s, COLORS[i]);
            if i == color {
                c.ring((x, y), 9.0 * s, 1.5 * s, ink);
            }
        }
        Item::Size(i) => c.circle((x, y), [1.5, 3.0, 5.0][i] * s, ink),
        Item::Undo | Item::Redo => {
            // An arch with its arrowhead on the left (undo) or the right (redo).
            let dir = if item == Item::Undo { 1.0 } else { -1.0 };
            let arc: Vec<(f32, f32)> = (0..=12)
                .map(|i| std::f32::consts::PI * (1.0 + i as f32 / 12.0))
                .map(|a| p(-dir * 5.0 * a.cos(), 2.0 + 5.0 * a.sin()))
                .collect();
            c.polyline(&arc, 1.6 * s, ink);
            c.polygon(&[p(-dir * 5.0, 6.0), p(-dir * 8.0, 1.5), p(-dir * 2.0, 1.5)], ink);
        }
        Item::Copy => {
            c.rect_outline(x - 5.5 * s, y - 2.5 * s, 8.0 * s, 8.0 * s, 1.4 * s, ink);
            c.polyline(&[p(-2.5, -3.5), p(-2.5, -5.5), p(5.5, -5.5), p(5.5, 2.5), p(3.5, 2.5)], 1.4 * s, ink);
        }
        Item::Save => {
            c.polyline(&[p(0.0, -6.0), p(0.0, 2.5)], 1.6 * s, ink);
            c.polyline(&[p(-3.5, -1.0), p(0.0, 2.5), p(3.5, -1.0)], 1.6 * s, ink);
            c.polyline(&[p(-6.0, 3.0), p(-6.0, 6.0), p(6.0, 6.0), p(6.0, 3.0)], 1.6 * s, ink);
        }
        Item::Done => c.polyline(&[p(-5.5, 0.5), p(-1.5, 4.5), p(6.0, -4.0)], 2.0 * s, ink),
        Item::Close => {
            c.polyline(&[p(-5.0, -5.0), p(5.0, 5.0)], 1.6 * s, ink);
            c.polyline(&[p(-5.0, 5.0), p(5.0, -5.0)], 1.6 * s, ink);
        }
    }
}
