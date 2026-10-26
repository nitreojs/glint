use std::sync::{Arc, Mutex};

use glint_core::geom::rect::Rect;
use glint_shell::paste::Image;
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;

use super::text::{Back, Style, Typesetter, outline};
use crate::platform::cursor::CursorImage;
use glint_shell::gfx::{Canvas, Dib, rgba};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tool {
    Pen,
    Highlighter,
    Arrow,
    Rectangle,
    Blur,
    Pixelate,
    Eraser,
    Text,
    Step,
    Cursor,
    Grab,
    /// Pictures from the clipboard: pasted, or picked from the recent ones.
    Image,
}

/// Tools in toolbar order with their tooltip, letter key and digit key.
pub(super) const TOOLS: [(Tool, &str, u8, Option<u32>); 12] = [
    (Tool::Pen, "pen  ·  1 / P", b'P', Some(1)),
    (Tool::Highlighter, "highlighter  ·  2 / H", b'H', Some(2)),
    (Tool::Arrow, "arrow  ·  3 / A", b'A', Some(3)),
    (Tool::Rectangle, "rectangle  ·  4 / R", b'R', Some(4)),
    (Tool::Blur, "blur  ·  5 / B", b'B', Some(5)),
    (Tool::Pixelate, "pixelate  ·  6 / X", b'X', Some(6)),
    (Tool::Eraser, "smart eraser  ·  9 / E", b'E', Some(9)),
    (Tool::Text, "text  ·  7 / T", b'T', Some(7)),
    (Tool::Step, "step  ·  8 / N", b'N', Some(8)),
    (Tool::Cursor, "cursor  ·  0 / M", b'M', Some(0)),
    (Tool::Grab, "grab a piece of the screen  ·  G", b'G', None),
    (Tool::Image, "clipboard  ·  V  ·  Ctrl+V pastes", b'V', None),
];

#[derive(Clone, Copy)]
pub(super) struct Strength {
    pub(super) presets: [f32; 3],
    pub(super) step: f32,
    pub(super) min: f32,
    pub(super) max: f32,
    pub(super) name: &'static str,
}

impl Tool {
    pub(super) fn strength(self) -> Option<Strength> {
        let (presets, step, min, max, name) = match self {
            Tool::Pen | Tool::Arrow | Tool::Rectangle => ([2.0, 4.0, 8.0], 1.0, 1.0, 40.0, "width"),
            Tool::Highlighter => ([10.0, 16.0, 28.0], 2.0, 4.0, 80.0, "width"),
            Tool::Blur => ([4.0, 8.0, 16.0], 1.0, 1.0, 40.0, "blur"),
            Tool::Pixelate => ([6.0, 12.0, 20.0], 2.0, 2.0, 80.0, "block size"),
            Tool::Text => ([16.0, 24.0, 36.0], 2.0, 8.0, 320.0, "size"),
            Tool::Step => ([20.0, 28.0, 40.0], 2.0, 12.0, 120.0, "size"),
            Tool::Cursor => ([75.0, 100.0, 150.0], 25.0, 50.0, 400.0, "size %"),
            Tool::Eraser | Tool::Grab | Tool::Image => return None,
        };
        Some(Strength { presets, step, min, max, name })
    }

    pub(super) fn picks_rects(self) -> bool {
        matches!(self, Tool::Rectangle | Tool::Blur | Tool::Pixelate | Tool::Eraser | Tool::Grab)
    }
}

pub(super) const COLORS: [D2D1_COLOR_F; 5] = [
    rgba(0xE5, 0x40, 0x2F, 1.0),
    rgba(0xFF, 0xD6, 0x0A, 1.0),
    rgba(0x34, 0xC7, 0x59, 1.0),
    rgba(0x0A, 0x84, 0xFF, 1.0),
    rgba(0xFF, 0xFF, 0xFF, 1.0),
];
/// Palette entries a white number can't sit on.
pub(super) const LIGHT: [bool; 5] = [false, true, false, false, true];
pub(super) const STEP_INK: D2D1_COLOR_F = rgba(20, 20, 24, 1.0);
const HIGHLIGHTER_ALPHA: f32 = 0.4;
pub(super) const MIN_SIDE: i32 = 3;

/// A cursor picture shared by its marks and their undo copies; equal only to itself.
#[derive(Clone, Debug, Default)]
pub(super) struct Sprite(pub(super) Arc<CursorImage>);

