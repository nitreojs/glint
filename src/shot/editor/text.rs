use std::cell::RefCell;

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::DirectWrite::*;
use windows::core::{BOOL, w};

use crate::platform::gfx::{Canvas, rgba};

const SHADE: D2D1_COLOR_F = rgba(0, 0, 0, 0.6);
const FORMATS: usize = 8;

/// A caret operation on a text being typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Op {
    Insert(char),
    Back,
    Delete,
    Left,
    Right,
    Home,
    End,
    Up,
    Down,
}

fn line_start(t: &str, at: usize) -> usize {
    t[..at].rfind('\n').map_or(0, |i| i + 1)
}

fn line_end(t: &str, at: usize) -> usize {
    t[at..].find('\n').map_or(t.len(), |i| at + i)
}

fn prev(t: &str, at: usize) -> usize {
    t[..at].char_indices().next_back().map_or(0, |(i, _)| i)
}

fn next(t: &str, at: usize) -> usize {
    t[at..].chars().next().map_or(at, |c| at + c.len_utf8())
}

fn at_column(t: &str, start: usize, column: usize) -> usize {
    let end = line_end(t, start);
    t[start..end].char_indices().nth(column).map_or(end, |(i, _)| start + i)
}

/// Applies `op` to `text` with the caret at byte offset `caret`; returns the new caret.
pub(super) fn edit(text: &mut String, caret: usize, op: Op) -> usize {
    let t = text.as_str();
    let column = || t[line_start(t, caret)..caret].chars().count();
    match op {
        Op::Insert(c) => {
            text.insert(caret, c);
            caret + c.len_utf8()
        }
        Op::Back => {
            let p = prev(t, caret);
            text.replace_range(p..caret, "");
            p
        }
        Op::Delete => {
            let n = next(t, caret);
            text.replace_range(caret..n, "");
            caret
        }
        Op::Left => prev(t, caret),
        Op::Right => next(t, caret),
        Op::Home => line_start(t, caret),
        Op::End => line_end(t, caret),
        Op::Up => match line_start(t, caret) {
            0 => 0,
            start => at_column(t, line_start(t, start - 1), column()),
        },
        Op::Down => match line_end(t, caret) {
            end if end == t.len() => end,
            end => at_column(t, end + 1, column()),
        },
    }
}

