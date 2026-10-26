//! What opens beside the toolbar: the text bar (font, size, bold / italic / underline /
//! strikethrough, alignment, backdrop) with its searchable font list, and the clipboard strip
//! (see `paste.rs`). Panels slide out of the toolbar on the side away from the selection.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glint_core::geom::rect::Rect;
use glint_shell::gfx::{Canvas, ease_out, rgba};
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;

use super::marks::{Ink, Mark, Tool};
use super::fonts;
use super::text::{Align, Back, Style, VAlign};
use super::toolbar::{BAR_BG, Item, WHITE};
use super::{Editor, Event};

/// How long a panel takes to slide out.
pub(super) const PANEL_MS: f32 = 160.0;
/// Rows the font list shows at once.
const ROWS: usize = 8;
/// Frames of an animation, roughly: repaints come this far apart while one runs.
pub(super) const FRAME: Duration = Duration::from_millis(12);

/// Buttons and the boxes they cover (client coordinates).
pub(super) type Cells = Vec<(Item, Rect)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Panel {
    Text,
    Strip,
}

/// The font list under the text bar: what's been typed to search it, how far it's scrolled, and
/// what the search found (kept until the query or the families change).
pub(super) struct FontPicker {
    pub(super) query: String,
    pub(super) scroll: usize,
    pub(super) since: Instant,
    found: RefCell<Option<Found>>,
}

/// A search, how many families it ran over, and where the matches sit among them.
type Found = (String, usize, Rc<Vec<usize>>);

impl FontPicker {
    pub(super) fn new() -> FontPicker {
        FontPicker { query: String::new(), scroll: 0, since: Instant::now(), found: RefCell::default() }
    }
}

/// How far (0 to 1, eased) an animation that started at `since` has come.
pub(super) fn progress(since: Instant, ms: f32) -> f32 {
    ease_out((since.elapsed().as_secs_f32() * 1000.0 / ms).min(1.0))
}

pub(super) fn faded(c: D2D1_COLOR_F, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { a: c.a * a, ..c }
}

/// The font list's parts: its box, the search row, the rows (with the families they show and
/// where those sit among all of them), and whether the families are still being read.
pub(super) struct FontList {
    pub(super) rect: Rect,
    pub(super) search: Rect,
    pub(super) rows: Vec<(Item, Rect, Arc<str>, usize)>,
    pub(super) loading: bool,
}

impl Editor {
    /// The panel showing now: the text bar for the text tool or a text, the strip while the
    /// clipboard tool has it open.
    pub(super) fn panel(&self) -> Option<Panel> {
        let text = self.typing.is_some()
            || self.tool == Some(Tool::Text)
            || self.picked_mark().is_some_and(|m| matches!(m, Mark::Text { .. }));
        if text {
            Some(Panel::Text)
        } else if self.tool == Some(Tool::Image) && self.strip_open {
            Some(Panel::Strip)
        } else {
            None
        }
    }

    /// How far the panel has slid out (0 to 1); a panel that just appeared starts its slide.
    pub(super) fn panel_t(&self) -> f32 {
        let now = self.panel();
        match self.panel_since.get() {
            Some((p, since)) if Some(p) == now => progress(since, PANEL_MS),
            _ => {
                self.panel_since.set(now.map(|p| (p, Instant::now())));
                0.0
            }
        }
    }

    /// A `w`×`h` panel next to the bar, on the side away from the selection, on the bar's monitor.
    pub(super) fn place_panel(&self, (w, h): (i32, i32)) -> Rect {
        let (b, m, gap) = (self.bar, self.bar_monitor, self.px(6.0) as i32);
        let below = self.below_bar();
        let mut y = if below { b.bottom() + gap } else { b.y - gap - h };
        if y < m.y || y + h > m.bottom() {
            y = if below { b.y - gap - h } else { b.bottom() + gap };
        }
        let x = (b.x + b.w / 2 - w / 2).clamp(m.x, (m.right() - w).max(m.x));
        Rect { x, y, w, h }
    }

