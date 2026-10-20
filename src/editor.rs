//! Screenshot markup on the frozen picker. Once a region is picked, a toolbar appears next to
//! it: pen, highlighter, arrow, rectangle, blur and pixelate, five colours, three sizes, undo
//! and redo, and copy / save / done / cancel. The selection keeps handles for resizing and can
//! be dragged while no tool is active.
//!
//! Marks live in screen (client) coordinates, so resizing or moving the selection reframes
//! the shot instead of dragging the marks along. Blur and pixelate hide what's on screen, so
//! they always sit under the drawn shapes.

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Gdi::{BitBlt, FillRect, GdiFlush, HBRUSH, HDC, SRCCOPY};
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

const COLORS: [D2D1_COLOR_F; 5] = [
    rgba(0xE5, 0x40, 0x2F, 1.0),
    rgba(0xFF, 0xD6, 0x0A, 1.0),
    rgba(0x34, 0xC7, 0x59, 1.0),
    rgba(0x0A, 0x84, 0xFF, 1.0),
    rgba(0xFF, 0xFF, 0xFF, 1.0),
];
/// Stroke widths at 96 DPI; the highlighter is four times as wide.
const SIZES: [f32; 3] = [2.0, 4.0, 8.0];
const BAR_BG: D2D1_COLOR_F = rgba(16, 16, 20, 0.92);
const WHITE: D2D1_COLOR_F = rgba(255, 255, 255, 0.96);

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

#[derive(Clone)]
enum Mark {
    Stroke { points: Vec<(f32, f32)>, color: D2D1_COLOR_F, width: f32 },
    Arrow { from: (f32, f32), to: (f32, f32), color: D2D1_COLOR_F, width: f32 },
    Rectangle { a: (f32, f32), b: (f32, f32), color: D2D1_COLOR_F, width: f32 },
    Blur(Rect),
    Pixelate(Rect),
}

impl Mark {
    fn is_effect(&self) -> bool {
        matches!(self, Mark::Blur(_) | Mark::Pixelate(_))
    }