impl PartialEq for Sprite {
    fn eq(&self, other: &Sprite) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// A pasted picture shared by its mark and the mark's undo copies; equal only to itself. A
/// picture shown much smaller than it is keeps a copy at the size it's shown (the second
/// field), so moving it doesn't scale the whole picture down again every frame.
#[derive(Clone, Debug)]
pub(super) struct Picture(pub(super) Arc<Image>, Arc<Mutex<Option<Image>>>);

impl Picture {
    pub(super) fn new(image: Arc<Image>) -> Picture {
        Picture(image, Arc::default())
    }
}

impl PartialEq for Picture {
    fn eq(&self, other: &Picture) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Where a cursor with its hotspot at `at`, drawn at `size` percent, lands: `[x, y, w, h]`.
fn cursor_box(at: (f32, f32), sprite: &Sprite, size: f32) -> [f32; 4] {
    let (img, k) = (&sprite.0, size / 100.0);
    [at.0 - img.hot.0 as f32 * k, at.1 - img.hot.1 as f32 * k, img.w as f32 * k, img.h as f32 * k]
}

/// `r` resized from a corner (`e`) with its height following its width at `aspect` (w / h).
fn keep_aspect(r: Rect, e: Edges, aspect: f32) -> Rect {
    let h = ((r.w as f32 / aspect).round() as i32).max(MIN_SIDE);
    Rect { y: if e.t { r.bottom() - h } else { r.y }, h, ..r }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Mark {
    Stroke { highlight: bool, points: Vec<(f32, f32)>, color: usize, width: f32 },
    Arrow { from: (f32, f32), to: (f32, f32), color: usize, width: f32 },
    Rectangle { rect: Rect, color: usize, width: f32 },
    Blur { rect: Rect, radius: f32 },
    Pixelate { rect: Rect, block: f32 },
    Eraser { rect: Rect, fill: u32 },
    /// `at` is the top-left corner of its box; `extent` is the box's laid-out size in physical
    /// pixels, padding included. `width` is the wrap width (physical pixels, text only), none
    /// for lines broken only at newlines.
    Text { at: (f32, f32), text: String, color: usize, size: f32, style: Style, width: Option<f32>, extent: (f32, f32) },
    /// A numbered disc of diameter `size` (radius `r` in physical pixels), with an arrow to `tail`.
    Step { at: (f32, f32), tail: Option<(f32, f32)>, color: usize, size: f32, r: f32 },
    /// A cursor picture with its hotspot at `at`, drawn at `size` percent.
    Cursor { at: (f32, f32), sprite: Sprite, size: f32 },
    /// The frozen screen's `src` (client coordinates) shown at `rect`.
    Grab { src: Rect, rect: Rect },
    /// A pasted picture shown at `rect`.
    Image { rect: Rect, picture: Picture },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Grip {
    From,
    To,
    Edge(Edges),
}

impl Mark {
    pub(super) fn new(tool: Tool, (x, y): (i32, i32), color: usize, strength: f32) -> Mark {
        let (at, rect) = ((x as f32, y as f32), Rect { x, y, w: 0, h: 0 });
        match tool {
            Tool::Pen | Tool::Highlighter => {
                Mark::Stroke { highlight: tool == Tool::Highlighter, points: vec![at], color, width: strength }
            }
            Tool::Arrow => Mark::Arrow { from: at, to: at, color, width: strength },
            Tool::Rectangle => Mark::Rectangle { rect, color, width: strength },
            Tool::Blur => Mark::Blur { rect, radius: strength },
            Tool::Pixelate => Mark::Pixelate { rect, block: strength },
            Tool::Eraser => Mark::Eraser { rect, fill: 0 },
            Tool::Text => Mark::Text {
                at,
                text: String::new(),
                color,
                size: strength,
                style: Style::default(),
                width: None,
                extent: (0.0, 0.0),
            },
            Tool::Step => Mark::Step { at, tail: None, color, size: strength, r: 0.0 },
            // the caller hands it its picture
            Tool::Cursor => Mark::Cursor { at, sprite: Sprite::default(), size: strength },
            Tool::Grab => Mark::Grab { src: rect, rect },
            Tool::Image => unreachable!("pictures are pasted, not drawn"),
        }
    }

    pub(super) fn tool(&self) -> Tool {
        match self {
            Mark::Stroke { highlight: true, .. } => Tool::Highlighter,
            Mark::Stroke { .. } => Tool::Pen,
            Mark::Arrow { .. } => Tool::Arrow,
            Mark::Rectangle { .. } => Tool::Rectangle,
            Mark::Blur { .. } => Tool::Blur,
            Mark::Pixelate { .. } => Tool::Pixelate,
            Mark::Eraser { .. } => Tool::Eraser,
            Mark::Text { .. } => Tool::Text,
            Mark::Step { .. } => Tool::Step,
            Mark::Cursor { .. } => Tool::Cursor,
            Mark::Grab { .. } => Tool::Grab,
            Mark::Image { .. } => Tool::Image,
        }
    }

    pub(super) fn is_effect(&self) -> bool {
        matches!(self, Mark::Blur { .. } | Mark::Pixelate { .. })
    }

    pub(super) fn strength(&self) -> Option<f32> {
        match self {
            Mark::Stroke { width, .. } | Mark::Arrow { width, .. } | Mark::Rectangle { width, .. } => Some(*width),
            Mark::Blur { radius, .. } => Some(*radius),
            Mark::Pixelate { block, .. } => Some(*block),
            Mark::Text { size, .. } | Mark::Step { size, .. } | Mark::Cursor { size, .. } => Some(*size),
            Mark::Eraser { .. } | Mark::Grab { .. } | Mark::Image { .. } => None,
        }
    }

    pub(super) fn strength_mut(&mut self) -> Option<&mut f32> {
        match self {
            Mark::Stroke { width, .. } | Mark::Arrow { width, .. } | Mark::Rectangle { width, .. } => Some(width),
            Mark::Blur { radius, .. } => Some(radius),
            Mark::Pixelate { block, .. } => Some(block),
            Mark::Text { size, .. } | Mark::Step { size, .. } | Mark::Cursor { size, .. } => Some(size),
            Mark::Eraser { .. } | Mark::Grab { .. } | Mark::Image { .. } => None,
        }
    }

    pub(super) fn color(&self) -> Option<usize> {
        match self {
            Mark::Stroke { color, .. }
            | Mark::Arrow { color, .. }
            | Mark::Rectangle { color, .. }
            | Mark::Text { color, .. }
            | Mark::Step { color, .. } => Some(*color),
            Mark::Blur { .. }
            | Mark::Pixelate { .. }
            | Mark::Eraser { .. }
            | Mark::Cursor { .. }
            | Mark::Grab { .. }
            | Mark::Image { .. } => None,
        }
    }

    pub(super) fn color_mut(&mut self) -> Option<&mut usize> {
        match self {
            Mark::Stroke { color, .. }
            | Mark::Arrow { color, .. }
            | Mark::Rectangle { color, .. }
            | Mark::Text { color, .. }
            | Mark::Step { color, .. } => Some(color),
            Mark::Blur { .. }
            | Mark::Pixelate { .. }
            | Mark::Eraser { .. }
            | Mark::Cursor { .. }
            | Mark::Grab { .. }
            | Mark::Image { .. } => None,
        }
    }

    pub(super) fn area_mut(&mut self) -> Option<&mut Rect> {
        match self {
            Mark::Rectangle { rect, .. }
            | Mark::Blur { rect, .. }
            | Mark::Pixelate { rect, .. }
            | Mark::Eraser { rect, .. }
            | Mark::Grab { rect, .. }
            | Mark::Image { rect, .. } => Some(rect),
            Mark::Stroke { .. } | Mark::Arrow { .. } | Mark::Text { .. } | Mark::Step { .. } | Mark::Cursor { .. } => None,
        }
    }

    /// Whether the mark is big enough to keep (a click without a drag isn't an arrow).
    pub(super) fn worth_keeping(&self) -> bool {
        match self {
            Mark::Stroke { .. } | Mark::Step { .. } | Mark::Cursor { .. } | Mark::Image { .. } => true,
            Mark::Text { text, .. } => !text.trim().is_empty(),
            Mark::Arrow { from, to, .. } => (from.0 - to.0).abs() >= 3.0 || (from.1 - to.1).abs() >= 3.0,
            Mark::Rectangle { rect, .. }
            | Mark::Blur { rect, .. }
            | Mark::Pixelate { rect, .. }
            | Mark::Eraser { rect, .. } => rect.w >= MIN_SIDE && rect.h >= MIN_SIDE,
            Mark::Grab { src, .. } => src.w >= MIN_SIDE && src.h >= MIN_SIDE,
        }
    }

    pub(super) fn bbox(&self) -> Rect {
        match self {
            Mark::Stroke { points, .. } => span(points),
            Mark::Arrow { from, to, .. } => span(&[*from, *to]),
            Mark::Rectangle { rect, .. }
            | Mark::Blur { rect, .. }
            | Mark::Pixelate { rect, .. }
            | Mark::Eraser { rect, .. }
            | Mark::Grab { rect, .. }
            | Mark::Image { rect, .. } => *rect,
            Mark::Text { at, extent, .. } => span(&[*at, (at.0 + extent.0.max(1.0), at.1 + extent.1)]),
            Mark::Step { at, tail, r, .. } => {
                let corners = [(at.0 - r, at.1 - r), (at.0 + r, at.1 + r)];
                span(&[&corners[..], tail.as_slice()].concat())
            }
            Mark::Cursor { at, sprite, size } => {
                let [x, y, w, h] = cursor_box(*at, sprite, *size);
                Rect::from_points(x.floor() as i32, y.floor() as i32, (x + w).ceil() as i32, (y + h).ceil() as i32)
            }
        }
    }

    /// Every pixel drawing the mark can touch (client coordinates).
    pub(super) fn ink(&self, scale: f32) -> Rect {
        let reach = match self {
            Mark::Stroke { width, .. } | Mark::Rectangle { width, .. } => ink_width(*width, scale) / 2.0,
            Mark::Arrow { width, .. } => {
                let w = ink_width(*width, scale);
                (w / 2.0).max(arrow_head(w, f32::MAX) * 0.55)
            }
            Mark::Text { size, style, .. } => {
                // italics and some letters reach past their box
                let px = size * scale;
                let lean = if style.italic { 0.25 } else { 0.12 };
                let rim = if style.back == Back::Outline { outline(px) } else { 0.0 };
                rim + px * lean
            }
            Mark::Step { tail: Some(_), r, .. } => arrow_head(tail_width(*r), f32::MAX) * 0.55,
            Mark::Blur { .. }
            | Mark::Pixelate { .. }
            | Mark::Eraser { .. }
            | Mark::Step { .. }
            | Mark::Cursor { .. }
            | Mark::Grab { .. }
            | Mark::Image { .. } => 0.0,
        };
        self.bbox().inflate(reach.ceil() as i32 + 1)
    }

    pub(super) fn shifted(&self, dx: i32, dy: i32) -> Mark {
        let shift = |p: &mut (f32, f32)| *p = (p.0 + dx as f32, p.1 + dy as f32);
        let mut m = self.clone();
        match &mut m {
            Mark::Stroke { points, .. } => points.iter_mut().for_each(shift),
            Mark::Arrow { from, to, .. } => {
                shift(from);
                shift(to);
            }
            Mark::Rectangle { rect, .. }
            | Mark::Blur { rect, .. }
            | Mark::Pixelate { rect, .. }
            | Mark::Eraser { rect, .. }
            | Mark::Grab { rect, .. }
            | Mark::Image { rect, .. } => *rect = rect.offset(dx, dy),
            Mark::Text { at, .. } | Mark::Cursor { at, .. } => shift(at),
            Mark::Step { at, tail, .. } => {
                shift(at);
                tail.iter_mut().for_each(shift);
            }
        }
        m
    }

    /// The mark with `grip` dragged by `(dx, dy)`, inside `b`. `keep` (Shift) keeps a picture's
    /// proportions from its corners. Texts are resized by the editor, which lays them out.
    pub(super) fn resized(&self, grip: Grip, (dx, dy): (i32, i32), b: Rect, keep: bool) -> Mark {
        let shift = |p: &mut (f32, f32)| {
            *p = (
                (p.0 + dx as f32).max(b.x as f32).min(b.right() as f32),
                (p.1 + dy as f32).max(b.y as f32).min(b.bottom() as f32),
            )
        };
        let mut m = self.clone();
        match (&mut m, grip) {
            (Mark::Arrow { from, .. }, Grip::From) => shift(from),
            (Mark::Arrow { to, .. }, Grip::To) => shift(to),
            (Mark::Step { tail: Some(t), .. }, Grip::To) => shift(t),
            // a grab keeps its proportions from the corners
            (Mark::Grab { src, rect }, Grip::Edge(e)) if (e.l || e.r) && (e.t || e.b) => {
                let aspect = src.w.max(1) as f32 / src.h.max(1) as f32;
                *rect = keep_aspect(resize(*rect, e, (dx, dy), b, MIN_SIDE), e, aspect);
            }
            (Mark::Image { picture, rect }, Grip::Edge(e)) if keep && (e.l || e.r) && (e.t || e.b) => {
                let aspect = picture.0.w.max(1) as f32 / picture.0.h.max(1) as f32;
                *rect = keep_aspect(resize(*rect, e, (dx, dy), b, MIN_SIDE), e, aspect);
            }
            (Mark::Text { .. }, _) => {}
            (m, Grip::Edge(edges)) => {
                if let Some(r) = m.area_mut() {
                    *r = resize(*r, edges, (dx, dy), b, MIN_SIDE);
                }
            }
            _ => {}
        }
        m
    }

    pub(super) fn hit(&self, p: (f32, f32), scale: f32) -> bool {
        let slop = 4.0 * scale;
        match self {
            Mark::Stroke { points, width, .. } => path_dist(points, p) <= width * scale / 2.0 + slop,
            Mark::Arrow { from, to, width, .. } => {
                let (w, len) = (width * scale, dist(*from, *to));
                let head = arrow_head(w, len);
                let t = if len > 0.0 { head / len } else { 0.0 };
                let base = (to.0 + (from.0 - to.0) * t, to.1 + (from.1 - to.1) * t);
                seg_dist(p, *from, *to) <= w / 2.0 + slop || seg_dist(p, base, *to) <= head * 0.55 + slop
            }
            Mark::Rectangle { rect, width, .. } => {
                let reach = width * scale / 2.0 + slop;
                within(*rect, p, reach) && !within(*rect, p, -reach)
            }
            Mark::Blur { rect, .. }
            | Mark::Pixelate { rect, .. }
            | Mark::Eraser { rect, .. }
            | Mark::Grab { rect, .. }
            | Mark::Image { rect, .. } => within(*rect, p, 0.0),
            Mark::Text { .. } | Mark::Cursor { .. } => within(self.bbox(), p, slop),
            Mark::Step { at, tail, r, .. } => {
                dist(p, *at) <= r + slop || tail.is_some_and(|t| seg_dist(p, *at, t) <= tail_width(*r) / 2.0 + slop)
            }
        }
    }

    pub(super) fn grips(&self) -> Vec<(Grip, (f32, f32))> {
        match self {
            // a text's grips go on its outline (see the editor's `grips_of`)
            Mark::Stroke { .. } | Mark::Text { .. } | Mark::Step { tail: None, .. } | Mark::Cursor { .. } => Vec::new(),
            Mark::Step { tail: Some(t), .. } => vec![(Grip::To, *t)],
            Mark::Arrow { from, to, .. } => vec![(Grip::From, *from), (Grip::To, *to)],
            Mark::Rectangle { rect, .. }
            | Mark::Blur { rect, .. }
            | Mark::Pixelate { rect, .. }
            | Mark::Eraser { rect, .. }
            | Mark::Grab { rect, .. }
            | Mark::Image { rect, .. } => {
                GRIPS.iter().map(|&e| (Grip::Edge(e), edge_point(*rect, e))).collect()
            }
        }
    }
}

fn span(points: &[(f32, f32)]) -> Rect {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &(x, y) in points {
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    Rect::from_points(x0.floor() as i32, y0.floor() as i32, x1.ceil() as i32, y1.ceil() as i32)
}

pub(super) fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

fn seg_dist(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    dist(p, (a.0 + t * dx, a.1 + t * dy))
}

fn path_dist(points: &[(f32, f32)], p: (f32, f32)) -> f32 {
    match points {
        [a] => dist(p, *a),
        _ => points.windows(2).map(|s| seg_dist(p, s[0], s[1])).fold(f32::INFINITY, f32::min),
    }
}

fn within(r: Rect, (x, y): (f32, f32), pad: f32) -> bool {
    x >= r.x as f32 - pad && x <= r.right() as f32 + pad && y >= r.y as f32 - pad && y <= r.bottom() as f32 + pad
}

pub(super) fn edge_point(r: Rect, e: Edges) -> (f32, f32) {
    let at = |lo: bool, hi: bool, start: i32, len: i32| {
        (if lo {
            start
        } else if hi {
            start + len
        } else {
            start + len / 2
        }) as f32
    };
    (at(e.l, e.r, r.x, r.w), at(e.t, e.b, r.y, r.h))
}

pub(super) fn resize(r: Rect, e: Edges, (dx, dy): (i32, i32), b: Rect, min: i32) -> Rect {
    let (mut x0, mut y0, mut x1, mut y1) = (r.x, r.y, r.right(), r.bottom());
    if e.l {
        x0 = (x0 + dx).max(b.x).min(x1 - min);
    }
    if e.r {
        x1 = (x1 + dx).min(b.right()).max(x0 + min);
    }
    if e.t {
        y0 = (y0 + dy).max(b.y).min(y1 - min);
    }
    if e.b {
        y1 = (y1 + dy).min(b.bottom()).max(y0 + min);
    }
    Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
}

pub(super) fn shift_within(r: Rect, (dx, dy): (i32, i32), b: Rect) -> (i32, i32) {
    let fit = |d: i32, lo: i32, hi: i32| d.clamp(lo.min(hi), lo.max(hi));
    (fit(dx, b.x - r.x, b.right() - r.right()), fit(dy, b.y - r.y, b.bottom() - r.bottom()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Edges {
    pub(super) l: bool,
    pub(super) t: bool,
    pub(super) r: bool,
    pub(super) b: bool,
}

pub(super) const GRIPS: [Edges; 8] = {
    const fn e(l: bool, t: bool, r: bool, b: bool) -> Edges {
        Edges { l, t, r, b }
    }
    [
        e(true, true, false, false),
        e(false, true, false, false),
        e(false, true, true, false),
        e(true, false, false, false),
        e(false, false, true, false),
        e(true, false, false, true),
        e(false, false, false, true),
        e(false, false, true, true),
    ]
};

/// A text's grips: its corners scale it, its sides set the width it wraps at.
pub(super) const TEXT_GRIPS: [Edges; 6] = [GRIPS[0], GRIPS[2], GRIPS[3], GRIPS[4], GRIPS[5], GRIPS[7]];

fn ink_width(width: f32, scale: f32) -> f32 {
    (width * scale).round().max(1.0)
}

/// Pairs each mark with the number the step markers so far reach: steps count 1, 2, 3 … in order.
pub(super) fn numbered<'a>(marks: impl Iterator<Item = &'a Mark>) -> impl Iterator<Item = (&'a Mark, usize)> {
    marks.scan(0, |n, m| {
        if let Mark::Step { .. } = m {
            *n += 1;
        }
        Some((m, *n))
    })
}

fn tail_width(r: f32) -> f32 {
    (r / 4.0).round().max(2.0)
}

/// Draws `m`; `step` is the number a step marker shows; `screen` is the frozen screen grabs show.
pub(super) fn draw_mark(
    c: &Canvas,
    m: &Mark,
    (dx, dy): (f32, f32),
    scale: f32,
    fonts: &Typesetter,
    step: usize,
    screen: &Dib,
) {
    let at = |(x, y): (f32, f32)| (x + dx, y + dy);
    match m {
        Mark::Stroke { highlight, points, color, width } => {
            let pts: Vec<(f32, f32)> = points.iter().map(|&p| at(p)).collect();
            let a = if *highlight { HIGHLIGHTER_ALPHA } else { 1.0 };
            c.polyline(&pts, ink_width(*width, scale), D2D1_COLOR_F { a, ..COLORS[*color] });
        }
        Mark::Arrow { from, to, color, width } => {
            arrow(c, at(*from), at(*to), ink_width(*width, scale), COLORS[*color])
        }
        Mark::Rectangle { rect, color, width } => {
            let (x, y) = at((rect.x as f32, rect.y as f32));
            c.rect_outline(x, y, rect.w as f32, rect.h as f32, ink_width(*width, scale), COLORS[*color]);
        }
        Mark::Eraser { rect, fill } => {
            let (x, y) = at((rect.x as f32, rect.y as f32));
            let rgb = rgba((fill >> 16) as u8, (fill >> 8) as u8, *fill as u8, 1.0);
            c.fill(x, y, rect.w as f32, rect.h as f32, rgb);
        }
        Mark::Text { at: corner, text, color, size, style, width, .. } => {
            let ink = if LIGHT[*color] { STEP_INK } else { COLORS[4] };
            fonts.draw(c, text, style, size * scale, *width, at(*corner), COLORS[*color], ink);
        }
        Mark::Step { at: centre, tail, color, r, .. } => {
            let o = at(*centre);
            if let Some(t) = tail {
                let t = at(*t);
                let len = dist(o, t);
                if len > *r {
                    let from = (o.0 + (t.0 - o.0) * r / len, o.1 + (t.1 - o.1) * r / len);
                    arrow(c, from, t, tail_width(*r), COLORS[*color]);
                }
            }
            c.circle(o, *r, COLORS[*color]);
            let ink = if LIGHT[*color] { STEP_INK } else { COLORS[4] };
            fonts.draw_centred(c, &step.to_string(), r * 1.15, o, ink);
        }
        Mark::Cursor { at: hot, sprite, size } => {
            let [x, y, w, h] = cursor_box(*hot, sprite, *size);
            let (x, y) = at((x, y));
            let img = &sprite.0;
            c.image(&img.px, img.w as usize, (img.w as u32, img.h as u32), [x, y, w, h]);
        }
        Mark::Grab { src, rect } => {
            let whole = Rect { x: 0, y: 0, w: screen.w, h: screen.h };
            if whole.intersect(src) == Some(*src) {
                let (x, y) = at((rect.x as f32, rect.y as f32));
                let from = &screen.pixels()[(src.y * screen.w + src.x) as usize..];
                c.image(from, screen.w as usize, (src.w as u32, src.h as u32), [x, y, rect.w as f32, rect.h as f32]);
            }
        }
        Mark::Image { rect, picture } => {
            let (x, y) = at((rect.x as f32, rect.y as f32));
            let box_ = [x, y, rect.w as f32, rect.h as f32];
            let img = &picture.0;
            let (w, h) = (rect.w.max(1) as u32, rect.h.max(1) as u32);
            if w < img.w / 2 + img.w / 4 && h < img.h / 2 + img.h / 4
                && let Ok(mut shown) = picture.1.lock()
            {
                if shown.as_ref().is_none_or(|s| (s.w, s.h) != (w, h)) {
                    *shown = Some(super::paste::shrunk(img, w, h));
                }
                if let Some(s) = shown.as_ref() {
                    return c.image(&s.px, s.w as usize, (s.w, s.h), box_);
                }
            }
            c.image(&img.px, img.w as usize, (img.w, img.h), box_);
        }
        Mark::Blur { .. } | Mark::Pixelate { .. } => {}
    }
}

fn arrow_head(width: f32, len: f32) -> f32 {
    (width * 3.2 + 7.0).min(len)
}

/// A shaft with a filled head whose size follows the stroke width.
pub(super) fn arrow(c: &Canvas, from: (f32, f32), to: (f32, f32), width: f32, color: D2D1_COLOR_F) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 {
        return;
    }
    let (ux, uy) = (dx / len, dy / len);
    let head = arrow_head(width, len);
    let half = head * 0.55;
    let base = (to.0 - ux * head, to.1 - uy * head);
    // The shaft ends inside the head so the joint doesn't show.
    c.polyline(&[from, (base.0 + ux * head * 0.3, base.1 + uy * head * 0.3)], width, color);
    c.polygon(&[to, (base.0 - uy * half, base.1 + ux * half), (base.0 + uy * half, base.1 - ux * half)], color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_are_hit_on_their_ink() {
        let arrow = Mark::Arrow { from: (0.0, 0.0), to: (100.0, 0.0), color: 0, width: 4.0 };
        assert!(arrow.hit((50.0, 5.0), 1.0));
        assert!(!arrow.hit((50.0, 20.0), 1.0));
        assert!(arrow.hit((82.0, 9.0), 1.0), "the head is wider than the shaft");
        assert!(!arrow.hit((20.0, 9.0), 1.0));

        let area = Rect { x: 100, y: 100, w: 200, h: 100 };
        let rect = Mark::Rectangle { rect: area, color: 0, width: 4.0 };
        assert!(rect.hit((100.0, 150.0), 1.0));
        assert!(rect.hit((95.0, 150.0), 1.0));
        assert!(!rect.hit((200.0, 150.0), 1.0));
        assert!(!rect.hit((80.0, 150.0), 1.0));

        let blur = Mark::Blur { rect: area, radius: 8.0 };
        assert!(blur.hit((200.0, 150.0), 1.0));
        assert!(!blur.hit((80.0, 150.0), 1.0));
    }
}