    /// The panel's box and its buttons.
    pub(super) fn panel_layout(&self) -> Option<(Panel, Rect, Cells)> {
        match self.panel()? {
            Panel::Text => {
                let (rect, cells) = self.text_bar();
                Some((Panel::Text, rect, cells))
            }
            Panel::Strip => {
                let (rect, cells) = self.strip_layout();
                Some((Panel::Strip, rect, cells))
            }
        }
    }

    /// The text bar: the font, smaller / bigger around the size, the four styles, alignment and
    /// backdrop.
    fn text_bar(&self) -> (Rect, Vec<(Item, Rect)>) {
        let (cell, pad, gap) = (self.px(32.0) as i32, self.px(4.0) as i32, self.px(9.0) as i32);
        let (font_w, size_w) = (self.px(148.0) as i32, self.px(30.0) as i32);
        let w = 2 * pad + font_w + 2 * cell + size_w + 4 * cell + 3 * cell + 3 * gap;
        let rect = self.place_panel((w, cell + 2 * pad));
        let (mut x, y) = (rect.x + pad, rect.y + pad);
        let mut cells = Vec::with_capacity(9);
        let mut put = |item: Item, w: i32, x: &mut i32| {
            cells.push((item, Rect { x: *x, y, w, h: cell }));
            *x += w;
        };
        put(Item::Font, font_w, &mut x);
        x += gap;
        put(Item::Smaller, cell, &mut x);
        x += size_w;
        put(Item::Bigger, cell, &mut x);
        x += gap;
        for item in [Item::Bold, Item::Italic, Item::Underline, Item::Strike] {
            put(item, cell, &mut x);
        }
        x += gap;
        put(Item::Align, cell, &mut x);
        put(Item::VAlign, cell, &mut x);
        put(Item::Back, cell, &mut x);
        (rect, cells)
    }

    /// All the families, and where the ones the font list's search finds sit among them.
    fn found_fonts(&self) -> (Arc<Vec<Arc<str>>>, Rc<Vec<usize>>) {
        let names = fonts::names().unwrap_or_default();
        let Some(picker) = &self.fonts_open else { return (names, Rc::default()) };
        let mut cache = picker.found.borrow_mut();
        if let Some((query, n, found)) = &*cache
            && *query == picker.query
            && *n == names.len()
        {
            return (names, found.clone());
        }
        let found = Rc::new(fonts::search(&names, &picker.query));
        *cache = Some((picker.query.clone(), names.len(), found.clone()));
        (names, found)
    }

    /// The font list, under (or over) the text bar's font button, while it's open.
    pub(super) fn font_list(&self) -> Option<FontList> {
        let picker = self.fonts_open.as_ref()?;
        let (Panel::Text, panel, cells) = self.panel_layout()? else { return None };
        let font = cells.iter().find(|(i, _)| *i == Item::Font).map(|(_, r)| *r)?;
        let (names, found) = self.found_fonts();
        let (pad, row, search_h) = (self.px(5.0) as i32, self.px(28.0) as i32, self.px(32.0) as i32);
        let shown: Vec<usize> = found.iter().copied().skip(picker.scroll).take(ROWS).collect();
        let (w, h) = (self.px(250.0) as i32, 2 * pad + search_h + row * (shown.len().max(1) as i32));
        let (m, gap) = (self.bar_monitor, self.px(6.0) as i32);
        let below = panel.y >= self.bar.y;
        // under the text bar, or over the bar and the text bar together when there's no room
        let (top, bottom) = (panel.y.min(self.bar.y), panel.bottom().max(self.bar.bottom()));
        let mut y = if below { panel.bottom() + gap } else { panel.y - gap - h };
        if y < m.y || y + h > m.bottom() {
            y = if below { top - gap - h } else { bottom + gap };
        }
        let x = font.x.clamp(m.x, (m.right() - w).max(m.x));
        let rect = Rect { x, y, w, h };
        let search = Rect { x: x + pad, y: y + pad, w: w - 2 * pad, h: search_h };
        let rows = shown
            .into_iter()
            .enumerate()
            .map(|(i, family)| {
                let r = Rect { x: x + pad, y: search.bottom() + i as i32 * row, w: w - 2 * pad, h: row };
                (Item::FontRow(picker.scroll + i), r, names[family].clone(), family)
            })
            .collect();
        Some(FontList { rect, search, rows, loading: fonts::names().is_none() })
    }

