//! Screenshot markup on the frozen picker. Once a region is picked, a toolbar appears next to
//! it: pen, highlighter, arrow, rectangle, blur and pixelate, five colours, three sizes, undo
//! and redo, and copy / save / done / cancel. The selection keeps handles for resizing and can
//! be dragged while no tool is active.
//!
//! Marks live in screen (client) coordinates, so resizing or moving the selection reframes
//! the shot instead of dragging the marks along. Blur and pixelate hide what's on screen, so
//! they always sit under the drawn shapes.

use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Gdi::{BitBlt, FillRect, GdiFlush, HBRUSH, HDC, RectVisible, SRCCOPY};
use windows::Win32::UI::WindowsAndMessaging::{
    IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE,
};
use windows::core::PCWSTR;

use crate::gfx::{Canvas, Dib, Painter, rgba};
use crate::util::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Pen,
    Highlighter,
    Arrow,
    Rectangle,
    Blur,
    Pixelate,
}

/// Tools with their tooltip and shortcut key.
const TOOLS: [(Tool, &str, u8); 6] = [
    (Tool::Pen, "pen  ·  P", b'P'),
    (Tool::Highlighter, "highlighter  ·  H", b'H'),
    (Tool::Arrow, "arrow  ·  A", b'A'),
    (Tool::Rectangle, "rectangle  ·  R", b'R'),
    (Tool::Blur, "blur  ·  B", b'B'),
    (Tool::Pixelate, "pixelate  ·  X", b'X'),
];

struct Strength {
    presets: [f32; 3],
    step: f32,
    min: f32,
    max: f32,
    name: &'static str,
}

impl Tool {
    fn strength(self) -> Strength {
        let (presets, step, min, max, name) = match self {
            Tool::Pen | Tool::Arrow | Tool::Rectangle => ([2.0, 4.0, 8.0], 1.0, 1.0, 40.0, "width"),
            Tool::Highlighter => ([10.0, 16.0, 28.0], 2.0, 4.0, 80.0, "width"),
            Tool::Blur => ([4.0, 8.0, 16.0], 1.0, 1.0, 40.0, "blur"),
            Tool::Pixelate => ([6.0, 12.0, 20.0], 2.0, 2.0, 80.0, "block size"),
        };
        Strength { presets, step, min, max, name }
    }
}

const COLORS: [D2D1_COLOR_F; 5] = [
    rgba(0xE5, 0x40, 0x2F, 1.0),
    rgba(0xFF, 0xD6, 0x0A, 1.0),
    rgba(0x34, 0xC7, 0x59, 1.0),
    rgba(0x0A, 0x84, 0xFF, 1.0),
    rgba(0xFF, 0xFF, 0xFF, 1.0),
];
const HIGHLIGHTER_ALPHA: f32 = 0.4;
const BAR_BG: D2D1_COLOR_F = rgba(16, 16, 20, 0.92);
const WHITE: D2D1_COLOR_F = rgba(255, 255, 255, 0.96);
const GRIP_EDGE: D2D1_COLOR_F = rgba(20, 20, 24, 0.9);
const NOTICE: Duration = Duration::from_millis(1200);
const WHEEL_GROUP: Duration = Duration::from_secs(1);
const WHEEL_DELTA: i32 = 120;
const MIN_SIDE: i32 = 3;

/// What the shot does when it's finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    /// Enter / done: whatever the settings say (copy, save or both).
    Default,
    Copy,
    Save,
}

pub enum Event {
    None,
    Repaint,
    Finish(Output),
    Cancel,
}

