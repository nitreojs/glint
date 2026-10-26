use glint_core::geom::rect::Rect;

use super::marks::{Edges, Mark, Tool};
use super::text::{self, Style, padding, remember_style};
use super::{Editor, Event};

/// A text mark being typed into.
pub(super) struct Typing {
    pub(super) index: usize,
    /// Byte offsets into the mark's text: the caret, and the other end of the selection (the
    /// caret itself when nothing is selected).
    pub(super) caret: usize,
    pub(super) anchor: usize,
    /// The mark before this session; `None` for a new one.
    from: Option<Mark>,
    /// Undo steps inside the session; the whole session is one step once it's committed.
    undo: Vec<Undo>,
    redo: Vec<Undo>,
    /// More typing joins the last undo step (until a space, a caret move or another edit).
    open: bool,
}

#[derive(Clone)]
struct Undo {
    mark: Mark,
    caret: usize,
    anchor: usize,
}

impl Typing {
    pub(super) fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }
}

/// What an editing key does to the text.
#[derive(Clone, Copy)]
enum Erase {
    Back,
    Forward,
}

/// The text being typed: its text, style, font size in physical pixels, wrap width and the
/// corner its letters start at.
type Typed<'a> = (&'a str, &'a Style, f32, Option<f32>, (f32, f32));

impl Editor {
    /// Brings a mark's measured geometry up to date with its text, style and size.
    pub(super) fn refit(&self, m: &mut Mark) {
        match m {
            Mark::Text { text, size, style, width, extent, .. } => {
                let px = *size * self.scale;
                let (w, h) = self.fonts.extent(text, style, px, *width);
                let (pad_x, pad_y) = padding(style.back, px);
                *extent = (w + 2.0 * pad_x, h + 2.0 * pad_y);
            }
            Mark::Step { size, r, .. } => *r = *size * self.scale / 2.0,
            _ => {}
        }
    }

    /// Where a text mark's letters start (its corner inside the padding), client coordinates.
    pub(super) fn text_origin(&self, m: &Mark) -> (f32, f32) {
        match m {
            Mark::Text { at, size, style, .. } => {
                let (pad_x, pad_y) = padding(style.back, size * self.scale);
                (at.0 + pad_x, at.1 + pad_y)
            }
            _ => (0.0, 0.0),
        }
    }