    /// The font list shows rows whose families are still loading (or none yet): it repaints
    /// until they're there.
    pub(super) fn fonts_pending(&self) -> bool {
        self.font_list().is_some_and(|l| l.loading || l.rows.iter().any(|r| fonts::ready(r.3).is_none()))
    }

    /// How many of the font list's rows show their own face (for repaints as they come in).
    pub(super) fn fonts_shown(&self) -> usize {
        self.font_list().map_or(0, |l| l.rows.iter().filter(|r| fonts::ready(r.3).is_some()).count())
    }

    /// The boxes panels cover: the panel and the font list.
    pub(super) fn panel_rects(&self) -> Vec<Rect> {
        let mut v: Vec<Rect> = self.panel_layout().map(|(_, r, _)| r).into_iter().collect();
        v.extend(self.font_list().map(|l| l.rect));
        v.extend(self.picker_layout().map(|l| l.rect));
        v
    }

    /// Every button: the toolbar's, the panel's, the font list's rows and the colour picker's parts.
    pub(super) fn all_cells(&self) -> Vec<(Item, Rect)> {
        let mut v = self.cells.clone();
        v.extend(self.panel_layout().into_iter().flat_map(|(_, _, cells)| cells));
        v.extend(self.font_list().into_iter().flat_map(|l| l.rows.into_iter().map(|(i, r, ..)| (i, r))));
        if let Some(l) = self.picker_layout() {
            v.extend([(Item::Sv, l.square), (Item::Hue, l.strip), (Item::Hex, l.hex)]);
        }
        v
    }

    /// The text the text bar works on: the one being typed, else the picked one.
    fn text_target(&self) -> Option<&Mark> {
        let i = self.typing.as_ref().map(|t| t.index).or(self.picked)?;
        Some(&self.marks[i]).filter(|m| matches!(m, Mark::Text { .. }))
    }

    /// The style and size the text bar shows.
    pub(super) fn shown_style(&self) -> (Style, f32) {
        match self.text_target() {
            Some(Mark::Text { style, size, .. }) => (style.clone(), *size),
            _ => (self.text_style.clone(), self.strength[Tool::Text as usize]),
        }
    }

    /// A text bar button or a font row was clicked.
    pub(super) fn activate_text(&mut self, item: Item) -> Event {
        match item {
            Item::Font => {
                self.fonts_open = match self.fonts_open {
                    Some(_) => None,
                    None => Some(FontPicker::new()),
                };
                Event::Repaint
            }
            Item::FontRow(i) => {
                let (names, found) = self.found_fonts();
                let Some(name) = found.get(i).map(|&f| names[f].clone()) else { return Event::None };
                self.fonts_open = None;
                self.restyle(|s| s.font = name)
            }
            Item::Smaller | Item::Bigger => {
                let spec = Tool::Text.strength().expect("text has sizes");
                let v = self.shown_style().1 + if item == Item::Bigger { spec.step } else { -spec.step };
                let tool = self.tool;
                // sizes follow the text tool, even when no text is picked
                self.tool = tool.or(Some(Tool::Text));
                let event = self.set_strength(v, false);
                self.tool = tool;
                event
            }
            Item::Bold => self.restyle(|s| s.bold = !s.bold),
            Item::Italic => self.restyle(|s| s.italic = !s.italic),
            Item::Underline => self.restyle(|s| s.underline = !s.underline),
            Item::Strike => self.restyle(|s| s.strike = !s.strike),
            Item::Align => self.restyle(|s| s.align = s.align.next()),
            Item::VAlign => self.restyle(|s| s.valign = s.valign.next()),
            Item::Back => self.restyle(|s| s.back = s.back.next()),
            _ => Event::None,
        }
    }

