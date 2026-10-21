use glint_core::geom::rect::Rect;
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;

use super::Editor;
use super::marks::{COLORS, TOOLS, Tool, arrow};
use super::text::Typesetter;
use crate::platform::gfx::{Canvas, Painter, rgba};

pub(super) const BAR_BG: D2D1_COLOR_F = rgba(16, 16, 20, 0.92);
pub(super) const WHITE: D2D1_COLOR_F = rgba(255, 255, 255, 0.96);
const COLOR_TIPS: [&str; 5] =
    ["red  ·  Shift+1", "yellow  ·  Shift+2", "green  ·  Shift+3", "blue  ·  Shift+4", "white  ·  Shift+5"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Item {
    Region,
    Tool(Tool),
    Color(usize),
    Size(usize),
    Undo,
    Redo,
    Copy,
    Save,
    Upload,
    Pin,
    Done,
    Close,
}

impl Item {
    pub(super) fn tip(self) -> Option<&'static str> {
        Some(match self {
            Item::Region => "region  ·  Q",
            Item::Tool(t) => TOOLS.iter().find(|(tool, ..)| *tool == t).map(|(_, tip, ..)| *tip)?,
            Item::Color(i) => COLOR_TIPS[i],
            Item::Size(_) => "size  ·  [ ]",
            Item::Undo => "undo  ·  Ctrl+Z",
            Item::Redo => "redo  ·  Ctrl+Y",
            Item::Copy => "copy  ·  Ctrl+C",
            Item::Save => "save  ·  Ctrl+S",
            Item::Upload => "upload  ·  Ctrl+U",
            Item::Pin => "pin to screen",
            Item::Done => "done  ·  Enter",
            Item::Close => "cancel  ·  Esc",
        })
    }
}

/// Toolbar groups, left to right; a separator goes between groups. `region`: canvas mode's region tool.
fn groups(region: bool) -> Vec<Vec<Item>> {
    let mut groups = if region { vec![vec![Item::Region]] } else { Vec::new() };
    groups.extend([
        TOOLS.iter().map(|(t, ..)| Item::Tool(*t)).collect(),
        (0..COLORS.len()).map(Item::Color).collect(),
        (0..3).map(Item::Size).collect(),
        vec![Item::Undo, Item::Redo],
        vec![Item::Copy, Item::Save, Item::Upload, Item::Pin, Item::Done, Item::Close],
    ]);
    groups
}

impl Editor {
    /// Lays the toolbar out under the selection, or above it, or inside it when neither fits; with
    /// no selection, docked at the top of the monitor under the mouse, or summoned to the mouse.
    pub(super) fn place_bar(&mut self) {
        let (cell, pad, gap) = (self.px(32.0) as i32, self.px(4.0) as i32, self.px(9.0) as i32);
        let groups = groups(self.region);
        let n: i32 = groups.iter().map(|g| g.len() as i32).sum();
        let w = pad * 2 + n * cell + gap * (groups.len() as i32 - 1);
        let h = cell + pad * 2;
        let margin = self.px(10.0) as i32;
        let (bar, monitor) = if self.selected {
            place(self.sel, (w, h), margin, &self.monitors)
        } else if self.summoned {
            summon(self.mouse, (w, h), margin, &self.monitors)
        } else {
            dock(self.mouse, (w, h), margin, &self.monitors)
        };
        self.bar_monitor = monitor;
        let (x, y) = (bar.x, bar.y);
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
        let (tw, _) = painter.measure(text, font);
        let (w, h) = ((tw.ceil() + self.px(20.0)) as i32, self.px(26.0) as i32);
        let b = self.bar;
        let cell = self.cells.iter().find(|(i, _)| *i == item).map(|(_, r)| *r).unwrap_or(b);
        let gap = self.px(6.0) as i32;
        let below_sel = !self.selected || b.y >= self.sel.bottom();
        let mut y = if below_sel { b.bottom() + gap } else { b.y - gap - h };
        let m = self.bar_monitor;
        if y < m.y || y + h > m.bottom() {
            y = if below_sel { b.y - gap - h } else { b.bottom() + gap };
        }
        let x = (cell.x + cell.w / 2 - w / 2).clamp(m.x, (m.right() - w).max(m.x));
        Rect { x, y, w, h }
    }

    pub(super) fn draw_bar(&self, c: &Canvas) {
        let (b, s) = (self.bar, self.scale);
        let (pressed, size, color) = (self.pressed(), self.current(), self.shown_color());
        let (can_undo, can_redo) = (!self.history.is_empty(), !self.future.is_empty());
        let groups = groups(self.region);
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
                Item::Region => self.picks_region(),
                Item::Tool(t) => self.tool == Some(t),
                Item::Size(n) => size.is_some_and(|(_, s, v)| s.presets[n] == v),
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
            draw_icon(c, item, (x + cw / 2.0, y + ch / 2.0), s, ink, color, &self.fonts);
        }
    }
}