    /// Whether the mark is big enough to keep (a click without a drag isn't an arrow).
    fn worth_keeping(&self) -> bool {
        let far = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() >= 3.0 || (a.1 - b.1).abs() >= 3.0;
        match self {
            Mark::Stroke { .. } => true,
            Mark::Arrow { from, to, .. } => far(*from, *to),
            Mark::Rectangle { a, b, .. } => (a.0 - b.0).abs() >= 3.0 && (a.1 - b.1).abs() >= 3.0,
            Mark::Blur(r) | Mark::Pixelate(r) => r.w >= 3 && r.h >= 3,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
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
        (0..SIZES.len()).map(Item::Size).collect(),
        vec![Item::Undo, Item::Redo],
        vec![Item::Copy, Item::Save, Item::Done, Item::Close],
    ]
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Edges {
    l: bool,
    t: bool,
    r: bool,
    b: bool,
}

enum Hit {
    Bar(Item),
    Handle(Edges),
    Inside,
    Outside,
}

enum Drag {
    /// A mark being drawn; `anchor` is where the press was (boxes grow from it either way).
    Mark { mark: Mark, anchor: (i32, i32) },
    Move { grab: (i32, i32), from: Rect },
    Resize { edges: Edges, grab: (i32, i32), from: Rect },
    /// A toolbar button is held; it fires on release over the same button.
    Bar(Item),
}

pub struct Editor {
    /// The shot's region (client coordinates).
    pub sel: Rect,
    /// The monitor the selection stays on.
    bounds: Rect,
    marks: Vec<Mark>,
    undone: Vec<Mark>,
    /// Bumped whenever the set of blur / pixelate marks changes (invalidates `base`).
    effects_gen: u64,
    tool: Option<Tool>,
    color: usize,
    size: usize,
    drag: Option<Drag>,
    hover: Option<Item>,
    scale: f32,
    /// The selection with its committed effects, and what it was built from.
    base: Option<(Rect, u64, Dib)>,
    /// What shows inside the selection: `base` plus shapes. Rebuilt when `view_dirty`.
    view: Option<Dib>,
    view_dirty: bool,
    /// Toolbar cells (client coordinates) and the rendered bar.
    cells: Vec<(Item, Rect)>,
    pub bar: Rect,
    bar_dib: Option<Dib>,
    bar_dirty: bool,
    tip: Option<(Rect, Dib, Item)>,
}

impl Editor {
    pub fn new(sel: Rect, bounds: Rect, scale: f32) -> Editor {
        let mut ed = Editor {
            sel,
            bounds,
            marks: Vec::new(),
            undone: Vec::new(),
            effects_gen: 0,
            tool: None,
            color: 0,
            size: 1,
            drag: None,
            hover: None,
            scale,
            base: None,
            view: None,
            view_dirty: true,
            cells: Vec::new(),
            bar: Rect::default(),
            bar_dib: None,
            bar_dirty: true,
            tip: None,
        };
        ed.place_bar();
        ed
    }

    fn px(&self, v: f32) -> f32 {
        (v * self.scale).round()
    }

    /// Everything the editor draws; the overlay repaints the union before and after a change.
    pub fn extent(&self) -> Vec<Rect> {
        let mut v = vec![self.sel.inflate(self.px(8.0) as i32), self.bar];
        v.extend(self.tip.as_ref().map(|t| t.0));
        v
    }

    /// The tooltip's rectangle, while one shows.
    pub fn tip(&self) -> Option<Rect> {
        self.tip.as_ref().map(|t| t.0)
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
        self.bar_dirty = true;
        self.tip = None;
    }

    fn hit(&self, p: (i32, i32)) -> Hit {
        if let Some((item, _)) = self.cells.iter().find(|(_, r)| r.contains(p.0, p.1)) {
            return Hit::Bar(*item);
        }
        if self.bar.contains(p.0, p.1) {
            return Hit::Outside; // the bar's padding
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
        if s.contains(p.0, p.1) { Hit::Inside } else { Hit::Outside }
    }

    pub fn cursor(&self, p: (i32, i32)) -> PCWSTR {
        if let Some(Drag::Mark { .. }) = self.drag {
            return IDC_CROSS;
        }
        match self.hit(p) {
            Hit::Bar(_) => IDC_HAND,
            Hit::Handle(e) => match (e.l || e.r, e.t || e.b) {
                (true, true) if (e.l && e.t) || (e.r && e.b) => IDC_SIZENWSE,
                (true, true) => IDC_SIZENESW,
                (true, false) => IDC_SIZEWE,
                _ => IDC_SIZENS,
            },
            Hit::Inside if self.tool.is_some() => IDC_CROSS,
            Hit::Inside => IDC_SIZEALL,
            Hit::Outside => IDC_ARROW,
        }
    }

    fn color(&self) -> D2D1_COLOR_F {
        COLORS[self.color]
    }

    fn width(&self) -> f32 {
        self.px(SIZES[self.size])
    }

    pub fn mouse_down(&mut self, p: (i32, i32)) -> Event {
        let f = (p.0 as f32, p.1 as f32);
        self.drag = match self.hit(p) {
            Hit::Bar(item) => {
                self.bar_dirty = true;
                Some(Drag::Bar(item))
            }
            Hit::Handle(edges) => Some(Drag::Resize { edges, grab: p, from: self.sel }),
            Hit::Inside => match self.tool {
                None => Some(Drag::Move { grab: p, from: self.sel }),
                Some(tool) => {
                    let (color, width) = (self.color(), self.width());
                    let r = Rect { x: p.0, y: p.1, w: 0, h: 0 };
                    let mark = match tool {
                        Tool::Pen => Mark::Stroke { points: vec![f], color, width },
                        // A wide translucent stroke, drawn as one path so overlaps don't darken.
                        Tool::Highlighter => {
                            Mark::Stroke { points: vec![f], color: D2D1_COLOR_F { a: 0.4, ..color }, width: width * 4.0 }
                        }
                        Tool::Arrow => Mark::Arrow { from: f, to: f, color, width },
                        Tool::Rectangle => Mark::Rectangle { a: f, b: f, color, width },
                        Tool::Blur => Mark::Blur(r),
                        Tool::Pixelate => Mark::Pixelate(r),
                    };
                    Some(Drag::Mark { mark, anchor: p })
                }
            },
            Hit::Outside => None,
        };
        if matches!(self.drag, Some(Drag::Mark { .. })) {
            self.view_dirty = true;
        }
        if self.drag.is_some() { Event::Repaint } else { Event::None }
    }

    pub fn mouse_move(&mut self, p: (i32, i32)) -> Event {
        let f = (p.0 as f32, p.1 as f32);
        let b = self.bounds;
        match &mut self.drag {
            Some(Drag::Mark { mark, anchor }) => {
                match mark {
                    Mark::Stroke { points, .. } => {
                        if points.last().is_some_and(|l| (l.0 - f.0).abs() + (l.1 - f.1).abs() >= 1.0) {
                            points.push(f);
                        }
                    }
                    Mark::Arrow { to, .. } => *to = f,
                    Mark::Rectangle { b: corner, .. } => *corner = f,
                    Mark::Blur(r) | Mark::Pixelate(r) => *r = Rect::from_points(anchor.0, anchor.1, p.0, p.1),
                }
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
                let (dx, dy) = (p.0 - grab.0, p.1 - grab.1);
                let (mut x0, mut y0, mut x1, mut y1) = (from.x, from.y, from.right(), from.bottom());
                if edges.l {
                    x0 = (x0 + dx).clamp(b.x, x1 - 1);
                }
                if edges.r {
                    x1 = (x1 + dx).clamp(x0 + 1, b.right());
                }
                if edges.t {
                    y0 = (y0 + dy).clamp(b.y, y1 - 1);
                }
                if edges.b {
                    y1 = (y1 + dy).clamp(y0 + 1, b.bottom());
                }
                self.set_sel(Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
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
                self.bar_dirty = true;
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
            Some(Drag::Bar(item)) => {
                self.bar_dirty = true;
                match self.hit(p) {
                    Hit::Bar(at) if at == item => self.activate(item),
                    _ => Event::Repaint,
                }
            }
            Some(Drag::Mark { mark: m, .. }) => {
                if m.worth_keeping() {
                    self.push(m);
                }
                self.view_dirty = true;
                Event::Repaint
            }
            Some(_) => Event::Repaint,
            None => Event::None,
        }
    }

    fn push(&mut self, m: Mark) {
        if m.is_effect() {
            self.effects_gen += 1;
        }
        self.marks.push(m);
        if self.undone.drain(..).any(|m| m.is_effect()) {
            self.effects_gen += 1;
        }
        self.bar_dirty = true;
    }

    fn undo(&mut self) -> Event {
        let Some(m) = self.marks.pop() else { return Event::None };
        if m.is_effect() {
            self.effects_gen += 1;
        }
        self.undone.push(m);
        self.view_dirty = true;
        self.bar_dirty = true;
        Event::Repaint
    }

    fn redo(&mut self) -> Event {
        let Some(m) = self.undone.pop() else { return Event::None };
        if m.is_effect() {
            self.effects_gen += 1;
        }
        self.marks.push(m);
        self.view_dirty = true;
        self.bar_dirty = true;
        Event::Repaint
    }

    fn activate(&mut self, item: Item) -> Event {
        self.bar_dirty = true;
        match item {
            Item::Tool(t) => self.tool = if self.tool == Some(t) { None } else { Some(t) },
            Item::Color(i) => self.color = i,
            Item::Size(i) => self.size = i,
            Item::Undo => return self.undo(),
            Item::Redo => return self.redo(),
            Item::Copy => return Event::Finish(Output::Copy),
            Item::Save => return Event::Finish(Output::Save),
            Item::Done => return Event::Finish(Output::Default),
            Item::Close => return Event::Cancel,
        }
        Event::Repaint
    }

    /// Keyboard shortcuts; `vk` is a virtual-key code.
    pub fn key(&mut self, vk: u32, ctrl: bool, shift: bool) -> Event {
        const VK_RETURN: u32 = 0x0D;
        const VK_ESCAPE: u32 = 0x1B;
        const VK_OEM_4: u32 = 0xDB; // [
        const VK_OEM_6: u32 = 0xDD; // ]
        if self.drag.is_some() && vk != VK_ESCAPE {
            return Event::None;
        }
        match (vk, ctrl) {
            (VK_ESCAPE, _) => Event::Cancel,
            (VK_RETURN, _) => Event::Finish(Output::Default),
            (0x43, true) => Event::Finish(Output::Copy), // C
            (0x53, true) => Event::Finish(Output::Save), // S
            (0x5A, true) if shift => self.redo(),        // Z
            (0x5A, true) => self.undo(),
            (0x59, true) => self.redo(), // Y
            (VK_OEM_4, false) => self.activate(Item::Size(self.size.saturating_sub(1))),
            (VK_OEM_6, false) => self.activate(Item::Size((self.size + 1).min(SIZES.len() - 1))),
            (_, false) => match TOOLS.iter().find(|(.., key)| *key as u32 == vk) {
                Some((tool, ..)) => self.activate(Item::Tool(*tool)),
                None => Event::None,
            },
            _ => Event::None,
        }
    }

    /// The mouse wheel steps through the sizes.
    pub fn wheel(&mut self, delta: i32) -> Event {
        let size = if delta > 0 { (self.size + 1).min(SIZES.len() - 1) } else { self.size.saturating_sub(1) };
        if size == self.size {
            return Event::None;
        }
        self.activate(Item::Size(size))
    }

    /// Brings `view`, the toolbar and its tooltip up to date. `bright` is the frozen screen,
    /// `dim` its darkened copy (the toolbar sits on it).
    pub fn render(&mut self, bright: &Dib, dim: &Dib, painter: &Painter, font: &IDWriteTextFormat) -> Result<(), String> {
        if self.view_dirty {
            self.render_view(bright, painter)?;
            self.view_dirty = false;
        }
        if self.bar_dirty {
            self.render_bar(dim, painter, font)?;
            self.bar_dirty = false;
        }
        Ok(())
    }

    fn render_view(&mut self, bright: &Dib, painter: &Painter) -> Result<(), String> {
        let s = self.sel;
        let fresh = self.base.as_ref().is_some_and(|(r, g, _)| *r == s && *g == self.effects_gen);
        if !fresh {
            let base = Dib::new(s.w, s.h)?;
            unsafe {
                let _ = BitBlt(base.dc, 0, 0, s.w, s.h, Some(bright.dc), s.x, s.y, SRCCOPY);
                let _ = GdiFlush();
            }
            let mut base = base;
            for m in self.marks.iter().filter(|m| m.is_effect()) {
                apply_effect(&mut base, m, s, self.scale);
            }
            self.base = Some((s, self.effects_gen, base));
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
        let live = match &self.drag {
            Some(Drag::Mark { mark, .. }) => Some(mark),
            _ => None,
        };
        if let Some(m) = live.filter(|m| m.is_effect()) {
            apply_effect(view, m, s, self.scale);
        }
        let shift = (-s.x as f32, -s.y as f32);
        let shapes = self.marks.iter().chain(live).filter(|m| !m.is_effect());
        painter.draw(view, |c| {
            for m in shapes {
                draw_mark(c, m, shift);
            }
        })
    }

    fn render_bar(&mut self, dim: &Dib, painter: &Painter, font: &IDWriteTextFormat) -> Result<(), String> {
        let b = self.bar;
        let dib = Dib::new(b.w, b.h)?;
        unsafe {
            let _ = BitBlt(dib.dc, 0, 0, b.w, b.h, Some(dim.dc), b.x, b.y, SRCCOPY);
        }
        let pressed = match self.drag {
            Some(Drag::Bar(item)) => Some(item),
            _ => None,
        };
        let s = self.scale;
        let (can_undo, can_redo) = (!self.marks.is_empty(), !self.undone.is_empty());
        let groups = groups();
        painter.draw(&dib, |c| {
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
                    Item::Size(i) => self.size == i,
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
                    _ => true,
                };
                let ink = if enabled { WHITE } else { rgba(255, 255, 255, 0.3) };
                draw_icon(c, item, (x + cw / 2.0, y + ch / 2.0), s, ink, self.color);
            }
        })?;
        self.bar_dib = Some(dib);

        // Tooltip for the hovered button, on the side of the bar away from the selection.
        let tip_item = self.hover.filter(|_| pressed.is_none());
        self.tip = match tip_item.and_then(|i| i.tip().map(|t| (i, t))) {
            Some((item, text)) => {
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
                let tip = Dib::new(w, h)?;
                unsafe {
                    let _ = BitBlt(tip.dc, 0, 0, w, h, Some(dim.dc), x, y, SRCCOPY);
                }
                painter.draw(&tip, |c| {
                    c.round_rect(0.0, 0.0, w as f32, h as f32, h as f32 / 2.0, BAR_BG);
                    c.text(text, font, [self.px(10.0), 0.0, tw + 2.0, h as f32], WHITE);
                })?;
                Some((Rect { x, y, w, h }, tip, item))
            }
            None => None,
        };
        Ok(())
    }

    /// Paints the selection (with marks), its handles, the toolbar and the tooltip.
    pub fn paint(&self, hdc: HDC, white: HBRUSH, black: HBRUSH) {
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
            let (cx, cy) = (s.x + s.w / 2, s.y + s.h / 2);
            for (x, y) in [
                (s.x, s.y),
                (cx, s.y),
                (s.right(), s.y),
                (s.x, cy),
                (s.right(), cy),
                (s.x, s.bottom()),
                (cx, s.bottom()),
                (s.right(), s.bottom()),
            ] {
                let outer = Rect { x: x - hs / 2 - 1, y: y - hs / 2 - 1, w: hs + 2, h: hs + 2 };
                FillRect(hdc, &outer.to_win(), black);
                FillRect(hdc, &outer.inflate(-1).to_win(), white);
            }
            if let Some(bar) = &self.bar_dib {
                let b = self.bar;
                let _ = BitBlt(hdc, b.x, b.y, b.w, b.h, Some(bar.dc), 0, 0, SRCCOPY);
            }
            if let Some((r, dib, _)) = &self.tip {
                let _ = BitBlt(hdc, r.x, r.y, r.w, r.h, Some(dib.dc), 0, 0, SRCCOPY);
            }
        }
    }

    /// The finished shot: the selection with every mark, as 0x00RRGGBB rows.
    pub fn pixels(&mut self, bright: &Dib, painter: &Painter) -> Result<(Vec<u32>, u32, u32), String> {
        self.drag = None;
        self.render_view(bright, painter)?;
        let v = self.view.as_ref().expect("rendered above");
        Ok((v.pixels().to_vec(), v.w as u32, v.h as u32))
    }
}

fn draw_mark(c: &Canvas, m: &Mark, (dx, dy): (f32, f32)) {
    let at = |(x, y): (f32, f32)| (x + dx, y + dy);
    match m {
        Mark::Stroke { points, color, width, .. } => {
            let pts: Vec<(f32, f32)> = points.iter().map(|&p| at(p)).collect();
            c.polyline(&pts, *width, *color);
        }
        Mark::Arrow { from, to, color, width } => arrow(c, at(*from), at(*to), *width, *color),
        Mark::Rectangle { a, b, color, width } => {
            let ((x0, y0), (x1, y1)) = (at(*a), at(*b));
            c.rect_outline(x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs(), *width, *color);
        }
        Mark::Blur(_) | Mark::Pixelate(_) => {}
    }
}

/// A shaft with a filled head whose size follows the stroke width.
fn arrow(c: &Canvas, from: (f32, f32), to: (f32, f32), width: f32, color: D2D1_COLOR_F) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 {
        return;
    }
    let (ux, uy) = (dx / len, dy / len);
    let head = (width * 3.2 + 7.0).min(len);
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
    let (r, blur) = match m {
        Mark::Blur(r) => (*r, true),
        Mark::Pixelate(r) => (*r, false),
        _ => return,
    };
    let Some(r) = r.intersect(&sel).map(|r| r.offset(-sel.x, -sel.y)) else { return };
    let stride = dib.w as usize;
    let px = dib.pixels_mut();
    if blur {
        box_blur(px, stride, r, (8.0 * scale).round().max(4.0) as usize);
    } else {
        pixelate(px, stride, r, (12.0 * scale).round().max(6.0) as i32);
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
