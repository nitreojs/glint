//! Screenshot markup on the frozen picker. Once a region is picked, a toolbar appears next to
//! it: pen, highlighter, arrow, rectangle, blur and pixelate, five colours, three sizes, undo
//! and redo, and copy / save / done / cancel. The selection keeps handles for resizing and can
//! be dragged while no tool is active.
//!
//! Marks live in screen (client) coordinates, so resizing or moving the selection reframes
//! the shot instead of dragging the marks along. Blur and pixelate hide what's on screen, so
//! they always sit under the drawn shapes.

use std::time::{Duration, Instant};

use windows::Win32::UI::WindowsAndMessaging::{
    IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE,
};
use windows::core::PCWSTR;

use crate::platform::gfx::Dib;
use crate::platform::rect::Rect;

mod effects;
mod marks;
mod render;
mod toolbar;

use marks::{Edges, Grip, Mark, TOOLS, Tool, dist, resize, shift_within};
use toolbar::Item;

const NOTICE: Duration = Duration::from_millis(1200);
const WHEEL_GROUP: Duration = Duration::from_secs(1);
const WHEEL_DELTA: i32 = 120;

/// What the shot does when it's finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Output {
    /// Enter / done: whatever the settings say (copy, save or both).
    Default,
    Copy,
    Save,
}

pub(crate) enum Event {
    None,
    Repaint,
    Finish(Output),
    Cancel,
}

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

pub(crate) struct Frame {
    canvas: Rect,
    picked: Option<(usize, Rect)>,
    ink: Vec<Rect>,
    rev: u64,
    chrome: Vec<Rect>,
    look: Look,
}

type Snapshot = (Vec<Mark>, Option<usize>);

fn hull(a: Rect, b: Rect) -> Rect {
    Rect::from_points(a.x.min(b.x), a.y.min(b.y), a.right().max(b.right()), a.bottom().max(b.bottom()))
}

pub(crate) struct Editor {
    /// The shot's region (client coordinates).
    pub sel: Rect,
    monitors: Vec<Rect>,
    area: Rect,
    bar_monitor: Rect,
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
    /// Bumped by every view change other than the dragged mark's own.
    rev: u64,
    back: Option<Dib>,
    /// Toolbar cells (client coordinates).
    cells: Vec<(Item, Rect)>,
    pub bar: Rect,
    pill: Option<(Rect, Item, String)>,
}