/// The bar and the monitor it lies on; it never straddles monitors or sits in a gap.
fn place(sel: Rect, (w, h): (i32, i32), margin: i32, monitors: &[Rect]) -> (Rect, Rect) {
    let cx = sel.x + sel.w / 2;
    let on = |m: Rect, y: i32| {
        Rect { x: (cx - w / 2).max(m.x + margin).min((m.right() - w - margin).max(m.x)), y, w, h }
    };
    let low = nearest(monitors, (cx, sel.bottom() - 1));
    let below = (sel.bottom() + margin).max(low.y + margin);
    if below + h <= low.bottom() - margin {
        return (on(low, below), low);
    }
    let high = nearest(monitors, (cx, sel.y));
    let above = (sel.y - margin - h).min(high.bottom() - margin - h);
    if above >= high.y + margin {
        return (on(high, above), high);
    }
    let inside = (sel.bottom() - margin - h).min(low.bottom() - margin - h).max(low.y + margin);
    (on(low, inside), low)
}

/// The bar at the top centre of the monitor under (or nearest) the mouse.
fn dock(mouse: (i32, i32), (w, h): (i32, i32), margin: i32, monitors: &[Rect]) -> (Rect, Rect) {
    let m = nearest(monitors, mouse);
    let x = (m.x + (m.w - w) / 2).max(m.x);
    (Rect { x, y: m.y + margin, w, h }, m)
}

fn summon((x, y): (i32, i32), (w, h): (i32, i32), margin: i32, monitors: &[Rect]) -> (Rect, Rect) {
    let m = nearest(monitors, (x, y));
    let below = y + margin;
    let y = if below + h <= m.bottom() - margin { below } else { y - margin - h };
    let fit = |v: i32, lo: i32, room: i32, len: i32| {
        let (min, max) = (lo + margin, lo + room - len - margin);
        v.clamp(min, max.max(min))
    };
    (Rect { x: fit(x - w / 2, m.x, m.w, w), y: fit(y, m.y, m.h, h), w, h }, m)
}

pub(super) fn beside((x, y): (i32, i32), (w, h): (i32, i32), off: i32, monitor: Rect) -> Rect {
    let m = monitor;
    let fit = |v: i32, lo: i32, room: i32, len: i32| v.clamp(lo, (lo + room - len).max(lo));
    Rect { x: fit(x + off, m.x, m.w, w), y: fit(y + off, m.y, m.h, h), w, h }
}

pub(super) fn nearest(monitors: &[Rect], (x, y): (i32, i32)) -> Rect {
    let gap = |m: &&Rect| {
        let dx = (m.x - x).max(x - (m.right() - 1)).max(0) as i64;
        let dy = (m.y - y).max(y - (m.bottom() - 1)).max(0) as i64;
        dx * dx + dy * dy
    };
    *monitors.iter().min_by_key(gap).expect("at least one monitor")
}

