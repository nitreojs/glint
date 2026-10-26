use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::DirectWrite::*;
use windows::core::{BOOL, HSTRING, w};

use glint_shell::gfx::{Canvas, rgba};

const SHADE: D2D1_COLOR_F = rgba(0, 0, 0, 0.6);
const PILL: D2D1_COLOR_F = rgba(12, 12, 16, 0.62);
const FORMATS: usize = 32;
const LAYOUTS: usize = 24;
/// Font list rows kept laid out; past this the cache starts over.
const PREVIEWS: usize = 256;
/// How long one paint may spend laying font list rows out in their own faces; the rest wait for
/// the next frame, in the UI's face meanwhile.
const PREVIEW_BUDGET: std::time::Duration = std::time::Duration::from_millis(4);
/// Wider than any text: lines are only broken at newlines unless a mark has a width.
const UNBOUNDED: f32 = 100_000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub(super) enum Align {
    #[default]
    Left,
    Centre,
    Right,
}

impl Align {
    pub(super) fn next(self) -> Align {
        match self {
            Align::Left => Align::Centre,
            Align::Centre => Align::Right,
            Align::Right => Align::Left,
        }
    }
}

/// Where the lines sit in a text box taller than they are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub(super) enum VAlign {
    #[default]
    Top,
    Middle,
    Bottom,
}

impl VAlign {
    pub(super) fn next(self) -> VAlign {
        match self {
            VAlign::Top => VAlign::Middle,
            VAlign::Middle => VAlign::Bottom,
            VAlign::Bottom => VAlign::Top,
        }
    }
}

/// How far down a box `height` tall (none: as tall as the text) its `lines` tall text starts.
pub(super) fn valign_offset(valign: VAlign, lines: f32, height: Option<f32>) -> f32 {
    let room = (height.unwrap_or(lines) - lines).max(0.0);
    match valign {
        VAlign::Top => 0.0,
        VAlign::Middle => (room / 2.0).round(),
        VAlign::Bottom => room,
    }
}

/// What keeps a text readable over a busy screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub(super) enum Back {
    /// A dark outline around the letters.
    #[default]
    Outline,
    /// A dark translucent pill behind the text.
    Shade,
    /// A pill in the text's colour, the letters in black or white on it.
    Tag,
    None,
}

impl Back {
    pub(super) fn next(self) -> Back {
        match self {
            Back::Outline => Back::Shade,
            Back::Shade => Back::Tag,
            Back::Tag => Back::None,
            Back::None => Back::Outline,
        }
    }
}

/// How a whole text mark looks, besides its colour and size.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Style {
    pub(super) font: Arc<str>,
    pub(super) bold: bool,
    pub(super) italic: bool,
    pub(super) underline: bool,
    pub(super) strike: bool,
    pub(super) align: Align,
    pub(super) valign: VAlign,
    pub(super) back: Back,
}

impl Default for Style {
    fn default() -> Style {
        Style {
            font: Arc::from("Segoe UI"),
            bold: true,
            italic: false,
            underline: false,
            strike: false,
            align: Align::Left,
            valign: VAlign::Top,
            back: Back::Outline,
        }
    }
}

/// The style and size the last text had: new texts start with them, in this shot and the next.
static LAST: Mutex<Option<(Style, f32)>> = Mutex::new(None);

pub(super) fn last_style() -> Option<(Style, f32)> {
    LAST.lock().ok()?.clone()
}

pub(super) fn remember_style(style: &Style, size: f32) {
    if let Ok(mut last) = LAST.lock() {
        *last = Some((style.clone(), size));
    }
}

/// Space between a pill's edge and its text, for a font of `px` physical pixels.
pub(super) fn padding(back: Back, px: f32) -> (f32, f32) {
    match back {
        Back::Shade | Back::Tag => ((px * 0.34).round().max(3.0), (px * 0.14).round().max(2.0)),
        Back::Outline | Back::None => (0.0, 0.0),
    }
}