    /// Typing while the font list is open searches it: `Some(c)` adds a letter, `None` takes the
    /// last one back.
    pub(super) fn font_query(&mut self, c: Option<char>) -> Event {
        let Some(picker) = self.fonts_open.as_mut() else { return Event::None };
        match c {
            Some(c) => picker.query.push(c),
            None => {
                if picker.query.pop().is_none() {
                    return Event::None;
                }
            }
        }
        picker.scroll = 0;
        Event::Repaint
    }

    /// Scrolls the font list by `rows`.
    pub(super) fn scroll_fonts(&mut self, rows: i32) -> Event {
        let n = self.found_fonts().1.len();
        let Some(picker) = self.fonts_open.as_mut() else { return Event::None };
        let max = n.saturating_sub(ROWS);
        let to = (picker.scroll as i64 + rows as i64).clamp(0, max as i64) as usize;
        if to == picker.scroll {
            return Event::None;
        }
        picker.scroll = to;
        Event::Repaint
    }

    /// Keys while the font list is open: Backspace edits the search, Enter takes the first
    /// family shown, the arrows scroll; letters arrive as characters.
    pub(super) fn font_key(&mut self, vk: u32) -> Event {
        match vk {
            0x08 => self.font_query(None),
            0x0D => {
                let first = self.fonts_open.as_ref().map_or(0, |f| f.scroll);
                self.activate_text(Item::FontRow(first))
            }
            0x26 => self.scroll_fonts(-1),
            0x28 => self.scroll_fonts(1),
            0x21 => self.scroll_fonts(-(ROWS as i32)),
            0x22 => self.scroll_fonts(ROWS as i32),
            _ => Event::None,
        }
    }