fn draw_icon(c: &Canvas, item: Item, (x, y): (f32, f32), s: f32, ink: D2D1_COLOR_F, color: usize, fonts: &Typesetter) {
    let p = |dx: f32, dy: f32| (x + dx * s, y + dy * s);
    match item {
        Item::Region => {
            c.polyline(&[p(-3.5, -7.0), p(-3.5, 3.5), p(7.0, 3.5)], 1.6 * s, ink);
            c.polyline(&[p(-7.0, -3.5), p(3.5, -3.5), p(3.5, 7.0)], 1.6 * s, ink);
        }
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
        Item::Tool(Tool::Eraser) => {
            let body = [p(-6.7, 2.5), p(-2.5, 6.7), p(6.7, -2.5), p(2.5, -6.7)];
            c.polygon(&[body[0], body[1], p(1.1, 3.2), p(-3.2, -1.1)], ink);
            c.polyline(&[body[0], body[1], body[2], body[3], body[0]], 1.5 * s, ink);
        }
        Item::Tool(Tool::Text) => {
            c.polyline(&[p(-5.5, -5.5), p(5.5, -5.5)], 2.0 * s, ink);
            c.polyline(&[p(0.0, -5.5), p(0.0, 6.0)], 2.0 * s, ink);
        }
        Item::Tool(Tool::Step) => {
            c.circle((x, y), 7.5 * s, ink);
            fonts.draw_centred(c, "1", 10.0 * s, (x, y), BAR_BG);
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
        Item::Upload => {
            c.polyline(&[p(0.0, 2.5), p(0.0, -6.0)], 1.6 * s, ink);
            c.polyline(&[p(-3.5, -2.5), p(0.0, -6.0), p(3.5, -2.5)], 1.6 * s, ink);
            c.polyline(&[p(-6.0, 3.0), p(-6.0, 6.0), p(6.0, 6.0), p(6.0, 3.0)], 1.6 * s, ink);
        }
        Item::Pin => {
            c.polyline(&[p(-4.0, -6.5), p(4.0, -6.5)], 1.6 * s, ink);
            c.polygon(&[p(-2.5, -6.5), p(2.5, -6.5), p(3.5, -1.0), p(-3.5, -1.0)], ink);
            c.polyline(&[p(-5.5, -1.0), p(5.5, -1.0)], 1.6 * s, ink);
            c.polyline(&[p(0.0, -1.0), p(0.0, 7.0)], 1.4 * s, ink);
        }
        Item::Done => c.polyline(&[p(-5.5, 0.5), p(-1.5, 4.5), p(6.0, -4.0)], 2.0 * s, ink),
        Item::Close => {
            c.polyline(&[p(-5.0, -5.0), p(5.0, 5.0)], 1.6 * s, ink);
            c.polyline(&[p(-5.0, 5.0), p(5.0, -5.0)], 1.6 * s, ink);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (i32, i32) = (556, 40);
    const MARGIN: i32 = 10;
    /// Desktop layout with gaps: a wide monitor, one to its lower left, one above it.
    const MONITORS: [Rect; 3] = [
        Rect { x: 0, y: 0, w: 3440, h: 1440 },
        Rect { x: -1920, y: 540, w: 1920, h: 1080 },
        Rect { x: 365, y: -1440, w: 2560, h: 1440 },
    ];

    fn bar(sel: Rect) -> (Rect, Rect) {
        place(sel, SIZE, MARGIN, &MONITORS)
    }

    #[test]
    fn bar_goes_below_then_above_then_inside() {
        let sel = Rect { x: 1000, y: 200, w: 800, h: 600 };
        assert_eq!(bar(sel), (Rect { x: 1122, y: 810, w: 556, h: 40 }, MONITORS[0]));
        let low = Rect { x: -1800, y: 1000, w: 800, h: 610 };
        assert_eq!(bar(low), (Rect { x: -1678, y: 950, w: 556, h: 40 }, MONITORS[1]));
        let top = (Rect { x: 1367, y: -50, w: 556, h: 40 }, MONITORS[2]);
        assert_eq!(bar(MONITORS[2]), top, "not on the monitor below");
        let both = Rect { x: 0, y: -1440, w: 3440, h: 2880 };
        assert_eq!(bar(both), (Rect { x: 1442, y: 1390, w: 556, h: 40 }, MONITORS[0]));
    }

    #[test]
    fn bar_avoids_gaps() {
        // bottom centre in the gap under the wide monitor; the left one has no room below
        let sel = Rect { x: -500, y: 1000, w: 1000, h: 600 };
        assert_eq!(bar(sel), (Rect { x: 10, y: 950, w: 556, h: 40 }, MONITORS[0]));
        for x in (-1920..3440).step_by(97) {
            for y in (-1440..1620).step_by(89) {
                for (w, h) in [(40, 30), (700, 500), (2600, 1700), (5360, 3060)] {
                    let sel = Rect { x, y, w, h };
                    let (b, m) = bar(sel);
                    assert!(MONITORS.contains(&m));
                    assert_eq!(m.intersect(&b), Some(b), "{sel:?} put the bar at {b:?}");
                }
            }
        }
    }

    #[test]
    fn summoned_bar_sits_under_the_cursor_on_its_monitor() {
        let at = |p: (i32, i32)| summon(p, SIZE, MARGIN, &MONITORS);
        assert_eq!(at((1720, 500)), (Rect { x: 1442, y: 510, w: 556, h: 40 }, MONITORS[0]));
        assert_eq!(at((3430, 500)).0, Rect { x: 2874, y: 510, w: 556, h: 40 }, "clamped at the right edge");
        assert_eq!(at((1000, 1420)).0, Rect { x: 722, y: 1370, w: 556, h: 40 }, "above when there's no room below");
        assert_eq!(at((-1900, 600)), (Rect { x: -1910, y: 610, w: 556, h: 40 }, MONITORS[1]));
        assert_eq!(at((-100, 200)), (Rect { x: 10, y: 210, w: 556, h: 40 }, MONITORS[0]), "from a gap, the nearest");
        for x in (-1920..3440).step_by(53) {
            for y in (-1440..1620).step_by(47) {
                let (b, m) = at((x, y));
                assert_eq!(m.intersect(&b), Some(b), "({x}, {y}) put the bar at {b:?}");
            }
        }
    }

    #[test]
    fn docked_bar_sits_at_the_top_of_the_mouses_monitor() {
        let at = |p: (i32, i32)| dock(p, SIZE, MARGIN, &MONITORS);
        assert_eq!(at((100, 1300)), (Rect { x: 1442, y: 10, w: 556, h: 40 }, MONITORS[0]));
        assert_eq!(at((-1000, 1000)), (Rect { x: -1238, y: 550, w: 556, h: 40 }, MONITORS[1]));
        assert_eq!(at((2000, -5)), (Rect { x: 1367, y: -1430, w: 556, h: 40 }, MONITORS[2]));
        assert_eq!(at((-100, 200)).1, MONITORS[0], "from a gap, the nearest");
    }

    #[test]
    fn hints_stay_beside_the_cursor_on_screen() {
        let m = MONITORS[0];
        assert_eq!(beside((100, 100), (200, 26), 16, m), Rect { x: 116, y: 116, w: 200, h: 26 });
        assert_eq!(beside((3430, 1430), (200, 26), 16, m), Rect { x: 3240, y: 1414, w: 200, h: 26 });
    }
}