#[derive(Clone, Debug, PartialEq)]
enum Mark {
    Stroke { highlight: bool, points: Vec<(f32, f32)>, color: usize, width: f32 },
    Arrow { from: (f32, f32), to: (f32, f32), color: usize, width: f32 },
    Rectangle { rect: Rect, color: usize, width: f32 },
    Blur { rect: Rect, radius: f32 },
    Pixelate { rect: Rect, block: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Grip {
    From,
    To,
    Edge(Edges),
}

impl Mark {
    fn new(tool: Tool, (x, y): (i32, i32), color: usize, strength: f32) -> Mark {
        let (at, rect) = ((x as f32, y as f32), Rect { x, y, w: 0, h: 0 });
        match tool {
            Tool::Pen | Tool::Highlighter => {
                Mark::Stroke { highlight: tool == Tool::Highlighter, points: vec![at], color, width: strength }
            }
            Tool::Arrow => Mark::Arrow { from: at, to: at, color, width: strength },
            Tool::Rectangle => Mark::Rectangle { rect, color, width: strength },
            Tool::Blur => Mark::Blur { rect, radius: strength },
            Tool::Pixelate => Mark::Pixelate { rect, block: strength },
        }
    }

    fn tool(&self) -> Tool {
        match self {
            Mark::Stroke { highlight: true, .. } => Tool::Highlighter,
            Mark::Stroke { .. } => Tool::Pen,
            Mark::Arrow { .. } => Tool::Arrow,
            Mark::Rectangle { .. } => Tool::Rectangle,
            Mark::Blur { .. } => Tool::Blur,
            Mark::Pixelate { .. } => Tool::Pixelate,
        }
    }

    fn is_effect(&self) -> bool {
        matches!(self, Mark::Blur { .. } | Mark::Pixelate { .. })
    }

    fn strength(&self) -> f32 {
        match self {
            Mark::Stroke { width, .. } | Mark::Arrow { width, .. } | Mark::Rectangle { width, .. } => *width,
            Mark::Blur { radius, .. } => *radius,
            Mark::Pixelate { block, .. } => *block,
        }
    }

    fn strength_mut(&mut self) -> &mut f32 {
        match self {
            Mark::Stroke { width, .. } | Mark::Arrow { width, .. } | Mark::Rectangle { width, .. } => width,
            Mark::Blur { radius, .. } => radius,
            Mark::Pixelate { block, .. } => block,
        }
    }

    fn color(&self) -> Option<usize> {
        match self {
            Mark::Stroke { color, .. } | Mark::Arrow { color, .. } | Mark::Rectangle { color, .. } => Some(*color),
            Mark::Blur { .. } | Mark::Pixelate { .. } => None,
        }
    }

    fn color_mut(&mut self) -> Option<&mut usize> {
        match self {
            Mark::Stroke { color, .. } | Mark::Arrow { color, .. } | Mark::Rectangle { color, .. } => Some(color),
            Mark::Blur { .. } | Mark::Pixelate { .. } => None,
        }
    }

    fn area_mut(&mut self) -> Option<&mut Rect> {
        match self {
            Mark::Rectangle { rect, .. } | Mark::Blur { rect, .. } | Mark::Pixelate { rect, .. } => Some(rect),
            Mark::Stroke { .. } | Mark::Arrow { .. } => None,
        }
    }

    /// Whether the mark is big enough to keep (a click without a drag isn't an arrow).
    fn worth_keeping(&self) -> bool {
        match self {
            Mark::Stroke { .. } => true,
            Mark::Arrow { from, to, .. } => (from.0 - to.0).abs() >= 3.0 || (from.1 - to.1).abs() >= 3.0,
            Mark::Rectangle { rect, .. } | Mark::Blur { rect, .. } | Mark::Pixelate { rect, .. } => {
                rect.w >= MIN_SIDE && rect.h >= MIN_SIDE
            }
        }
    }

    fn bbox(&self) -> Rect {
        match self {
            Mark::Stroke { points, .. } => span(points),
            Mark::Arrow { from, to, .. } => span(&[*from, *to]),
            Mark::Rectangle { rect, .. } | Mark::Blur { rect, .. } | Mark::Pixelate { rect, .. } => *rect,
        }
    }

    fn shifted(&self, dx: i32, dy: i32) -> Mark {
        let shift = |p: &mut (f32, f32)| *p = (p.0 + dx as f32, p.1 + dy as f32);
        let mut m = self.clone();
        match &mut m {
            Mark::Stroke { points, .. } => points.iter_mut().for_each(shift),
            Mark::Arrow { from, to, .. } => {
                shift(from);
                shift(to);
            }
            Mark::Rectangle { rect, .. } | Mark::Blur { rect, .. } | Mark::Pixelate { rect, .. } => {
                *rect = rect.offset(dx, dy)
            }
        }
        m
    }

    fn resized(&self, grip: Grip, (dx, dy): (i32, i32), b: Rect) -> Mark {
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
            (m, Grip::Edge(edges)) => {
                if let Some(r) = m.area_mut() {
                    *r = resize(*r, edges, (dx, dy), b, MIN_SIDE);
                }
            }
            _ => {}
        }
        m
    }

    fn hit(&self, p: (f32, f32), scale: f32) -> bool {
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
            Mark::Blur { rect, .. } | Mark::Pixelate { rect, .. } => within(*rect, p, 0.0),
        }
    }