impl Editor {
    pub(crate) fn new(sel: Rect, monitors: Vec<Rect>, scale: f32) -> Editor {
        let area = monitors.iter().copied().reduce(hull).expect("at least one monitor");
        let mut ed = Editor {
            sel,
            monitors,
            area,
            bar_monitor: area,
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
            rev: 0,
            back: None,
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
    pub(crate) fn extent(&self) -> Vec<Rect> {
        let mut v = vec![self.canvas()];
        v.extend(self.picked_extent());
        v.extend(self.chrome());
        v
    }

    fn canvas(&self) -> Rect {
        self.sel.inflate(self.px(8.0) as i32)
    }

    fn chrome(&self) -> Vec<Rect> {
        let mut v = vec![self.bar];
        v.extend(self.pill.as_ref().map(|p| p.0));
        v
    }

    /// What the dragged mark can repaint; editing an effect reorders the whole effect stack.
    fn ink(&self) -> Vec<Rect> {
        let (m, edit) = match &self.drag {
            Some(Drag::Mark { mark, .. }) => (mark, false),
            Some(Drag::Edit { index, .. }) => (&self.marks[*index], true),
            _ => return Vec::new(),
        };
        let mut v = vec![m.ink(self.scale)];
        if edit && m.is_effect() {
            v.extend(self.marks.iter().filter(|m| m.is_effect()).map(Mark::bbox));
        }
        v
    }

    fn touch(&mut self) {
        self.view_dirty = true;
        self.rev += 1;
    }

    pub(crate) fn frame(&self) -> Frame {
        Frame {
            canvas: self.canvas(),
            picked: self.picked.zip(self.picked_extent()),
            ink: self.ink(),
            rev: self.rev,
            chrome: self.chrome(),
            look: self.look(),
        }
    }

    pub(crate) fn damage(&self, before: &Frame) -> Vec<Rect> {
        let now = self.frame();
        let mut v = Vec::new();
        if now.canvas != before.canvas || now.rev != before.rev {
            v.extend([before.canvas, now.canvas]);
        } else if self.view_dirty {
            v.extend(&before.ink);
            v.extend(&now.ink);
        }
        if now.picked != before.picked {
            v.extend(before.picked.map(|(_, r)| r));
            v.extend(now.picked.map(|(_, r)| r));
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

    pub(crate) fn cursor(&self, p: (i32, i32)) -> PCWSTR {
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

    pub(crate) fn mouse_down(&mut self, p: (i32, i32)) -> Event {
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

    pub(crate) fn mouse_move(&mut self, p: (i32, i32)) -> Event {
        let b = self.area;
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

    pub(crate) fn mouse_up(&mut self, p: (i32, i32)) -> Event {
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
        self.touch();
        Event::Repaint
    }

    fn delete(&mut self) -> Event {
        let Some(i) = self.picked else { return Event::None };
        self.remember();
        self.marks.remove(i);
        self.picked = None;
        self.touch();
        Event::Repaint
    }

    fn escape(&mut self) -> Event {
        if self.picked.take().is_none() {
            return Event::Cancel;
        }
        if let Some(Drag::Edit { index, from, .. }) = self.drag.take() {
            self.marks[index] = from;
        }
        self.touch();
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
                    self.touch();
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
            self.touch();
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
    pub(crate) fn key(&mut self, vk: u32, ctrl: bool, shift: bool) -> Event {
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

    pub(crate) fn wheel(&mut self, delta: i32) -> Event {
        if self.drag.is_some() {
            return Event::None;
        }
        self.wheel_acc += delta;
        let notches = self.wheel_acc / WHEEL_DELTA;
        self.wheel_acc -= notches * WHEEL_DELTA;
        if notches == 0 { Event::None } else { self.nudge(notches) }
    }

    pub(crate) fn notice_left(&self) -> Option<Duration> {
        self.notice.as_ref().map(|(_, at)| NOTICE.saturating_sub(at.elapsed()))
    }

    pub(crate) fn tick(&mut self) -> Event {
        if self.notice.as_ref().is_none_or(|(_, at)| at.elapsed() < NOTICE) {
            return Event::None;
        }
        self.notice = None;
        Event::Repaint
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> Editor {
        Editor::new(Rect { x: 0, y: 0, w: 400, h: 300 }, vec![Rect { x: 0, y: 0, w: 1920, h: 1080 }], 1.0)
    }

    #[test]
    fn dragging_a_mark_repaints_only_around_it() {
        let screen = Rect { x: 0, y: 0, w: 1920, h: 1080 };
        let mut ed = Editor::new(screen, vec![screen], 1.0);
        assert_eq!(screen.intersect(&ed.bar), Some(ed.bar), "a full-screen shot keeps the bar inside");
        ed.marks.push(Mark::Rectangle { rect: Rect { x: 100, y: 100, w: 200, h: 100 }, color: 0, width: 4.0 });
        ed.mouse_down((100, 150));
        for x in 101..140 {
            let before = ed.frame();
            assert!(matches!(ed.mouse_move((x, 160)), Event::Repaint));
            let damage = ed.damage(&before);
            let now = ed.marks[0].bbox();
            assert!(damage.iter().any(|r| r.intersect(&now) == Some(now)));
            assert!(damage.iter().all(|r| r.intersect(&ed.bar).is_none() && r.w < 400 && r.h < 300), "{damage:?}");
        }
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
}