    /// The text bar, drawn onto `c` (which shows its box `rect`).
    pub(super) fn draw_text_bar(&self, c: &Canvas, rect: Rect, cells: &[(Item, Rect)]) {
        let (s, t) = (self.scale, self.panel_t());
        let a = t;
        let slide = (1.0 - t) * self.px(6.0) * if rect.y >= self.bar.y { -1.0 } else { 1.0 };
        let (w, h) = (rect.w as f32, rect.h as f32);
        c.round_rect(0.0, slide, w, h, 10.0 * s, faded(BAR_BG, a));
        let (style, size) = self.shown_style();
        let color = self.shown_ink();
        let pressed = self.pressed();
        for &(item, r) in cells {
            let (x, y) = ((r.x - rect.x) as f32, (r.y - rect.y) as f32 + slide);
            let (cw, ch) = (r.w as f32, r.h as f32);
            let active = match item {
                Item::Bold => style.bold,
                Item::Italic => style.italic,
                Item::Underline => style.underline,
                Item::Strike => style.strike,
                Item::Font => self.fonts_open.is_some(),
                _ => false,
            };
            let bg = if pressed == Some(item) {
                0.26
            } else if active {
                0.18
            } else if self.hover == Some(item) {
                0.10
            } else if item == Item::Font {
                0.05
            } else {
                0.0
            };
            if bg > 0.0 {
                c.round_rect(x + 2.0 * s, y + 2.0 * s, cw - 4.0 * s, ch - 4.0 * s, 7.0 * s, rgba(255, 255, 255, bg * a));
            }
            let ink = faded(WHITE, a);
            let mid = (x + cw / 2.0, y + ch / 2.0);
            let p = |dx: f32, dy: f32| (mid.0 + dx * s, mid.1 + dy * s);
            let glyph = |text: &str, bold: bool, italic: bool| {
                let look = Style { bold, italic, ..Style::default() };
                self.fonts.draw_glyph(c, text, &look, 15.0 * s, mid, ink);
            };
            match item {
                Item::Font => {
                    let face = Style { bold: false, italic: false, underline: false, strike: false, ..style.clone() };
                    let box_ = [x + 9.0 * s, y, cw - 26.0 * s, ch];
                    self.fonts.draw_line(c, &style.font, &face, 13.0 * s, box_, ink);
                    let (vx, vy) = (x + cw - 12.0 * s, mid.1);
                    c.polyline(&[(vx - 3.5 * s, vy - 1.5 * s), (vx, vy + 2.0 * s), (vx + 3.5 * s, vy - 1.5 * s)], 1.4 * s, faded(WHITE, 0.6 * a));
                }
                Item::Smaller => c.polyline(&[p(-4.5, 0.0), p(4.5, 0.0)], 1.6 * s, ink),
                Item::Bigger => {
                    c.polyline(&[p(-4.5, 0.0), p(4.5, 0.0)], 1.6 * s, ink);
                    c.polyline(&[p(0.0, -4.5), p(0.0, 4.5)], 1.6 * s, ink);
                    // the size, between the two buttons
                    let label = (x - self.px(15.0), mid.1);
                    self.fonts.draw_centred(c, &format!("{size}"), 12.0 * s, label, faded(WHITE, 0.85 * a));
                }
                Item::Bold => glyph("B", true, false),
                Item::Italic => {
                    let look = Style { font: Arc::from("Georgia"), bold: false, italic: true, ..Style::default() };
                    self.fonts.draw_glyph(c, "I", &look, 16.0 * s, mid, ink);
                }
                Item::Underline => {
                    glyph("U", false, false);
                    c.polyline(&[p(-5.0, 7.5), p(5.0, 7.5)], 1.3 * s, ink);
                }
                Item::Strike => {
                    glyph("S", false, false);
                    c.polyline(&[p(-6.0, 0.5), p(6.0, 0.5)], 1.3 * s, ink);
                }
                Item::Align => {
                    for (i, len) in [12.0, 7.0, 10.0].into_iter().enumerate() {
                        let x0 = match style.align {
                            Align::Left => -6.0,
                            Align::Centre => -len / 2.0,
                            Align::Right => 6.0 - len,
                        };
                        let yy = -4.5 + i as f32 * 4.5;
                        c.polyline(&[p(x0, yy), p(x0 + len, yy)], 1.5 * s, ink);
                    }
                }
                Item::VAlign => {
                    // a box, with the lines at its top, middle or bottom
                    c.round_rect_outline([mid.0 - 7.0 * s, mid.1 - 7.0 * s, 14.0 * s, 14.0 * s], 2.0 * s, 1.2 * s, faded(WHITE, 0.55 * a));
                    let y0 = match style.valign {
                        VAlign::Top => -4.0,
                        VAlign::Middle => -1.5,
                        VAlign::Bottom => 1.0,
                    };
                    for (i, len) in [8.0, 5.0].into_iter().enumerate() {
                        let yy = y0 + i as f32 * 3.0;
                        c.polyline(&[p(-4.0, yy), p(-4.0 + len, yy)], 1.5 * s, ink);
                    }
                }
                Item::Back => self.draw_back_icon(c, style.back, mid, s, a, color),
                _ => {}
            }
        }
    }

    /// A little "A" in backdrop `back`.
    fn draw_back_icon(&self, c: &Canvas, back: Back, mid: (f32, f32), s: f32, a: f32, color: Ink) {
        let look = Style::default();
        let px = 14.0 * s;
        let (w, h) = (17.0 * s, 15.0 * s);
        let pill = |fill| c.round_rect(mid.0 - w / 2.0, mid.1 - h / 2.0, w, h, 4.0 * s, fill);
        match back {
            Back::Outline => {
                for (dx, dy) in [(1.2, 0.0), (-1.2, 0.0), (0.0, 1.2), (0.0, -1.2)] {
                    self.fonts.draw_glyph(c, "A", &look, px, (mid.0 + dx * s, mid.1 + dy * s), rgba(0, 0, 0, 0.9 * a));
                }
                self.fonts.draw_glyph(c, "A", &look, px, mid, faded(WHITE, a));
            }
            Back::Shade => {
                pill(rgba(0, 0, 0, 0.75 * a));
                c.round_rect_outline([mid.0 - w / 2.0, mid.1 - h / 2.0, w, h], 4.0 * s, 1.0, rgba(255, 255, 255, 0.25 * a));
                self.fonts.draw_glyph(c, "A", &look, px, mid, faded(WHITE, a));
            }
            Back::Tag => {
                pill(faded(color.d2d(), a));
                self.fonts.draw_glyph(c, "A", &look, px, mid, faded(color.ink_on(), a));
            }
            Back::None => self.fonts.draw_glyph(c, "A", &look, px, mid, faded(WHITE, 0.75 * a)),
        }
    }