fn utf16_len(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

/// Byte offset of UTF-16 position `pos` in `t`.
fn byte_at(t: &str, pos: u32) -> usize {
    let mut units = 0;
    for (i, c) in t.char_indices() {
        if units >= pos {
            return i;
        }
        units += c.len_utf16() as u32;
    }
    t.len()
}

/// Lays out mark text: Segoe UI Semibold, top-left aligned, lines broken only at newlines.
pub(super) struct Typesetter {
    dwrite: IDWriteFactory,
    formats: RefCell<Vec<(u32, IDWriteTextFormat)>>,
}

impl Typesetter {
    pub(super) fn new() -> Typesetter {
        // the shared factory is a process singleton the picker's painter has already created
        let dwrite = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.expect("DirectWrite shared factory");
        Typesetter { dwrite, formats: RefCell::new(Vec::new()) }
    }

    fn format(&self, px: f32) -> Option<IDWriteTextFormat> {
        let key = px.to_bits();
        let mut formats = self.formats.borrow_mut();
        if let Some((_, f)) = formats.iter().find(|(k, _)| *k == key) {
            return Some(f.clone());
        }
        let f = unsafe {
            let f = self
                .dwrite
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_SEMI_BOLD,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    px,
                    w!("en-us"),
                )
                .ok()?;
            let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            f
        };
        if formats.len() == FORMATS {
            formats.remove(0);
        }
        formats.push((key, f.clone()));
        Some(f)
    }

    fn layout(&self, text: &str, px: f32) -> Option<IDWriteTextLayout> {
        let s: Vec<u16> = text.encode_utf16().collect();
        unsafe { self.dwrite.CreateTextLayout(&s, &self.format(px)?, 100_000.0, 100_000.0).ok() }
    }

    /// Width and height of `text` at `px` physical pixels; an empty text is one line high.
    pub(super) fn extent(&self, text: &str, px: f32) -> (f32, f32) {
        let Some(layout) = self.layout(text, px) else { return (0.0, px) };
        let mut m = DWRITE_TEXT_METRICS::default();
        match unsafe { layout.GetMetrics(&mut m) } {
            Ok(()) => (m.widthIncludingTrailingWhitespace, m.height),
            Err(_) => (0.0, px),
        }
    }

    /// Top of the caret before byte `caret`, and its height, relative to the text's corner.
    pub(super) fn caret(&self, text: &str, px: f32, caret: usize) -> (f32, f32, f32) {
        let Some(layout) = self.layout(text, px) else { return (0.0, 0.0, px) };
        let (mut x, mut y, mut m) = (0.0, 0.0, DWRITE_HIT_TEST_METRICS::default());
        match unsafe { layout.HitTestTextPosition(utf16_len(&text[..caret]), false, &mut x, &mut y, &mut m) } {
            Ok(()) => (x, m.top, m.height),
            Err(_) => (0.0, 0.0, px),
        }
    }

    /// The byte offset a click at `(x, y)` (relative to the text's corner) puts the caret at.
    pub(super) fn hit(&self, text: &str, px: f32, (x, y): (f32, f32)) -> usize {
        let Some(layout) = self.layout(text, px) else { return text.len() };
        let (mut trailing, mut inside, mut m) = (BOOL(0), BOOL(0), DWRITE_HIT_TEST_METRICS::default());
        match unsafe { layout.HitTestPoint(x, y, &mut trailing, &mut inside, &mut m) } {
            Ok(()) => byte_at(text, m.textPosition + if trailing.as_bool() { m.length } else { 0 }),
            Err(_) => text.len(),
        }
    }

    /// `text` with its corner at `at`, inside a dark outline `reach` pixels wide.
    pub(super) fn draw_outlined(
        &self,
        c: &Canvas,
        text: &str,
        px: f32,
        at: (f32, f32),
        color: D2D1_COLOR_F,
        reach: f32,
    ) {
        let Some(layout) = self.layout(text, px) else { return };
        let d = reach * std::f32::consts::FRAC_1_SQRT_2;
        for (dx, dy) in [(reach, 0.0), (-reach, 0.0), (0.0, reach), (0.0, -reach), (d, d), (d, -d), (-d, d), (-d, -d)] {
            c.text_layout(&layout, (at.0 + dx, at.1 + dy), SHADE);
        }
        c.text_layout(&layout, at, color);
    }

    /// One line of `text` centred on `centre`.
    pub(super) fn draw_centred(&self, c: &Canvas, text: &str, px: f32, centre: (f32, f32), color: D2D1_COLOR_F) {
        let Some(layout) = self.layout(text, px) else { return };
        let (w, h) = self.extent(text, px);
        c.text_layout(&layout, (centre.0 - w / 2.0, centre.1 - h / 2.0), color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, caret: usize, ops: &[Op]) -> (String, usize) {
        let mut t = text.to_owned();
        let mut c = caret;
        for &op in ops {
            c = edit(&mut t, c, op);
        }
        (t, c)
    }

    #[test]
    fn caret_edits_text() {
        assert_eq!(run("", 0, &[Op::Insert('h'), Op::Insert('é'), Op::Insert('y')]), ("héy".into(), 4));
        assert_eq!(run("héy", 4, &[Op::Left, Op::Back]), ("hy".into(), 1));
        assert_eq!(run("héy", 0, &[Op::Right, Op::Delete]), ("hy".into(), 1));
        assert_eq!(run("ab", 0, &[Op::Back, Op::Left]), ("ab".into(), 0));
        assert_eq!(run("ab", 2, &[Op::Delete, Op::Right]), ("ab".into(), 2));
        assert_eq!(run("a😀b", 5, &[Op::Back]), ("ab".into(), 1));
    }

    #[test]
    fn caret_moves_between_lines() {
        let t = "first\nab\nthird";
        assert_eq!(run(t, 4, &[Op::Down]).1, 8, "column clamps to the shorter line");
        assert_eq!(run(t, 4, &[Op::Down, Op::Down]).1, 11, "the column is the shorter line's end");
        assert_eq!(run(t, 13, &[Op::Up]).1, 8);
        assert_eq!(run(t, 13, &[Op::Up, Op::Up]).1, 2);
        assert_eq!(run(t, 2, &[Op::Up]).1, 0);
        assert_eq!(run(t, 13, &[Op::Down]).1, t.len());
        assert_eq!(run(t, 7, &[Op::Home]).1, 6);
        assert_eq!(run(t, 7, &[Op::End]).1, 8);
        assert_eq!(run(t, 8, &[Op::Insert('\n'), Op::Insert('c')]), ("first\nab\nc\nthird".into(), 10));
    }

    #[test]
    fn utf16_positions_map_to_bytes() {
        let t = "a😀é";
        assert_eq!(utf16_len(t), 4);
        assert_eq!(byte_at(t, 1), 1);
        assert_eq!(byte_at(t, 3), 5);
        assert_eq!(byte_at(t, 4), 7);
        assert_eq!(byte_at(t, 9), 7);
    }
}