    fn typed(&self) -> Option<Typed<'_>> {
        let t = self.typing.as_ref()?;
        let m = &self.marks[t.index];
        let Mark::Text { text, style, size, width, .. } = m else { return None };
        Some((text, style, size * self.scale, *width, self.text_origin(m)))
    }

    /// A new text whose first line is centred on `p`, in the last style used.
    pub(super) fn start_typing(&mut self, p: (i32, i32)) -> Event {
        let size = self.strength[Tool::Text as usize];
        let px = size * self.scale;
        let (_, line) = self.fonts.extent("", &self.text_style, px, None);
        let (pad_x, pad_y) = padding(self.text_style.back, px);
        let corner = ((p.0 as f32 - pad_x).round() as i32, (p.1 as f32 - line / 2.0 - pad_y).round() as i32);
        let mut m = Mark::new(Tool::Text, corner, self.color, size);
        if let Mark::Text { style, .. } = &mut m {
            *style = self.text_style.clone();
        }
        self.refit(&mut m);
        let before = self.ink();
        self.marks.push(m);
        let index = self.marks.len() - 1;
        self.picked = Some(index);
        self.typing = Some(Typing { index, caret: 0, anchor: 0, from: None, undo: Vec::new(), redo: Vec::new(), open: false });
        self.repaint_ink(before);
        Event::Repaint
    }

    /// Types into the existing text mark `index`, with the caret at `p`.
    pub(super) fn retype(&mut self, index: usize, p: (i32, i32)) -> Event {
        self.drag = None;
        self.picked = Some(index);
        if let Mark::Text { style, size, color, .. } = &self.marks[index] {
            (self.text_style, self.color) = (style.clone(), *color);
            self.strength[Tool::Text as usize] = *size;
        }
        let from = Some(self.marks[index].clone());
        self.typing = Some(Typing { index, caret: 0, anchor: 0, from, undo: Vec::new(), redo: Vec::new(), open: false });
        self.place_caret(p, false)
    }

    /// The byte offset a click at `p` (client coordinates) puts the caret at.
    fn caret_at(&self, p: (i32, i32)) -> Option<usize> {
        let (text, style, px, width, o) = self.typed()?;
        Some(self.fonts.hit(text, style, px, width, (p.0 as f32 - o.0, p.1 as f32 - o.1)))
    }

    /// A click (or a drag, `extend`) at `p` moves the caret there.
    pub(super) fn place_caret(&mut self, p: (i32, i32), extend: bool) -> Event {
        let Some(at) = self.caret_at(p) else { return Event::None };
        let Some(t) = self.typing.as_mut() else { return Event::None };
        if (t.caret, t.anchor) == (at, if extend { t.anchor } else { at }) {
            return Event::None;
        }
        t.caret = at;
        if !extend {
            t.anchor = at;
        }
        t.open = false;
        Event::Repaint
    }

    /// A double-click at `p` selects the word there.
    pub(super) fn select_word(&mut self, p: (i32, i32)) -> Event {
        let Some(at) = self.caret_at(p) else { return Event::None };
        let Some((text, ..)) = self.typed() else { return Event::None };
        let (start, end) = text::word_at(text, at);
        let Some(t) = self.typing.as_mut() else { return Event::None };
        (t.anchor, t.caret, t.open) = (start, end, false);
        Event::Repaint
    }

    /// Ends typing: an emptied text goes away, a changed one becomes one undo step. Returns
    /// whether anything was being typed.
    pub(super) fn commit(&mut self) -> bool {
        let before_ink = self.ink();
        let Some(Typing { index, from, .. }) = self.typing.take() else { return false };
        self.surrogate = None;
        self.fonts_open = None;
        let mut before = self.marks.clone();
        let picked = match from {
            Some(m) => {
                before[index] = m;
                Some(index)
            }
            None => {
                before.remove(index);
                None
            }
        };
        if !self.marks[index].worth_keeping() {
            self.marks.remove(index);
            self.picked = None;
        }
        if before != self.marks {
            self.record((before, picked));
        }
        self.repaint_ink(before_ink);
        self.marks_rev += 1;
        true
    }

    /// Changes the text being typed with `f(text, caret, anchor)`; `group` lets the change join
    /// the last undo step.
    fn edit_text(&mut self, group: bool, f: impl FnOnce(&mut String, &mut usize, &mut usize)) -> Event {
        let before = self.ink();
        let Some(t) = self.typing.as_mut() else { return Event::None };
        let Mark::Text { text, .. } = &mut self.marks[t.index] else { return Event::None };
        let (old_text, old_caret, old_anchor) = (text.clone(), t.caret, t.anchor);
        f(text, &mut t.caret, &mut t.anchor);
        if *text == old_text {
            t.open = false;
            return if (t.caret, t.anchor) == (old_caret, old_anchor) { Event::None } else { Event::Repaint };
        }
        let mut was = self.marks[t.index].clone();
        if let Mark::Text { text, .. } = &mut was {
            *text = old_text;
        }
        if !(group && t.open) {
            t.undo.push(Undo { mark: was, caret: old_caret, anchor: old_anchor });
        }
        t.redo.clear();
        t.open = group;
        let index = t.index;
        let mut m = self.marks[index].clone();
        self.refit(&mut m);
        self.marks[index] = m;
        self.repaint_ink(before);
        self.marks_rev += 1;
        Event::Repaint
    }

    /// Replaces the selection with `s` (newlines as `\n`).
    pub(super) fn insert(&mut self, s: &str) -> Event {
        let group = !s.contains(char::is_whitespace) && s.chars().count() == 1;
        self.edit_text(group, |text, caret, anchor| {
            let (a, b) = (*caret.min(anchor), *caret.max(anchor));
            text.replace_range(a..b, s);
            *caret = a + s.len();
            *anchor = *caret;
        })
    }

    fn erase(&mut self, dir: Erase, word: bool) -> Event {
        self.edit_text(false, |text, caret, anchor| {
            let (mut a, mut b) = (*caret.min(anchor), *caret.max(anchor));
            if a == b {
                match (dir, word) {
                    (Erase::Back, false) => a = text::prev(text, a),
                    (Erase::Back, true) => a = text::word_left(text, a),
                    (Erase::Forward, false) => b = text::next(text, b),
                    (Erase::Forward, true) => b = text::word_right(text, b),
                }
            }
            text.replace_range(a..b, "");
            (*caret, *anchor) = (a, a);
        })
    }

    /// Moves the caret to `to` (from the text and the caret); `extend` keeps the anchor.
    fn move_caret(&mut self, extend: bool, to: impl FnOnce(&str, usize, (usize, usize)) -> usize) -> Event {
        let Some(t) = self.typing.as_ref() else { return Event::None };
        let Mark::Text { text, .. } = &self.marks[t.index] else { return Event::None };
        let at = to(text, t.caret, t.selection());
        let t = self.typing.as_mut().expect("typing above");
        let anchor = if extend { t.anchor } else { at };
        if (t.caret, t.anchor) == (at, anchor) {
            return Event::None;
        }
        (t.caret, t.anchor, t.open) = (at, anchor, false);
        Event::Repaint
    }

    /// The caret a line up (`-1`) or down (`1`) from where it is, at the same x.
    fn line_from(&self, dir: f32) -> Option<usize> {
        let t = self.typing.as_ref()?;
        let (text, style, px, width, _) = self.typed()?;
        let (x, y, h) = self.fonts.caret(text, style, px, width, t.caret);
        let y = if dir < 0.0 { y - h / 2.0 } else { y + h * 1.5 };
        let (_, height) = self.fonts.extent(text, style, px, width);
        if y < 0.0 {
            return Some(0);
        }
        if y > height {
            return Some(text.len());
        }
        Some(self.fonts.hit(text, style, px, width, (x, y)))
    }

    pub(super) fn selected_text(&self) -> Option<String> {
        let t = self.typing.as_ref()?;
        let Mark::Text { text, .. } = &self.marks[t.index] else { return None };
        let (a, b) = t.selection();
        (a < b).then(|| text[a..b].to_owned())
    }

    fn select_all(&mut self) -> Event {
        let Some((text, ..)) = self.typed() else { return Event::None };
        let len = text.len();
        let Some(t) = self.typing.as_mut() else { return Event::None };
        (t.anchor, t.caret, t.open) = (0, len, false);
        Event::Repaint
    }

    /// Undoes (`back`) or redoes an edit inside the session; `None` when there's nothing left,
    /// so the editor's own undo takes over.
    fn travel_text(&mut self, back: bool) -> Option<Event> {
        let before = self.ink();
        let t = self.typing.as_mut()?;
        let (from, to) = if back { (&mut t.undo, &mut t.redo) } else { (&mut t.redo, &mut t.undo) };
        let step = from.pop()?;
        to.push(Undo { mark: self.marks[t.index].clone(), caret: t.caret, anchor: t.anchor });
        (t.caret, t.anchor, t.open) = (step.caret, step.anchor, false);
        self.marks[t.index] = step.mark;
        self.repaint_ink(before);
        self.marks_rev += 1;
        Some(Event::Repaint)
    }

    /// Pushes the typed mark as it is now as an undo step inside the session (before a move,
    /// a resize or a style change).
    pub(super) fn typing_checkpoint(&mut self, index: usize, was: Mark) {
        if let Some(t) = self.typing.as_mut().filter(|t| t.index == index) {
            t.undo.push(Undo { mark: was, caret: t.caret, anchor: t.anchor });
            t.redo.clear();
            t.open = false;
        }
    }

    /// Whether the text mark `index` is being typed into.
    pub(super) fn typing_into(&self, index: usize) -> bool {
        self.typing.as_ref().is_some_and(|t| t.index == index)
    }

    /// A key while typing: `None` lets it through to the editor's shortcuts; other keys without
    /// Ctrl are text (WM_CHAR).
    pub(super) fn typing_key(&mut self, vk: u32, ctrl: bool, shift: bool) -> Option<Event> {
        const VK_BACK: u32 = 0x08;
        const VK_END: u32 = 0x23;
        const VK_HOME: u32 = 0x24;
        const VK_LEFT: u32 = 0x25;
        const VK_UP: u32 = 0x26;
        const VK_RIGHT: u32 = 0x27;
        const VK_DOWN: u32 = 0x28;
        const VK_DELETE: u32 = 0x2E;
        Some(match (vk, ctrl) {
            (VK_BACK, _) => self.erase(Erase::Back, ctrl),
            (VK_DELETE, _) => self.erase(Erase::Forward, ctrl),
            // without Shift, an arrow first collapses the selection to its side
            (VK_LEFT, _) => self.move_caret(shift, |t, at, (a, b)| match (ctrl, shift) {
                (true, _) => text::word_left(t, at),
                (false, false) if a != b => a,
                _ => text::prev(t, at),
            }),
            (VK_RIGHT, _) => self.move_caret(shift, |t, at, (a, b)| match (ctrl, shift) {
                (true, _) => text::word_right(t, at),
                (false, false) if a != b => b,
                _ => text::next(t, at),
            }),
            (VK_HOME, _) => self.move_caret(shift, |t, at, _| if ctrl { 0 } else { text::line_start(t, at) }),
            (VK_END, _) => self.move_caret(shift, |t, at, _| if ctrl { t.len() } else { text::line_end(t, at) }),
            (VK_UP | VK_DOWN, false) => {
                let to = self.line_from(if vk == VK_UP { -1.0 } else { 1.0 })?;
                self.move_caret(shift, |_, _, _| to)
            }
            (0x41, true) => self.select_all(), // A
            (0x43, true) => {
                // C: with nothing selected, Ctrl+C copies the shot
                let text = self.selected_text()?;
                self.clip.set_text(&text);
                Event::None
            }
            (0x58, true) => match self.selected_text() {
                // X
                Some(text) => {
                    self.clip.set_text(&text);
                    self.insert("")
                }
                None => Event::None,
            },
            (0x56, true) => match self.clip.text() {
                // V
                Some(text) => self.insert(&text.replace("\r\n", "\n").replace('\r', "\n")),
                None => Event::None,
            },
            (0x5A, true) => self.travel_text(!shift)?, // Z
            (0x59, true) => self.travel_text(false)?,  // Y
            (_, true) => return None,
            _ => Event::None,
        })
    }

    /// A WM_CHAR code unit. IME composition isn't shown; its result arrives here as characters.
    pub(crate) fn char(&mut self, unit: u16) -> Event {
        if self.typing.is_none() && self.fonts_open.is_none() {
            return Event::None;
        }
        let c = match unit {
            0xD800..=0xDBFF => {
                self.surrogate = Some(unit);
                return Event::None;
            }
            0xDC00..=0xDFFF => match self.surrogate.take() {
                Some(high) => char::decode_utf16([high, unit]).next().and_then(Result::ok),
                None => None,
            },
            _ => char::from_u32(unit as u32),
        };
        if self.fonts_open.is_some() {
            return match c {
                Some(c) if !c.is_control() => self.font_query(Some(c)),
                _ => Event::None,
            };
        }
        match c {
            Some('\r') => self.insert("\n"),
            Some(c) if !c.is_control() => self.insert(c.encode_utf8(&mut [0; 4])),
            _ => Event::None,
        }
    }

    /// The caret (client coordinates) while typing.
    pub(super) fn caret_rect(&self) -> Option<Rect> {
        let t = self.typing.as_ref()?;
        let (text, style, px, width, o) = self.typed()?;
        let (x, y, h) = self.fonts.caret(text, style, px, width, t.caret);
        let w = self.px(2.0).max(1.0) as i32;
        Some(Rect { x: (o.0 + x).round() as i32 - w / 2, y: (o.1 + y).round() as i32, w, h: h.round() as i32 })
    }

    /// The selected part of the text being typed, one box per line (client coordinates).
    pub(super) fn selection_rects(&self) -> Vec<Rect> {
        let Some(t) = self.typing.as_ref() else { return Vec::new() };
        let Some((text, style, px, width, o)) = self.typed() else { return Vec::new() };
        let boxes = self.fonts.range(text, style, px, width, t.selection());
        boxes
            .iter()
            .map(|[x, y, w, h]| {
                let (x0, y0) = ((o.0 + x).floor() as i32, (o.1 + y).floor() as i32);
                Rect::from_points(x0, y0, (o.0 + x + w).ceil() as i32, (o.1 + y + h).ceil() as i32)
            })
            .collect()
    }

    /// `from` (a text mark) with grip `e` dragged by `d`: the corners scale the font, the sides
    /// set the width it wraps at. The opposite corner or side stays put.
    pub(super) fn resize_text(&self, from: &Mark, e: Edges, (dx, dy): (i32, i32)) -> Mark {
        let mut m = from.clone();
        let b = from.bbox();
        let Mark::Text { size, style, width, at, .. } = &mut m else { return m };
        let spec = Tool::Text.strength().expect("text has sizes");
        let px = *size * self.scale;
        let (pad_x, _) = padding(style.back, px);
        let grow_x = if e.l { -dx } else { dx } as f32;
        if (e.l || e.r) && (e.t || e.b) {
            let grow_y = if e.t { -dy } else { dy } as f32;
            let k = ((b.w as f32 + grow_x) / b.w.max(1) as f32).max((b.h as f32 + grow_y) / b.h.max(1) as f32);
            let new = (*size * k).clamp(spec.min, spec.max);
            let k = new / *size;
            *size = (new * 2.0).round() / 2.0;
            *width = width.map(|w| w * k);
        } else {
            *width = Some((b.w as f32 - 2.0 * pad_x + grow_x).max(px));
        }
        let corner = *at;
        self.refit(&mut m);
        let nb = m.bbox();
        if let Mark::Text { at, .. } = &mut m {
            *at = (
                if e.l { b.right() as f32 - (nb.w as f32) } else { corner.0 },
                if e.t { b.bottom() as f32 - (nb.h as f32) } else { corner.1 },
            );
        }
        m
    }

    /// Applies `f` to the style of the text being typed or picked (one undo step), and of the
    /// texts to come.
    pub(super) fn restyle(&mut self, f: impl FnOnce(&mut Style)) -> Event {
        let target = self.typing.as_ref().map(|t| t.index).or(self.picked.filter(|&i| matches!(self.marks[i], Mark::Text { .. })));
        let mut style = match target.map(|i| &self.marks[i]) {
            Some(Mark::Text { style, .. }) => style.clone(),
            _ => self.text_style.clone(),
        };
        f(&mut style);
        self.text_style = style.clone();
        remember_style(&style, self.strength[Tool::Text as usize]);
        let Some(i) = target else { return Event::Repaint };
        if matches!(&self.marks[i], Mark::Text { style: s, .. } if *s == style) {
            return Event::Repaint;
        }
        let before = self.ink();
        let was = self.marks[i].clone();
        if self.typing_into(i) {
            self.typing_checkpoint(i, was);
        } else {
            self.remember();
        }
        let mut m = self.marks[i].clone();
        if let Mark::Text { style: s, .. } = &mut m {
            *s = style;
        }
        self.refit(&mut m);
        self.marks[i] = m;
        self.repaint_ink(before);
        self.repaint_ink(vec![self.marks[i].ink(self.scale)]);
        self.marks_rev += 1;
        Event::Repaint
    }
}