    /// The font list, drawn onto `c` (which shows its box).
    pub(super) fn draw_font_list(&self, c: &Canvas, list: &FontList) {
        let s = self.scale;
        let a = self.fonts_open.as_ref().map_or(1.0, |f| progress(f.since, PANEL_MS));
        let o = (list.rect.x, list.rect.y);
        let local = |r: Rect| [(r.x - o.0) as f32, (r.y - o.1) as f32, r.w as f32, r.h as f32];
        let (w, h) = (list.rect.w as f32, list.rect.h as f32);
        c.round_rect(0.0, 0.0, w, h, 10.0 * s, faded(BAR_BG, a));
        let [sx, sy, sw, sh] = local(list.search);
        c.round_rect(sx, sy + 2.0 * s, sw, sh - 4.0 * s, 7.0 * s, rgba(255, 255, 255, 0.07 * a));
        // a magnifier, then what's been typed (or a faint hint)
        let (mx, my) = (sx + 13.0 * s, sy + sh / 2.0);
        c.ring((mx - 1.0 * s, my - 1.0 * s), 4.0 * s, 1.4 * s, faded(WHITE, 0.6 * a));
        c.polyline(&[(mx + 2.0 * s, my + 2.0 * s), (mx + 5.0 * s, my + 5.0 * s)], 1.4 * s, faded(WHITE, 0.6 * a));
        let query = self.fonts_open.as_ref().map_or("", |f| f.query.as_str());
        let ui = Style { bold: false, ..Style::default() };
        let text_box = [sx + 26.0 * s, sy, sw - 30.0 * s, sh];
        if query.is_empty() {
            self.fonts.draw_line(c, "type to search", &ui, 13.0 * s, text_box, rgba(255, 255, 255, 0.38 * a));
        } else {
            self.fonts.draw_line(c, query, &ui, 13.0 * s, text_box, faded(WHITE, a));
            let x = text_box[0] + self.fonts.measure(query, 13.0 * s) + 1.0 * s;
            c.fill(x, sy + sh * 0.27, 1.5 * s, sh * 0.46, faded(WHITE, a));
        }
        let empty = if list.loading { "reading fonts…" } else { "no such font" };
        if list.rows.is_empty() {
            let r = [sx, sy + sh, sw, h - sh - sy * 2.0];
            self.fonts.draw_line(c, empty, &ui, 13.0 * s, [r[0] + 10.0 * s, r[1], r[2], r[3]], rgba(255, 255, 255, 0.45 * a));
        }
        let current = self.shown_style().0.font;
        for (item, r, name, family) in &list.rows {
            let [x, y, rw, rh] = local(*r);
            if self.hover == Some(*item) {
                c.round_rect(x, y + 1.0 * s, rw, rh - 2.0 * s, 6.0 * s, rgba(255, 255, 255, 0.10 * a));
            }
            // in its own face once that's loaded (symbol fonts would spell their name in pictures)
            let own = fonts::ready(*family) == Some(false);
            self.fonts.draw_preview(c, name, own, 15.0 * s, [x + 10.0 * s, y, rw - 34.0 * s, rh], faded(WHITE, a));
            if **name == *current {
                let (cx, cy) = (x + rw - 14.0 * s, y + rh / 2.0);
                c.polyline(&[(cx - 4.5 * s, cy), (cx - 1.5 * s, cy + 3.0 * s), (cx + 4.5 * s, cy - 3.5 * s)], 1.6 * s, faded(WHITE, a));
            }
        }
        // the next page starts loading before it's scrolled to
        let (_, found) = self.found_fonts();
        let scroll = self.fonts_open.as_ref().map_or(0, |f| f.scroll);
        for &family in found.iter().skip(scroll + ROWS).take(ROWS) {
            fonts::ready(family);
        }
    }
}
