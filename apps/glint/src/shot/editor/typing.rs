use glint_core::geom::rect::Rect;

use super::marks::{Mark, Tool};
use super::text::{Op, edit};
use super::{Editor, Event};

/// A text mark being typed into.
pub(super) struct Typing {
    pub(super) index: usize,
    /// Byte offset into the mark's text.
    caret: usize,
    /// The mark before this session; `None` for a new one.
    from: Option<Mark>,
}

impl Editor {
    /// Brings a mark's measured geometry up to date with its text and size.
    pub(super) fn refit(&self, m: &mut Mark) {
        match m {
            Mark::Text { text, size, extent, .. } => *extent = self.fonts.extent(text, *size * self.scale),
            Mark::Step { size, r, .. } => *r = *size * self.scale / 2.0,
            _ => {}
        }
    }

    /// A new text whose first line is centred on `p`.
    pub(super) fn start_typing(&mut self, p: (i32, i32)) -> Event {
        let size = self.strength[Tool::Text as usize];
        let (_, line) = self.fonts.extent("", size * self.scale);
        let mut m = Mark::new(Tool::Text, (p.0, (p.1 as f32 - line / 2.0).round() as i32), self.color, size);
        self.refit(&mut m);
        self.marks.push(m);
        let index = self.marks.len() - 1;
        self.picked = Some(index);
        self.typing = Some(Typing { index, caret: 0, from: None });
        self.touch();
        Event::Repaint
    }

    /// Types into the existing text mark `index`, with the caret at `p`.
    pub(super) fn retype(&mut self, index: usize, p: (i32, i32)) -> Event {
        self.drag = None;
        self.picked = Some(index);
        let from = Some(self.marks[index].clone());
        self.typing = Some(Typing { index, caret: 0, from });
        self.place_caret(p)
    }

    pub(super) fn place_caret(&mut self, p: (i32, i32)) -> Event {
        let Some(t) = self.typing.as_mut() else { return Event::None };
        let Mark::Text { at, text, size, .. } = &self.marks[t.index] else { return Event::None };
        t.caret = self.fonts.hit(text, size * self.scale, (p.0 as f32 - at.0, p.1 as f32 - at.1));
        Event::Repaint
    }

    /// Ends typing: an emptied text goes away, a changed one becomes one undo step. Returns
    /// whether anything was being typed.
    pub(super) fn commit(&mut self) -> bool {
        let Some(Typing { index, from, .. }) = self.typing.take() else { return false };
        self.surrogate = None;
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
        self.touch();
        true
    }

    fn type_op(&mut self, op: Op) -> Event {
        let Some(t) = self.typing.as_mut() else { return Event::None };
        let Mark::Text { text, .. } = &mut self.marks[t.index] else { return Event::None };
        t.caret = edit(text, t.caret, op);
        let index = t.index;
        let mut m = self.marks[index].clone();
        self.refit(&mut m);
        self.marks[index] = m;
        self.touch();
        Event::Repaint
    }

    /// A key while typing: `None` lets shortcuts with Ctrl through; other keys are text (WM_CHAR).
    pub(super) fn typing_key(&mut self, vk: u32, ctrl: bool) -> Option<Event> {
        let op = match vk {
            0x08 => Op::Back,
            0x2E => Op::Delete,
            0x25 => Op::Left,
            0x27 => Op::Right,
            0x24 => Op::Home,
            0x23 => Op::End,
            0x26 => Op::Up,
            0x28 => Op::Down,
            _ if ctrl => return None,
            _ => return Some(Event::None),
        };
        Some(self.type_op(op))
    }

    /// A WM_CHAR code unit. IME composition isn't shown; its result arrives here as characters.
    pub(crate) fn char(&mut self, unit: u16) -> Event {
        if self.typing.is_none() {
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
        match c {
            Some('\r') => self.type_op(Op::Insert('\n')),
            Some(c) if !c.is_control() => self.type_op(Op::Insert(c)),
            _ => Event::None,
        }
    }

    /// The caret (client coordinates) while typing.
    pub(super) fn caret_rect(&self) -> Option<Rect> {
        let t = self.typing.as_ref()?;
        let Mark::Text { at, text, size, .. } = &self.marks[t.index] else { return None };
        let (x, y, h) = self.fonts.caret(text, size * self.scale, t.caret);
        let w = self.px(2.0).max(1.0) as i32;
        Some(Rect { x: (at.0 + x).round() as i32 - w / 2, y: (at.1 + y).round() as i32, w, h: h.round() as i32 })
    }
}