/// How far the dark outline around text reaches, for a font of `px` physical pixels.
pub(super) fn outline(px: f32) -> f32 {
    (px / 14.0).round().max(1.0)
}

// --- editing: byte offsets into the text, always on char boundaries ---

pub(super) fn line_start(t: &str, at: usize) -> usize {
    t[..at].rfind('\n').map_or(0, |i| i + 1)
}

pub(super) fn line_end(t: &str, at: usize) -> usize {
    t[at..].find('\n').map_or(t.len(), |i| at + i)
}

pub(super) fn prev(t: &str, at: usize) -> usize {
    t[..at].char_indices().next_back().map_or(0, |(i, _)| i)
}

pub(super) fn next(t: &str, at: usize) -> usize {
    t[at..].chars().next().map_or(at, |c| at + c.len_utf8())
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Where Ctrl+Left lands: the start of the word before `at` (skipping the spaces in between).
pub(super) fn word_left(t: &str, at: usize) -> usize {
    let mut chars = t[..at].char_indices().rev().peekable();
    while chars.next_if(|(_, c)| c.is_whitespace()).is_some() {}
    match chars.peek() {
        Some(&(_, c)) if is_word(c) => {
            let mut start = at;
            while let Some((i, _)) = chars.next_if(|(_, c)| is_word(*c)) {
                start = i;
            }
            start
        }
        Some(&(i, _)) => i,
        None => 0,
    }
}

/// Where Ctrl+Right lands: past the word at `at` and the spaces after it.
pub(super) fn word_right(t: &str, at: usize) -> usize {
    let mut chars = t[at..].char_indices().map(|(i, c)| (at + i, c)).peekable();
    match chars.peek() {
        Some(&(_, c)) if is_word(c) => while chars.next_if(|(_, c)| is_word(*c)).is_some() {},
        Some(_) => {
            chars.next();
        }
        None => return t.len(),
    }
    while chars.next_if(|(_, c)| c.is_whitespace() && *c != '\n').is_some() {}
    chars.peek().map_or(t.len(), |&(i, _)| i)
}

/// The word (or run of other characters) around `at`, for a double-click.
pub(super) fn word_at(t: &str, at: usize) -> (usize, usize) {
    let kind = |c: char| if is_word(c) { 0 } else if c.is_whitespace() { 1 } else { 2 };
    let Some(c) = t[at..].chars().next().or_else(|| t[..at].chars().next_back()) else { return (at, at) };
    let k = kind(c);
    let start = t[..at].char_indices().rev().take_while(|(_, c)| kind(*c) == k).last().map_or(at, |(i, _)| i);
    let end = t[at..].char_indices().find(|(_, c)| kind(*c) != k).map_or(t.len(), |(i, _)| at + i);
    (start, end)
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

#[derive(Clone, PartialEq, Eq, Hash)]
struct LayoutKey {
    text: String,
    style: Style,
    px: u32,
    width: Option<u32>,
}

struct Laid {
    key: LayoutKey,
    layout: IDWriteTextLayout,
    /// The text's box: width (the wrap width when it has one) and height.
    size: (f32, f32),
}

/// A text format's family, bold, italic and size (its bits).
type FormatKey = (Arc<str>, bool, bool, u32);

/// Lays out and draws mark text with DirectWrite. Layouts are kept for the last few texts, so
/// measuring, the caret, hit tests and drawing the same text share one.
pub(super) struct Typesetter {
    dwrite: IDWriteFactory,
    formats: RefCell<Vec<(FormatKey, IDWriteTextFormat)>>,
    layouts: RefCell<Vec<Laid>>,
    /// Font list rows: each family's name laid out in its own face, or the UI's.
    previews: RefCell<HashMap<PreviewKey, IDWriteTextLayout>>,
    /// When this paint's font list started, and whether it left rows for the next frame.
    preview_frame: Cell<Option<std::time::Instant>>,
    deferred: Cell<bool>,
}

/// A font list row: the family, whether it's in its own face, and the size and box (their bits).
type PreviewKey = (Arc<str>, bool, [u32; 3]);

impl Typesetter {
    pub(super) fn new() -> Typesetter {
        // the shared factory is a process singleton the picker's painter has already created
        let dwrite = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.expect("DirectWrite shared factory");
        let (formats, layouts, previews, preview_frame, deferred) = Default::default();
        Typesetter { dwrite, formats, layouts, previews, preview_frame, deferred }
    }

    fn format(&self, font: &Arc<str>, bold: bool, italic: bool, px: f32) -> Option<IDWriteTextFormat> {
        let key = (font.clone(), bold, italic, px.to_bits());
        let mut formats = self.formats.borrow_mut();
        if let Some(i) = formats.iter().position(|(k, _)| *k == key) {
            let entry = formats.remove(i);
            let f = entry.1.clone();
            formats.push(entry);
            return Some(f);
        }
        let f = unsafe {
            let f = self
                .dwrite
                .CreateTextFormat(
                    &HSTRING::from(&*key.0),
                    None,
                    if bold { DWRITE_FONT_WEIGHT_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL },
                    if italic { DWRITE_FONT_STYLE_ITALIC } else { DWRITE_FONT_STYLE_NORMAL },
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

    /// The layout of `text` in `style` at `px` physical pixels, `width` wide when it wraps, and the
    /// size of its box.
    fn laid<R>(&self, text: &str, style: &Style, px: f32, width: Option<f32>, f: impl FnOnce(&IDWriteTextLayout, (f32, f32)) -> R) -> Option<R> {
        let key = LayoutKey { text: text.to_owned(), style: style.clone(), px: px.to_bits(), width: width.map(f32::to_bits) };
        {
            let mut layouts = self.layouts.borrow_mut();
            if let Some(i) = layouts.iter().position(|l| l.key == key) {
                let l = layouts.remove(i);
                let out = f(&l.layout, l.size);
                layouts.push(l);
                return Some(out);
            }
        }
        let format = self.format(&style.font, style.bold, style.italic, px)?;
        let s: Vec<u16> = text.encode_utf16().collect();
        let (layout, size) = unsafe {
            let layout = self.dwrite.CreateTextLayout(&s, &format, width.unwrap_or(UNBOUNDED), UNBOUNDED).ok()?;
            let all = DWRITE_TEXT_RANGE { startPosition: 0, length: s.len() as u32 };
            if style.underline {
                let _ = layout.SetUnderline(true, all);
            }
            if style.strike {
                let _ = layout.SetStrikethrough(true, all);
            }
            if width.is_some() {
                let _ = layout.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
            }
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m).ok()?;
            let w = width.unwrap_or(m.widthIncludingTrailingWhitespace);
            // lines line up with each other inside the box
            let _ = layout.SetMaxWidth(w.max(1.0));
            let _ = layout.SetTextAlignment(match style.align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Centre => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            });
            (layout, (w, m.height))
        };
        let out = f(&layout, size);
        let mut layouts = self.layouts.borrow_mut();
        if layouts.len() == LAYOUTS {
            layouts.remove(0);
        }
        layouts.push(Laid { key, layout, size });
        Some(out)
    }

    /// Width and height of the text's box (without padding); an empty text is one line high.
    pub(super) fn extent(&self, text: &str, style: &Style, px: f32, width: Option<f32>) -> (f32, f32) {
        self.laid(text, style, px, width, |_, size| size).unwrap_or((width.unwrap_or(0.0), px))
    }

    /// Left and top of the caret before byte `caret`, and its height, relative to the text's corner.
    pub(super) fn caret(&self, text: &str, style: &Style, px: f32, width: Option<f32>, caret: usize) -> (f32, f32, f32) {
        self.laid(text, style, px, width, |layout, _| {
            let (mut x, mut y, mut m) = (0.0, 0.0, DWRITE_HIT_TEST_METRICS::default());
            match unsafe { layout.HitTestTextPosition(utf16_len(&text[..caret]), false, &mut x, &mut y, &mut m) } {
                Ok(()) => (x, m.top, m.height),
                Err(_) => (0.0, 0.0, px),
            }
        })
        .unwrap_or((0.0, 0.0, px))
    }

    /// The byte offset a click at `(x, y)` (relative to the text's corner) puts the caret at.
    pub(super) fn hit(&self, text: &str, style: &Style, px: f32, width: Option<f32>, (x, y): (f32, f32)) -> usize {
        self.laid(text, style, px, width, |layout, _| {
            let (mut trailing, mut inside, mut m) = (BOOL(0), BOOL(0), DWRITE_HIT_TEST_METRICS::default());
            match unsafe { layout.HitTestPoint(x, y, &mut trailing, &mut inside, &mut m) } {
                Ok(()) => byte_at(text, m.textPosition + if trailing.as_bool() { m.length } else { 0 }),
                Err(_) => text.len(),
            }
        })
        .unwrap_or(text.len())
    }

    /// The boxes (relative to the text's corner) the bytes `from..to` cover, one per line.
    pub(super) fn range(&self, text: &str, style: &Style, px: f32, width: Option<f32>, (from, to): (usize, usize)) -> Vec<[f32; 4]> {
        if from >= to {
            return Vec::new();
        }
        self.laid(text, style, px, width, |layout, _| unsafe {
            let (start, len) = (utf16_len(&text[..from]), utf16_len(&text[from..to]));
            let mut n = 0;
            let _ = layout.HitTestTextRange(start, len, 0.0, 0.0, None, &mut n);
            let mut boxes = vec![DWRITE_HIT_TEST_METRICS::default(); n as usize];
            if layout.HitTestTextRange(start, len, 0.0, 0.0, Some(&mut boxes), &mut n).is_err() {
                return Vec::new();
            }
            // an empty line still shows that it's selected
            boxes.iter().map(|m| [m.left, m.top, m.width.max(px / 4.0), m.height]).collect()
        })
        .unwrap_or_default()
    }

    /// Draws a text mark's box at `at`: its backing (filling the box `height` tall, when it's
    /// taller than the text), then the letters, placed in it by the style's vertical alignment.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw(
        &self,
        c: &Canvas,
        text: &str,
        style: &Style,
        px: f32,
        (width, height): (Option<f32>, Option<f32>),
        at: (f32, f32),
        color: D2D1_COLOR_F,
        ink_on_color: D2D1_COLOR_F,
    ) {
        let (pad_x, pad_y) = padding(style.back, px);
        self.laid(text, style, px, width, |layout, (w, h)| {
            let origin = (at.0 + pad_x, at.1 + pad_y + valign_offset(style.valign, h, height));
            let (bw, bh) = (w + 2.0 * pad_x, h.max(height.unwrap_or(0.0)) + 2.0 * pad_y);
            let filled = !text.is_empty() || height.is_some();
            let pill = |fill| c.round_rect(at.0, at.1, bw, bh, (bh * 0.3).min(px * 0.6), fill);
            match style.back {
                Back::Outline => {
                    let reach = outline(px);
                    let d = reach * std::f32::consts::FRAC_1_SQRT_2;
                    for (dx, dy) in [(reach, 0.0), (-reach, 0.0), (0.0, reach), (0.0, -reach), (d, d), (d, -d), (-d, d), (-d, -d)] {
                        c.text_layout(layout, (origin.0 + dx, origin.1 + dy), SHADE);
                    }
                    c.text_layout(layout, origin, color);
                }
                Back::Shade => {
                    if filled {
                        pill(PILL);
                    }
                    c.text_layout(layout, origin, color);
                }
                Back::Tag => {
                    if filled {
                        pill(color);
                    }
                    c.text_layout(layout, origin, if filled { ink_on_color } else { color });
                }
                Back::None => c.text_layout(layout, origin, color),
            }
        });
    }

    /// One line of `text` in `font`, left-aligned and vertically centred in `[x, y, w, h]`, cut off
    /// at the box's right edge.
    pub(super) fn draw_line(&self, c: &Canvas, text: &str, style: &Style, px: f32, [x, y, w, h]: [f32; 4], color: D2D1_COLOR_F) {
        let plain = Style { align: Align::Left, back: Back::None, ..style.clone() };
        let Some(format) = self.format(&plain.font, plain.bold, plain.italic, px) else { return };
        let s: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let Ok(layout) = self.dwrite.CreateTextLayout(&s, &format, w.max(1.0), h.max(1.0)) else { return };
            let _ = layout.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            let all = DWRITE_TEXT_RANGE { startPosition: 0, length: s.len() as u32 };
            if style.underline {
                let _ = layout.SetUnderline(true, all);
            }
            if style.strike {
                let _ = layout.SetStrikethrough(true, all);
            }
            let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
            let _ = layout.SetTrimming(&trim, None);
            c.text_layout(&layout, (x, y), color);
        }
    }

    pub(super) fn draw_centred(&self, c: &Canvas, text: &str, px: f32, centre: (f32, f32), color: D2D1_COLOR_F) {
        if let Some(format) = self.format(&Arc::from("Segoe UI"), true, false, px) {
            c.text_centred(text, &format, centre, color);
        }
    }

    /// A short `text` in `style`'s font, weight and slant, its ink centred on `centre`.
    pub(super) fn draw_glyph(&self, c: &Canvas, text: &str, style: &Style, px: f32, centre: (f32, f32), color: D2D1_COLOR_F) {
        if let Some(format) = self.format(&style.font, style.bold, style.italic, px) {
            c.text_centred(text, &format, centre, color);
        }
    }

    /// Width of one line of `text` in the UI's face at `px`.
    pub(super) fn measure(&self, text: &str, px: f32) -> f32 {
        let style = Style { bold: false, ..Style::default() };
        self.extent(text, &style, px, None).0
    }

    /// A paint of the font list starts: its rows get [`PREVIEW_BUDGET`] to lay out new faces.
    pub(super) fn begin_previews(&self) {
        self.preview_frame.set(Some(std::time::Instant::now()));
    }

    /// The last paint left font list rows in the UI's face for lack of time.
    pub(super) fn previews_deferred(&self) -> bool {
        self.deferred.get()
    }

    /// [`Typesetter::previews_deferred`], and forget it: the next paint tries those rows again.
    pub(super) fn take_deferred(&self) -> bool {
        self.deferred.replace(false)
    }

    /// A font list row: `name` left-aligned and vertically centred in `[x, y, w, h]`, cut off at the
    /// box's right edge, in its own face when `own` (the family is loaded) and the UI's otherwise.
    /// Rows are laid out once and kept; a paint that has used its time leaves new faces for later.
    pub(super) fn draw_preview(&self, c: &Canvas, name: &Arc<str>, own: bool, px: f32, [x, y, w, h]: [f32; 4], color: D2D1_COLOR_F) {
        let key = (name.clone(), own, [px.to_bits(), w.to_bits(), h.to_bits()]);
        let cached = self.previews.borrow().get(&key).cloned();
        let layout = match cached {
            Some(l) => l,
            None if own && self.preview_frame.get().is_some_and(|at| at.elapsed() >= PREVIEW_BUDGET) => {
                self.deferred.set(true);
                return self.draw_preview(c, name, false, px, [x, y, w, h], color);
            }
            None => {
                let face = if own { name.clone() } else { Arc::from("Segoe UI") };
                let Some(format) = self.format(&face, false, false, px) else { return };
                let s: Vec<u16> = name.encode_utf16().collect();
                let Ok(layout) = (unsafe { self.dwrite.CreateTextLayout(&s, &format, w.max(1.0), h.max(1.0)) }) else { return };
                unsafe {
                    let _ = layout.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                    let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
                    let _ = layout.SetTrimming(&trim, None);
                }
                let mut previews = self.previews.borrow_mut();
                if previews.len() >= PREVIEWS {
                    previews.clear();
                }
                previews.insert(key, layout.clone());
                layout
            }
        };
        c.text_layout(&layout, (x, y), color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_found_both_ways() {
        let t = "hello,  big world\nnext";
        assert_eq!(word_left(t, 13), 12, "back to the start of the word");
        assert_eq!(word_left(t, 12), 8, "over the space into the word before");
        assert_eq!(word_left(t, 8), 5, "over the spaces onto the comma");
        assert_eq!(word_left(t, 5), 0);
        assert_eq!(word_left(t, 0), 0);
        assert_eq!(word_right(t, 0), 5, "past hello");
        assert_eq!(word_right(t, 5), 8, "the comma and the spaces after it");
        assert_eq!(word_right(t, 8), 12);
        assert_eq!(word_right(t, 12), 17, "stops at the line's end");
        assert_eq!(word_right(t, t.len()), t.len());
        assert_eq!(word_at(t, 9), (8, 11));
        assert_eq!(word_at(t, 6), (6, 8), "a run of spaces");
        assert_eq!(word_at("ab", 2), (0, 2), "at the end, the word before");
        assert_eq!(word_left("é😀 x", 7), 2, "an emoji isn't part of a word");
    }

    #[test]
    fn lines_and_chars_step_on_boundaries() {
        let t = "first\nab\nthird";
        assert_eq!((line_start(t, 7), line_end(t, 7)), (6, 8));
        assert_eq!(prev("a😀b", 5), 1);
        assert_eq!(next("a😀b", 1), 5);
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

    #[test]
    fn lines_sit_in_a_taller_box_as_aligned() {
        assert_eq!(valign_offset(VAlign::Top, 30.0, Some(100.0)), 0.0);
        assert_eq!(valign_offset(VAlign::Middle, 30.0, Some(100.0)), 35.0);
        assert_eq!(valign_offset(VAlign::Bottom, 30.0, Some(100.0)), 70.0);
        assert_eq!(valign_offset(VAlign::Bottom, 30.0, None), 0.0, "a box as tall as its text");
        assert_eq!(valign_offset(VAlign::Middle, 120.0, Some(100.0)), 0.0, "text taller than the box grows it");
        assert_eq!(VAlign::Top.next().next().next(), VAlign::Top);
    }

    #[test]
    fn styles_change_the_layout() {
        let ts = Typesetter::new();
        let plain = Style { bold: false, ..Style::default() };
        let bold = Style::default();
        let (w, h) = ts.extent("glint", &plain, 40.0, None);
        assert!(w > 40.0 && h >= 40.0, "{w} {h}");
        assert!(ts.extent("glint", &bold, 40.0, None).0 > w, "bold is wider");
        let wrapped = ts.extent("one two three four", &plain, 40.0, Some(120.0));
        assert_eq!(wrapped.0, 120.0, "a wrapping text's box is its width");
        assert!(wrapped.1 > 2.0 * h, "and it wraps onto more lines");
        let centred = Style { align: Align::Centre, ..plain.clone() };
        let (x, ..) = ts.caret("wide line\nab", &centred, 40.0, None, 10);
        assert!(x > 20.0, "a short line is centred under the long one: {x}");
        assert_eq!(ts.caret("wide line\nab", &plain, 40.0, None, 10).0, 0.0);
        let hit = ts.hit("abc", &plain, 40.0, None, (1000.0, 10.0));
        assert_eq!(hit, 3);
        let boxes = ts.range("ab\ncd", &plain, 40.0, None, (1, 4));
        assert_eq!(boxes.len(), 2, "one box per line");
    }
}