    fn grips(&self) -> Vec<(Grip, (f32, f32))> {
        match self {
            Mark::Stroke { .. } => Vec::new(),
            Mark::Arrow { from, to, .. } => vec![(Grip::From, *from), (Grip::To, *to)],
            Mark::Rectangle { rect, .. } | Mark::Blur { rect, .. } | Mark::Pixelate { rect, .. } => {
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

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
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

fn edge_point(r: Rect, e: Edges) -> (f32, f32) {
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

fn resize(r: Rect, e: Edges, (dx, dy): (i32, i32), b: Rect, min: i32) -> Rect {
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

fn shift_within(r: Rect, (dx, dy): (i32, i32), b: Rect) -> (i32, i32) {
    let fit = |d: i32, lo: i32, hi: i32| d.clamp(lo.min(hi), lo.max(hi));
    (fit(dx, b.x - r.x, b.right() - r.right()), fit(dy, b.y - r.y, b.bottom() - r.bottom()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
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
    fn tip(self) -> Option<&'static str> {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Edges {
    l: bool,
    t: bool,
    r: bool,
    b: bool,
}

const GRIPS: [Edges; 8] = {
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

fn edge_cursor(e: Edges) -> PCWSTR {
    match (e.l || e.r, e.t || e.b) {
        (true, true) if (e.l && e.t) || (e.r && e.b) => IDC_SIZENWSE,
        (true, true) => IDC_SIZENESW,
        (true, false) => IDC_SIZEWE,
        _ => IDC_SIZENS,
    }
}

fn grip_cursor(grip: Option<Grip>) -> PCWSTR {
    match grip {
        Some(Grip::Edge(e)) => edge_cursor(e),
        Some(Grip::From | Grip::To) => IDC_CROSS,
        None => IDC_SIZEALL,
    }
}

enum Hit {
    Bar(Item),
    Grip(Grip),
    Handle(Edges),
    Mark(usize),
    Inside,
    Outside,
}

enum Drag {
    /// A mark being drawn; `anchor` is where the press was (boxes grow from it either way).
    Mark { mark: Mark, anchor: (i32, i32) },
    Edit { index: usize, grip: Option<Grip>, grab: (i32, i32), from: Mark },
    Move { grab: (i32, i32), from: Rect },
    Resize { edges: Edges, grab: (i32, i32), from: Rect },
    /// A toolbar button is held; it fires on release over the same button.
    Bar(Item),
}

#[derive(PartialEq)]
struct Look {
    hover: Option<Item>,
    pressed: Option<Item>,
    tool: Option<Tool>,
    color: usize,
    size: Option<(Tool, f32)>,
    undo: bool,
    redo: bool,
    pill: Option<(Item, String)>,
}

#[derive(PartialEq)]
pub struct Frame {
    canvas: Vec<Rect>,
    chrome: Vec<Rect>,
    picked: Option<usize>,
    look: Look,
}

type Snapshot = (Vec<Mark>, Option<usize>);

pub struct Editor {
    /// The shot's region (client coordinates).
    pub sel: Rect,
    /// The monitor the selection stays on.
    bounds: Rect,
    marks: Vec<Mark>,
    picked: Option<usize>,
    history: Vec<Snapshot>,
    future: Vec<Snapshot>,
    wheel_group: Option<(usize, Instant)>,
    wheel_acc: i32,
    tool: Option<Tool>,
    color: usize,
    strength: [f32; 6],
    drag: Option<Drag>,
    hover: Option<Item>,
    notice: Option<(String, Instant)>,
    scale: f32,
    /// The selection with its committed effects, and what it was built from.
    base: Option<(Rect, Vec<Mark>, Dib)>,
    /// What shows inside the selection: `base` plus shapes. Rebuilt when `view_dirty`.
    view: Option<Dib>,
    view_dirty: bool,
    /// Toolbar cells (client coordinates).
    cells: Vec<(Item, Rect)>,
    pub bar: Rect,
    pill: Option<(Rect, Item, String)>,
}

impl Editor {
    pub fn new(sel: Rect, bounds: Rect, scale: f32) -> Editor {
        let mut ed = Editor {
            sel,
            bounds,
            marks: Vec::new(),
            picked: None,
            history: Vec::new(),
            future: Vec::new(),
            wheel_group: None,
            wheel_acc: 0,
            tool: None,
            color: 0,
            strength: TOOLS.map(|(t, ..)| t.strength().presets[1]),
            drag: None,
            hover: None,
            notice: None,
            scale,
            base: None,
            view: None,
            view_dirty: true,
            cells: Vec::new(),
            bar: Rect::default(),
            pill: None,
        };
        ed.place_bar();
        ed
    }

    fn px(&self, v: f32) -> f32 {
        (v * self.scale).round()
    }

    /// Everything the editor draws.
    pub fn extent(&self) -> Vec<Rect> {
        let mut v = self.canvas();
        v.extend(self.chrome());
        v
    }

    fn canvas(&self) -> Vec<Rect> {
        let mut v = vec![self.sel.inflate(self.px(8.0) as i32)];
        v.extend(self.picked_extent());
        v
    }

    fn chrome(&self) -> Vec<Rect> {
        let mut v = vec![self.bar];
        v.extend(self.pill.as_ref().map(|p| p.0));
        v
    }

    pub fn frame(&self) -> Frame {
        Frame { canvas: self.canvas(), chrome: self.chrome(), picked: self.picked, look: self.look() }
    }

    pub fn damage(&self, before: &Frame) -> Vec<Rect> {
        let now = self.frame();
        let mut v = Vec::new();
        if self.view_dirty || now.canvas != before.canvas || now.picked != before.picked {
            v.extend(&before.canvas);
            v.extend(&now.canvas);
        }
        if now.chrome != before.chrome || now.look != before.look {
            v.extend(&before.chrome);
            v.extend(&now.chrome);
        }
        v
    }

    fn look(&self) -> Look {
        Look {
            hover: self.hover,
            pressed: self.pressed(),
            tool: self.tool,
            color: self.shown_color(),
            size: self.current(),
            undo: !self.history.is_empty(),
            redo: !self.future.is_empty(),
            pill: self.pill_text(),
        }
    }

    fn pressed(&self) -> Option<Item> {
        match self.drag {
            Some(Drag::Bar(item)) => Some(item),
            _ => None,
        }
    }

    fn picked_mark(&self) -> Option<&Mark> {
        self.picked.map(|i| &self.marks[i])
    }

    fn shown_color(&self) -> usize {
        self.picked_mark().and_then(Mark::color).unwrap_or(self.color)
    }

    fn current(&self) -> Option<(Tool, f32)> {
        match self.picked_mark() {
            Some(m) => Some((m.tool(), m.strength())),
            None => self.tool.map(|t| (t, self.strength[t as usize])),
        }
    }

    fn tip(&self, item: Item) -> Option<String> {
        match (item, self.current()) {
            (Item::Size(n), Some((tool, _))) => {
                let s = tool.strength();
                Some(format!("{}  ·  {}", s.name, s.presets[n]))
            }
            _ => item.tip().map(str::to_owned),
        }
    }

    fn pill_text(&self) -> Option<(Item, String)> {
        if let Some((text, _)) = &self.notice {
            return Some((Item::Size(1), text.clone()));
        }
        let item = self.hover.filter(|_| self.pressed().is_none())?;
        self.tip(item).map(|t| (item, t))
    }

    fn grip_box(&self, (x, y): (f32, f32)) -> Rect {
        let k = self.px(8.0) as i32;
        Rect { x: x.round() as i32 - k, y: y.round() as i32 - k, w: 2 * k, h: 2 * k }
    }

    fn outline(&self, m: &Mark) -> Rect {
        m.bbox().inflate((m.strength() * self.scale / 2.0 + self.px(4.0)).ceil() as i32)
    }

    fn picked_extent(&self) -> Option<Rect> {
        let m = self.picked_mark()?;
        Some(match m {
            Mark::Stroke { .. } => self.outline(m).inflate(2),
            _ => m.bbox().inflate(self.px(8.0) as i32 + 1),
        })
    }

    /// Lays the toolbar out under the selection, or above it, or inside it when neither fits.
    fn place_bar(&mut self) {
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

    fn hit(&self, p: (i32, i32)) -> Hit {
        if let Some((item, _)) = self.cells.iter().find(|(_, r)| r.contains(p.0, p.1)) {
            return Hit::Bar(*item);
        }
        if self.bar.contains(p.0, p.1) {
            return Hit::Outside; // the bar's padding
        }
        let at = (p.0 as f32, p.1 as f32);
        let reach = self.px(8.0);
        let mut grips = self.picked_mark().into_iter().flat_map(Mark::grips);
        if let Some((grip, _)) = grips.find(|(_, q)| dist(at, *q) <= reach) {
            return Hit::Grip(grip);
        }
        let s = self.sel;
        let grip = self.px(6.0) as i32;
        let near = |v: i32, edge: i32| (v - edge).abs() <= grip;
        let within = |v: i32, lo: i32, hi: i32| v >= lo - grip && v <= hi + grip;
        if within(p.0, s.x, s.right()) && within(p.1, s.y, s.bottom()) {
            let edges = Edges { l: near(p.0, s.x), t: near(p.1, s.y), r: near(p.0, s.right()), b: near(p.1, s.bottom()) };
            if edges.l || edges.t || edges.r || edges.b {
                return Hit::Handle(edges);
            }
        }
        if !s.contains(p.0, p.1) {
            return Hit::Outside;
        }
        match self.mark_at(at) {
            Some(i) => Hit::Mark(i),
            None => Hit::Inside,
        }
    }

    fn mark_at(&self, at: (f32, f32)) -> Option<usize> {
        let n = self.marks.len();
        let shapes = (0..n).rev().filter(|&i| !self.marks[i].is_effect());
        let effects = (0..n).rev().filter(|&i| self.marks[i].is_effect());
        self.picked
            .into_iter()
            .chain(shapes)
            .chain(effects)
            .filter(|&i| self.tool.is_none_or(|t| t == self.marks[i].tool()))
            .find(|&i| self.marks[i].hit(at, self.scale))
    }

    pub fn cursor(&self, p: (i32, i32)) -> PCWSTR {
        match &self.drag {
            Some(Drag::Mark { .. }) => return IDC_CROSS,
            Some(Drag::Edit { grip, .. }) => return grip_cursor(*grip),
            _ => {}
        }
        match self.hit(p) {
            Hit::Bar(_) => IDC_HAND,
            Hit::Grip(grip) => grip_cursor(Some(grip)),
            Hit::Handle(e) => edge_cursor(e),
            Hit::Mark(i) if self.picked == Some(i) => IDC_SIZEALL,
            Hit::Mark(_) => IDC_HAND,
            Hit::Inside if self.tool.is_some() => IDC_CROSS,
            Hit::Inside => IDC_SIZEALL,
            Hit::Outside => IDC_ARROW,
        }
    }

    fn grab(&self, index: usize, grip: Option<Grip>, p: (i32, i32)) -> Drag {
        Drag::Edit { index, grip, grab: p, from: self.marks[index].clone() }
    }

    pub fn mouse_down(&mut self, p: (i32, i32)) -> Event {
        let hit = self.hit(p);
        let picked = self.picked;
        match hit {
            Hit::Bar(_) | Hit::Grip(_) => {}
            Hit::Mark(i) => self.picked = Some(i),
            _ => self.picked = None,
        }
        self.drag = match hit {
            Hit::Bar(item) => Some(Drag::Bar(item)),
            Hit::Grip(grip) => self.picked.map(|index| self.grab(index, Some(grip), p)),
            Hit::Mark(index) => Some(self.grab(index, None, p)),
            Hit::Handle(edges) => Some(Drag::Resize { edges, grab: p, from: self.sel }),
            Hit::Inside => match self.tool {
                None => Some(Drag::Move { grab: p, from: self.sel }),
                Some(tool) => {
                    self.view_dirty = true;
                    Some(Drag::Mark { mark: Mark::new(tool, p, self.color, self.strength[tool as usize]), anchor: p })
                }
            },
            Hit::Outside => None,
        };
        if self.drag.is_some() || self.picked != picked { Event::Repaint } else { Event::None }
    }

    pub fn mouse_move(&mut self, p: (i32, i32)) -> Event {
        let b = self.bounds;
        match &mut self.drag {
            Some(Drag::Mark { mark, anchor }) => {
                let q = (p.0.clamp(b.x, b.right()), p.1.clamp(b.y, b.bottom()));
                let f = (q.0 as f32, q.1 as f32);
                match mark {
                    Mark::Stroke { points, .. } => {
                        if points.last().is_some_and(|l| (l.0 - f.0).abs() + (l.1 - f.1).abs() >= 1.0) {
                            points.push(f);
                        }
                    }
                    Mark::Arrow { to, .. } => *to = f,
                    Mark::Rectangle { rect, .. } | Mark::Blur { rect, .. } | Mark::Pixelate { rect, .. } => {
                        *rect = Rect::from_points(anchor.0, anchor.1, q.0, q.1)
                    }
                }
                self.view_dirty = true;
                Event::Repaint
            }
            Some(Drag::Edit { index, grip, grab, from }) => {
                let d = (p.0 - grab.0, p.1 - grab.1);
                let mark = match grip {
                    Some(grip) => from.resized(*grip, d, b),
                    None => {
                        let (dx, dy) = shift_within(from.bbox(), d, b);
                        from.shifted(dx, dy)
                    }
                };
                let i = *index;
                if self.marks[i] == mark {
                    return Event::None;
                }
                self.marks[i] = mark;
                self.view_dirty = true;
                Event::Repaint
            }
            &mut Some(Drag::Move { grab, from }) => {
                let (dx, dy) = (p.0 - grab.0, p.1 - grab.1);
                let x = (from.x + dx).clamp(b.x, (b.right() - from.w).max(b.x));
                let y = (from.y + dy).clamp(b.y, (b.bottom() - from.h).max(b.y));
                self.set_sel(Rect { x, y, ..from })
            }
            &mut Some(Drag::Resize { edges, grab, from }) => {
                self.set_sel(resize(from, edges, (p.0 - grab.0, p.1 - grab.1), b, 1))
            }
            Some(Drag::Bar(_)) => Event::None,
            None => {
                let hover = match self.hit(p) {
                    Hit::Bar(item) => Some(item),
                    _ => None,
                };
                if hover == self.hover {
                    return Event::None;
                }
                self.hover = hover;
                Event::Repaint
            }
        }
    }

    fn set_sel(&mut self, sel: Rect) -> Event {
        if sel == self.sel {
            return Event::None;
        }
        self.sel = sel;
        self.view_dirty = true;
        self.place_bar();
        Event::Repaint
    }

    pub fn mouse_up(&mut self, p: (i32, i32)) -> Event {
        match self.drag.take() {
            Some(Drag::Bar(item)) => match self.hit(p) {
                Hit::Bar(at) if at == item => self.activate(item),
                _ => Event::Repaint,
            },
            Some(Drag::Mark { mark, .. }) => {
                if mark.worth_keeping() {
                    self.remember();
                    self.marks.push(mark);
                    self.picked = Some(self.marks.len() - 1);
                }
                self.view_dirty = true;
                Event::Repaint
            }
            Some(Drag::Edit { index, from, .. }) => {
                if self.marks[index] != from {
                    let mut before = self.marks.clone();
                    before[index] = from;
                    self.record((before, Some(index)));
                }
                self.view_dirty = true;
                Event::Repaint
            }
            Some(_) => Event::Repaint,
            None => Event::None,
        }
    }

    fn remember(&mut self) {
        self.record((self.marks.clone(), self.picked));
    }

    fn record(&mut self, before: Snapshot) {
        self.history.push(before);
        self.future.clear();
        self.wheel_group = None;
    }

    fn undo(&mut self) -> Event {
        self.travel(true)
    }

    fn redo(&mut self) -> Event {
        self.travel(false)
    }

    fn travel(&mut self, back: bool) -> Event {
        let (from, to) =
            if back { (&mut self.history, &mut self.future) } else { (&mut self.future, &mut self.history) };
        let Some((marks, picked)) = from.pop() else { return Event::None };
        to.push((std::mem::replace(&mut self.marks, marks), self.picked));
        self.picked = picked.filter(|&i| self.tool.is_none_or(|t| self.marks.get(i).is_some_and(|m| m.tool() == t)));
        self.wheel_group = None;
        self.view_dirty = true;
        Event::Repaint
    }

    fn delete(&mut self) -> Event {
        let Some(i) = self.picked else { return Event::None };
        self.remember();
        self.marks.remove(i);
        self.picked = None;
        self.view_dirty = true;
        Event::Repaint
    }

    fn escape(&mut self) -> Event {
        if self.picked.take().is_none() {
            return Event::Cancel;
        }
        if let Some(Drag::Edit { index, from, .. }) = self.drag.take() {
            self.marks[index] = from;
        }
        self.view_dirty = true;
        Event::Repaint
    }

    fn activate(&mut self, item: Item) -> Event {
        match item {
            Item::Tool(t) => {
                self.tool = if self.tool == Some(t) { None } else { Some(t) };
                if self.tool.is_some_and(|t| self.picked_mark().is_some_and(|m| m.tool() != t)) {
                    self.picked = None;
                }
            }
            Item::Color(c) => {
                self.color = c;
                if let Some(i) = self.picked
                    && self.marks[i].color().is_some_and(|old| old != c)
                {
                    self.remember();
                    if let Some(slot) = self.marks[i].color_mut() {
                        *slot = c;
                    }
                    self.view_dirty = true;
                }
            }
            Item::Size(n) => match self.current() {
                Some((tool, _)) => return self.set_strength(tool.strength().presets[n], false),
                None => return Event::None,
            },
            Item::Undo => return self.undo(),
            Item::Redo => return self.redo(),
            Item::Copy => return Event::Finish(Output::Copy),
            Item::Save => return Event::Finish(Output::Save),
            Item::Done => return Event::Finish(Output::Default),
            Item::Close => return Event::Cancel,
        }
        Event::Repaint
    }

    fn set_strength(&mut self, v: f32, wheel: bool) -> Event {
        let Some((tool, _)) = self.current() else { return Event::None };
        let spec = tool.strength();
        let v = v.clamp(spec.min, spec.max);
        self.strength[tool as usize] = v;
        if let Some(i) = self.picked
            && self.marks[i].strength() != v
        {
            let grouped = wheel && self.wheel_group.is_some_and(|(g, at)| g == i && at.elapsed() < WHEEL_GROUP);
            if !grouped {
                self.remember();
            }
            *self.marks[i].strength_mut() = v;
            if wheel {
                self.wheel_group = Some((i, Instant::now()));
            }
            self.view_dirty = true;
        }
        if wheel {
            self.notice = Some((format!("{}  ·  {v}", spec.name), Instant::now()));
        }
        Event::Repaint
    }

    fn nudge(&mut self, steps: i32) -> Event {
        let Some((tool, v)) = self.current() else { return Event::None };
        self.set_strength(v + steps as f32 * tool.strength().step, true)
    }

    /// Keyboard shortcuts; `vk` is a virtual-key code.
    pub fn key(&mut self, vk: u32, ctrl: bool, shift: bool) -> Event {
        const VK_BACK: u32 = 0x08;
        const VK_RETURN: u32 = 0x0D;
        const VK_ESCAPE: u32 = 0x1B;
        const VK_SPACE: u32 = 0x20;
        const VK_DELETE: u32 = 0x2E;
        const VK_OEM_4: u32 = 0xDB; // [
        const VK_OEM_6: u32 = 0xDD; // ]
        if vk == VK_ESCAPE {
            return self.escape();
        }
        if self.drag.is_some() {
            return Event::None;
        }
        match (vk, ctrl) {
            (VK_RETURN | VK_SPACE, _) => Event::Finish(Output::Default),
            (0x43, true) => Event::Finish(Output::Copy), // C
            (0x53, true) => Event::Finish(Output::Save), // S
            (0x5A, true) if shift => self.redo(),        // Z
            (0x5A, true) => self.undo(),
            (0x59, true) => self.redo(), // Y
            (VK_DELETE | VK_BACK, false) => self.delete(),
            (VK_OEM_4, false) => self.nudge(-1),
            (VK_OEM_6, false) => self.nudge(1),
            (_, false) => match TOOLS.iter().find(|(.., key)| *key as u32 == vk) {
                Some((tool, ..)) => self.activate(Item::Tool(*tool)),
                None => Event::None,
            },
            _ => Event::None,
        }
    }

    pub fn wheel(&mut self, delta: i32) -> Event {
        if self.drag.is_some() {
            return Event::None;
        }
        self.wheel_acc += delta;
        let notches = self.wheel_acc / WHEEL_DELTA;
        self.wheel_acc -= notches * WHEEL_DELTA;
        if notches == 0 { Event::None } else { self.nudge(notches) }
    }

    pub fn notice_left(&self) -> Option<Duration> {
        self.notice.as_ref().map(|(_, at)| NOTICE.saturating_sub(at.elapsed()))
    }

    pub fn tick(&mut self) -> Event {
        if self.notice.as_ref().is_none_or(|(_, at)| at.elapsed() < NOTICE) {
            return Event::None;
        }
        self.notice = None;
        Event::Repaint
    }

    /// Brings `view` and the pill's placement up to date. `bright` is the frozen screen.
    pub fn render(&mut self, bright: &Dib, painter: &Painter, font: &IDWriteTextFormat) -> Result<(), String> {
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

    /// Tooltip for the hovered button, on the side of the bar away from the selection.
    fn place_pill(&self, item: Item, text: &str, painter: &Painter, font: &IDWriteTextFormat) -> Rect {
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

    fn draw_bar(&self, c: &Canvas) {
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

    /// Paints the selection (with marks), its handles, the picked mark's handles, the toolbar and the pill.
    pub fn paint(
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
                for e in crate::overlay::ring(outer, inner) {
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
    pub fn pixels(&mut self, bright: &Dib, painter: &Painter) -> Result<(Vec<u32>, u32, u32), String> {
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
        for e in crate::overlay::ring(outer, r) {
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

fn ink_width(width: f32, scale: f32) -> f32 {
    (width * scale).round().max(1.0)
}

fn draw_mark(c: &Canvas, m: &Mark, (dx, dy): (f32, f32), scale: f32) {
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
        Mark::Blur { .. } | Mark::Pixelate { .. } => {}
    }
}

fn arrow_head(width: f32, len: f32) -> f32 {
    (width * 3.2 + 7.0).min(len)
}

/// A shaft with a filled head whose size follows the stroke width.
fn arrow(c: &Canvas, from: (f32, f32), to: (f32, f32), width: f32, color: D2D1_COLOR_F) {
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

/// Applies a blur / pixelate mark to `dib`, which shows the screen area `sel`.
fn apply_effect(dib: &mut Dib, m: &Mark, sel: Rect, scale: f32) {
    let (r, blur, strength) = match m {
        Mark::Blur { rect, radius } => (*rect, true, *radius),
        Mark::Pixelate { rect, block } => (*rect, false, *block),
        _ => return,
    };
    let Some(r) = r.intersect(&sel).map(|r| r.offset(-sel.x, -sel.y)) else { return };
    let stride = dib.w as usize;
    let px = dib.pixels_mut();
    if blur {
        box_blur(px, stride, r, (strength * scale).round().max(1.0) as usize);
    } else {
        pixelate(px, stride, r, (strength * scale).round().max(2.0) as i32);
    }
}

/// Three box passes per axis (close to a Gaussian), clamped to the area's own edges so
/// nothing from outside the box bleeds in.
fn box_blur(px: &mut [u32], stride: usize, r: Rect, radius: usize) {
    let (w, h) = (r.w as usize, r.h as usize);
    let mut line = vec![0u32; w.max(h)];
    let mut tmp = vec![0u32; w.max(h)];
    for _ in 0..3 {
        for y in 0..h {
            let row = (r.y as usize + y) * stride + r.x as usize;
            line[..w].copy_from_slice(&px[row..row + w]);
            box_pass(&mut line[..w], &mut tmp[..w], radius);
            px[row..row + w].copy_from_slice(&line[..w]);
        }
        for x in 0..w {
            let col = r.y as usize * stride + r.x as usize + x;
            for y in 0..h {
                line[y] = px[col + y * stride];
            }
            box_pass(&mut line[..h], &mut tmp[..h], radius);
            for y in 0..h {
                px[col + y * stride] = line[y];
            }
        }
    }
}

fn box_pass(line: &mut [u32], tmp: &mut [u32], radius: usize) {
    let n = line.len() as isize;
    let get = |i: isize| line[i.clamp(0, n - 1) as usize];
    let ch = |p: u32| [(p >> 16) & 255, (p >> 8) & 255, p & 255];
    let div = (2 * radius + 1) as u32;
    let mut sum = [0u32; 3];
    for k in -(radius as isize)..=radius as isize {
        for (s, c) in sum.iter_mut().zip(ch(get(k))) {
            *s += c;
        }
    }
    for i in 0..n {
        tmp[i as usize] = ((sum[0] / div) << 16) | ((sum[1] / div) << 8) | (sum[2] / div);
        let (add, sub) = (ch(get(i + radius as isize + 1)), ch(get(i - radius as isize)));
        for k in 0..3 {
            sum[k] = sum[k] + add[k] - sub[k];
        }
    }
    line.copy_from_slice(tmp);
}

/// Averages `block`-sized squares, aligned to the area's top-left corner.
fn pixelate(px: &mut [u32], stride: usize, r: Rect, block: i32) {
    let mut by = r.y;
    while by < r.bottom() {
        let bh = block.min(r.bottom() - by);
        let mut bx = r.x;
        while bx < r.right() {
            let bw = block.min(r.right() - bx);
            let mut sum = [0u64; 3];
            for y in by..by + bh {
                for x in bx..bx + bw {
                    let p = px[y as usize * stride + x as usize];
                    sum[0] += ((p >> 16) & 255) as u64;
                    sum[1] += ((p >> 8) & 255) as u64;
                    sum[2] += (p & 255) as u64;
                }
            }
            let n = (bw * bh) as u64;
            let avg = ((sum[0] / n) as u32) << 16 | ((sum[1] / n) as u32) << 8 | (sum[2] / n) as u32;
            for y in by..by + bh {
                for x in bx..bx + bw {
                    px[y as usize * stride + x as usize] = avg;
                }
            }
            bx += block;
        }
        by += block;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> Editor {
        Editor::new(Rect { x: 0, y: 0, w: 400, h: 300 }, Rect { x: 0, y: 0, w: 1920, h: 1080 }, 1.0)
    }

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

    #[test]
    fn tools_pick_only_their_own_kind() {
        let mut ed = editor();
        ed.marks.push(Mark::Blur { rect: Rect { x: 50, y: 50, w: 200, h: 150 }, radius: 8.0 });
        ed.marks.push(Mark::Arrow { from: (60.0, 100.0), to: (240.0, 100.0), color: 0, width: 4.0 });
        let (on_arrow, on_blur) = ((150, 100), (150, 170));
        assert!(matches!(ed.hit(on_arrow), Hit::Mark(1)));
        assert!(matches!(ed.hit(on_blur), Hit::Mark(0)));
        ed.tool = Some(Tool::Arrow);
        assert!(matches!(ed.hit(on_arrow), Hit::Mark(1)));
        assert!(matches!(ed.hit(on_blur), Hit::Inside));
        ed.tool = Some(Tool::Blur);
        assert!(matches!(ed.hit(on_arrow), Hit::Mark(0)));
    }

    #[test]
    fn undo_restores_moved_and_resized_marks() {
        let mut ed = editor();
        let start = Rect { x: 100, y: 100, w: 100, h: 80 };
        ed.marks.push(Mark::Rectangle { rect: start, color: 0, width: 4.0 });
        let area = |ed: &Editor| match &ed.marks[0] {
            Mark::Rectangle { rect, .. } => *rect,
            _ => unreachable!(),
        };
        ed.mouse_down((100, 140));
        ed.mouse_move((130, 150));
        ed.mouse_up((130, 150));
        let moved = Rect { x: 130, y: 110, ..start };
        assert_eq!(area(&ed), moved);
        ed.mouse_down((230, 190));
        ed.mouse_move((250, 200));
        ed.mouse_up((250, 200));
        let resized = Rect { w: 120, h: 90, ..moved };
        assert_eq!(area(&ed), resized);
        ed.wheel(WHEEL_DELTA);
        ed.wheel(WHEEL_DELTA);
        assert_eq!(ed.marks[0].strength(), 6.0);

        ed.undo();
        assert_eq!(ed.marks[0].strength(), 4.0, "one wheel gesture is one step");
        ed.undo();
        assert_eq!(area(&ed), moved);
        ed.undo();
        assert_eq!(area(&ed), start);
        ed.redo();
        ed.redo();
        assert_eq!(area(&ed), resized);
    }

    #[test]
    fn blur_and_pixelate_stay_inside_their_box() {
        // 8x4 image: left half black, right half white; effect on the middle 4x2 only.
        let (stride, h) = (8usize, 4usize);
        let fresh = || (0..stride * h).map(|i| if i % stride < 4 { 0 } else { 0x00FF_FFFF }).collect::<Vec<u32>>();
        let area = Rect { x: 2, y: 1, w: 4, h: 2 };
        for blur in [true, false] {
            let mut px = fresh();
            if blur { box_blur(&mut px, stride, area, 2) } else { pixelate(&mut px, stride, area, 4) }
            for (i, (&a, &b)) in px.iter().zip(&fresh()).enumerate() {
                let (x, y) = ((i % stride) as i32, (i / stride) as i32);
                if area.contains(x, y) {
                    assert!(a != 0 && a != 0x00FF_FFFF, "{blur} {x},{y} should be mixed, got {a:#x}");
                } else {
                    assert_eq!(a, b, "{blur} {x},{y} outside the box changed");
                }
            }
        }
    }
}
