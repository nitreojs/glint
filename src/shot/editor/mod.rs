//! Screenshot markup on the frozen picker. Once a region is picked, a toolbar appears next to
//! it: pen, highlighter, arrow, rectangle, blur, pixelate, text and step markers, five colours,
//! three sizes, undo and redo, and copy / save / upload / pin / done / cancel. The selection
//! keeps handles for resizing and can be dragged while no tool is active. In canvas mode
//! ([`Editor::canvas`]) there is no selection until one is picked: the whole screen is drawable.
//!
//! Marks live in screen (client) coordinates, so resizing or moving the selection reframes
//! the shot instead of dragging the marks along. Blur and pixelate hide what's on screen, so
//! they always sit under the drawn shapes.

use std::time::{Duration, Instant};

use windows::Win32::UI::Input::KeyboardAndMouse::{GetDoubleClickTime, GetKeyState, VK_CONTROL};
use windows::Win32::UI::WindowsAndMessaging::{
    IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_IBEAM, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE,
};
use windows::core::PCWSTR;

use crate::platform::gfx::Dib;
use crate::platform::rect::Rect;
use crate::platform::snap;

mod effects;
mod marks;
mod render;
mod text;
mod toolbar;
mod typing;

use marks::{COLORS, Edges, Grip, Mark, TOOLS, Tool, dist, resize, shift_within};
use text::Typesetter;
use toolbar::{Item, nearest};
use typing::Typing;

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
    Upload,
    Pin,
}

pub(crate) enum Event {
    None,
    Repaint,
    Finish(Output),
    Cancel,
    /// Canvas mode's region tool: pick (or re-pick) the shot's region.
    PickRegion,
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
    /// On the text being typed.
    Typing,
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
    caret: Option<Rect>,
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
    /// The shot's region (client coordinates); the whole screen in canvas mode until a region is picked.
    pub sel: Rect,
    /// A multi-window shot: only these parts of `sel` are kept, the rest is transparent.
    cutout: Option<Vec<Rect>>,
    /// False in canvas mode until a region is picked.
    selected: bool,
    /// The toolbar has the region tool (canvas mode).
    region: bool,
    /// Last mouse position; with no selection, the shot is the monitor under it.
    mouse: (i32, i32),
    monitors: Vec<Rect>,
    /// Edges the selection snaps to while it's moved or resized (Ctrl held: no snapping).
    snap: snap::Edges,
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
    strength: [f32; TOOLS.len()],
    drag: Option<Drag>,
    typing: Option<Typing>,
    /// A UTF-16 high surrogate waiting for its pair.
    surrogate: Option<u16>,
    last_click: Option<(Instant, (i32, i32))>,
    fonts: Typesetter,
    hover: Option<Item>,
    notice: Option<(String, Instant)>,
    scale: f32,
    /// The selection with its committed effects, and what it was built from.
    base: Option<(Rect, Vec<Mark>, Dib)>,
    /// What shows inside the selection: `base` plus shapes. Rebuilt when `view_dirty`.
    view: Option<Dib>,
    view_dirty: bool,
    /// Part of the view (client coordinates) to redraw when only the dragged mark changed.
    patch: Option<Rect>,
    /// Bumped by every view change other than the dragged mark's own.
    rev: u64,
    back: Option<Dib>,
    /// Toolbar cells (client coordinates).
    cells: Vec<(Item, Rect)>,
    pub bar: Rect,
    pill: Option<(Rect, Item, String)>,
}

impl Editor {
    /// Select-first: `sel` is picked; `cutout` keeps only those parts of it (multi-window shots).
    pub(crate) fn new(sel: Rect, cutout: Option<Vec<Rect>>, monitors: Vec<Rect>, scale: f32) -> Editor {
        let area = monitors.iter().copied().reduce(hull).expect("at least one monitor");
        let centre = (sel.x + sel.w / 2, sel.y + sel.h / 2);
        Editor::build(sel, area, cutout, true, monitors, scale, centre)
    }

    /// Canvas mode: the whole `screen` is drawable, with a region tool to pick the shot's region later.
    pub(crate) fn canvas(screen: Rect, monitors: Vec<Rect>, scale: f32, cursor: (i32, i32)) -> Editor {
        Editor::build(screen, screen, None, false, monitors, scale, cursor)
    }

    fn build(
        sel: Rect,
        area: Rect,
        cutout: Option<Vec<Rect>>,
        selected: bool,
        monitors: Vec<Rect>,
        scale: f32,
        mouse: (i32, i32),
    ) -> Editor {
        let mut ed = Editor {
            sel,
            cutout,
            selected,
            region: !selected,
            mouse,
            monitors,
            snap: snap::Edges::default(),
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
            typing: None,
            surrogate: None,
            last_click: None,
            fonts: Typesetter::new(),
            hover: None,
            notice: None,
            scale,
            base: None,
            view: None,
            view_dirty: true,
            patch: None,
            rev: 0,
            back: None,
            cells: Vec::new(),
            bar: Rect::default(),
            pill: None,
        };
        ed.place_bar();
        ed
    }

    /// Canvas mode picked a region: from now on it behaves like select-first, keeping the marks.
    pub(crate) fn set_selection(&mut self, sel: Rect, cutout: Option<Vec<Rect>>) {
        self.commit();
        self.sel = sel;
        self.cutout = cutout;
        self.selected = true;
        self.base = None;
        self.place_bar();
        self.touch();
    }

    /// What the selection snaps to (client coordinates).
    pub(crate) fn set_snap(&mut self, rects: Vec<Rect>) {
        self.snap = snap::edges(&rects);
    }

    pub(crate) fn has_selection(&self) -> bool {
        self.selected
    }

    /// What the finished shot covers: the selection, or with none the monitor under the mouse.
    fn output_rect(&self) -> Rect {
        if self.selected { self.sel } else { nearest(&self.monitors, self.mouse) }
    }

    fn px(&self, v: f32) -> f32 {
        (v * self.scale).round()
    }

    /// Everything the editor draws.
    pub(crate) fn extent(&self) -> Vec<Rect> {
        let mut v = vec![self.drawn()];
        v.extend(self.picked_extent());
        v.extend(self.chrome());
        v
    }

    fn drawn(&self) -> Rect {
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
            canvas: self.drawn(),
            picked: self.picked.zip(self.picked_extent()),
            caret: self.caret_rect(),
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
        } else if self.view_dirty || self.patch.is_some() {
            v.extend(&before.ink);
            v.extend(&now.ink);
        }
        if now.picked != before.picked {
            v.extend(before.picked.map(|(_, r)| r));
            v.extend(now.picked.map(|(_, r)| r));
        }
        if now.caret != before.caret {
            v.extend(before.caret);
            v.extend(now.caret);
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

    /// The dashed box around a picked mark that has no grips of its own.
    fn outline(&self, m: &Mark) -> Rect {
        match m {
            Mark::Stroke { .. } => m.bbox().inflate((m.strength() * self.scale / 2.0 + self.px(4.0)).ceil() as i32),
            _ => m.bbox().inflate(self.px(4.0) as i32),
        }
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
        if let Some(t) = &self.typing
            && self.outline(&self.marks[t.index]).contains(p.0, p.1)
        {
            return Hit::Typing;
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
        if self.selected && within(p.0, s.x, s.right()) && within(p.1, s.y, s.bottom()) {
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
            Hit::Typing => IDC_IBEAM,
            Hit::Inside => match self.tool {
                Some(Tool::Text) => IDC_IBEAM,
                Some(_) => IDC_CROSS,
                None if self.selected => IDC_SIZEALL,
                None => IDC_ARROW,
            },
            Hit::Outside => IDC_ARROW,
        }
    }

    fn grab(&self, index: usize, grip: Option<Grip>, p: (i32, i32)) -> Drag {
        Drag::Edit { index, grip, grab: p, from: self.marks[index].clone() }
    }

    /// Whether a press at `p` completes a double-click.
    fn double_click(&mut self, p: (i32, i32)) -> bool {
        let window = Duration::from_millis(unsafe { GetDoubleClickTime() } as u64);
        let slop = self.px(4.0) as i32;
        let double = self
            .last_click
            .is_some_and(|(at, q)| at.elapsed() <= window && (q.0 - p.0).abs() <= slop && (q.1 - p.1).abs() <= slop);
        self.last_click = if double { None } else { Some((Instant::now(), p)) };
        double
    }

    pub(crate) fn mouse_down(&mut self, p: (i32, i32)) -> Event {
        self.mouse = p;
        let hit = self.hit(p);
        let double = self.double_click(p);
        match hit {
            Hit::Typing => return self.place_caret(p),
            Hit::Bar(_) => {}
            _ if self.commit() => return Event::Repaint,
            Hit::Mark(i)
                if double
                    && matches!(self.marks[i], Mark::Text { .. })
                    && matches!(self.tool, None | Some(Tool::Text)) =>
            {
                return self.retype(i, p);
            }
            _ => {}
        }
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
                None if self.selected => Some(Drag::Move { grab: p, from: self.sel }),
                None => None,
                Some(Tool::Text) => return self.start_typing(p),
                Some(tool) => {
                    let mut mark = Mark::new(tool, p, self.color, self.strength[tool as usize]);
                    self.refit(&mut mark);
                    Some(Drag::Mark { mark, anchor: p })
                }
            },
            Hit::Typing | Hit::Outside => None,
        };
        if let Some(Drag::Mark { .. }) = self.drag {
            self.repaint_ink(Vec::new());
        }
        if self.drag.is_some() || self.picked != picked { Event::Repaint } else { Event::None }
    }

    pub(crate) fn mouse_move(&mut self, p: (i32, i32)) -> Event {
        self.mouse = p;
        let b = self.area;
        let slop = self.px(4.0);
        let th = if unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0 { -1 } else { snap::threshold(self.scale) };
        let before = self.ink();
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
                    Mark::Step { at, tail, r, .. } => *tail = (dist(*at, f) > *r + slop).then_some(f),
                    Mark::Text { .. } => {}
                }
                self.repaint_ink(before);
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
                self.repaint_ink(before);
                Event::Repaint
            }
            &mut Some(Drag::Move { grab, from }) => {
                let moved =
                    snap_moved(Rect { x: from.x + p.0 - grab.0, y: from.y + p.1 - grab.1, ..from }, &self.snap, th);
                let x = moved.x.clamp(b.x, (b.right() - from.w).max(b.x));
                let y = moved.y.clamp(b.y, (b.bottom() - from.h).max(b.y));
                self.set_sel(Rect { x, y, ..from })
            }
            &mut Some(Drag::Resize { edges, grab, from }) => {
                let resized = resize(from, edges, (p.0 - grab.0, p.1 - grab.1), b, 1);
                self.set_sel(snap_resized(resized, edges, &self.snap, th))
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
        if sel == self.sel || !self.selected {
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
                self.settled(&mark);
                if mark.worth_keeping() {
                    self.remember();
                    self.marks.push(mark);
                    self.picked = Some(self.marks.len() - 1);
                }
                Event::Repaint
            }
            Some(Drag::Edit { index, from, .. }) => {
                self.settled(&self.marks[index].clone());
                if self.marks[index] != from {
                    let mut before = self.marks.clone();
                    before[index] = from;
                    self.record((before, Some(index)));
                }
                Event::Repaint
            }
            Some(_) => Event::Repaint,
            None => Event::None,
        }
    }

    /// The view is stale where the dragged mark was (`before`, from [`Editor::ink`]) and is now.
    fn repaint_ink(&mut self, before: Vec<Rect>) {
        for r in before.into_iter().chain(self.ink()) {
            self.patch = Some(self.patch.map_or(r, |p| hull(p, r)));
        }
    }

    /// A drag of `m` ended: an effect joins (or rejoins) the committed stack, a shape stays put.
    fn settled(&mut self, m: &Mark) {
        if m.is_effect() {
            self.view_dirty = true;
        } else {
            self.repaint_ink(vec![m.ink(self.scale)]);
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
        self.commit();
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
        if self.commit() {
            return Event::Repaint;
        }
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
            Item::Region => {
                self.commit();
                return Event::PickRegion;
            }
            Item::Tool(t) => {
                self.commit();
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
                    if self.typing.is_none() {
                        self.remember();
                    }
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
            Item::Upload => return Event::Finish(Output::Upload),
            Item::Pin => return Event::Finish(Output::Pin),
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
            if !grouped && self.typing.is_none() {
                self.remember();
            }
            let mut mark = self.marks[i].clone();
            *mark.strength_mut() = v;
            self.refit(&mut mark);
            self.marks[i] = mark;
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
        const VK_1: u32 = 0x31;
        const VK_OEM_4: u32 = 0xDB; // [
        const VK_OEM_6: u32 = 0xDD; // ]
        if vk == VK_ESCAPE {
            return self.escape();
        }
        if self.drag.is_some() {
            return Event::None;
        }
        if self.typing.is_some()
            && let Some(event) = self.typing_key(vk, ctrl)
        {
            return event;
        }
        match (vk, ctrl) {
            (VK_RETURN | VK_SPACE, _) => Event::Finish(Output::Default),
            (0x43, true) => Event::Finish(Output::Copy),   // C
            (0x53, true) => Event::Finish(Output::Save),   // S
            (0x55, true) => Event::Finish(Output::Upload), // U
            (0x5A, true) if shift => self.redo(),          // Z
            (0x5A, true) => self.undo(),
            (0x59, true) => self.redo(), // Y
            (VK_DELETE | VK_BACK, false) => self.delete(),
            (VK_OEM_4, false) => self.nudge(-1),
            (VK_OEM_6, false) => self.nudge(1),
            (0x51, false) if self.region => self.activate(Item::Region), // Q
            (_, false) if shift => match vk.checked_sub(VK_1) {
                Some(i) if (i as usize) < COLORS.len() => self.activate(Item::Color(i as usize)),
                _ => Event::None,
            },
            (_, false) => match TOOLS.iter().enumerate().find(|(i, (.., key))| tool_key(vk, *i, *key)) {
                Some((_, (tool, ..))) => self.activate(Item::Tool(*tool)),
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

/// `r` moved so whichever of its edges is nearer a snap edge (per axis) lands on it.
fn snap_moved(r: Rect, to: &snap::Edges, th: i32) -> Rect {
    let axis = |lo: i32, len: i32, edges: &[i32]| {
        let a = snap::snap_edge(lo, edges, th) - lo;
        let b = snap::snap_edge(lo + len, edges, th) - (lo + len);
        lo + if a != 0 && (b == 0 || a.abs() <= b.abs()) { a } else { b }
    };
    Rect { x: axis(r.x, r.w, &to.xs), y: axis(r.y, r.h, &to.ys), ..r }
}

/// `r` with its dragged `edges` snapped, never inverting it.
fn snap_resized(r: Rect, edges: Edges, to: &snap::Edges, th: i32) -> Rect {
    let (mut x0, mut y0, mut x1, mut y1) = (r.x, r.y, r.right(), r.bottom());
    let near = |v: i32, c: &[i32]| snap::snap_edge(v, c, th);
    if edges.l {
        x0 = near(x0, &to.xs).min(x1 - 1);
    }
    if edges.r {
        x1 = near(x1, &to.xs).max(x0 + 1);
    }
    if edges.t {
        y0 = near(y0, &to.ys).min(y1 - 1);
    }
    if edges.b {
        y1 = near(y1, &to.ys).max(y0 + 1);
    }
    Rect::from_points(x0, y0, x1, y1)
}

fn tool_key(vk: u32, index: usize, letter: u8) -> bool {
    const VK_0: u32 = 0x30;
    const VK_NUMPAD0: u32 = 0x60;
    let digit = index as u32 + 1;
    vk == letter as u32 || vk == VK_0 + digit || vk == VK_NUMPAD0 + digit
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::gfx::Painter;

    fn editor() -> Editor {
        Editor::new(Rect { x: 0, y: 0, w: 400, h: 300 }, None, vec![Rect { x: 0, y: 0, w: 1920, h: 1080 }], 1.0)
    }

    fn click(ed: &mut Editor, p: (i32, i32)) {
        ed.mouse_down(p);
        ed.mouse_up(p);
        ed.last_click = None;
    }

    fn type_text(ed: &mut Editor, text: &str) {
        for u in text.encode_utf16() {
            ed.char(u);
        }
    }

    fn texts(ed: &Editor) -> Vec<String> {
        ed.marks
            .iter()
            .filter_map(|m| match m {
                Mark::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn steps_stay_consecutive() {
        let mut ed = editor();
        ed.activate(Item::Tool(Tool::Step));
        for x in [50, 150, 250] {
            click(&mut ed, (x, 100));
        }
        let numbers = |ed: &Editor| -> Vec<(i32, usize)> {
            marks::numbered(ed.marks.iter()).map(|(m, n)| (m.bbox().x + m.bbox().w / 2, n)).collect()
        };
        assert_eq!(numbers(&ed), [(50, 1), (150, 2), (250, 3)]);
        ed.picked = Some(1);
        ed.delete();
        assert_eq!(numbers(&ed), [(50, 1), (250, 2)]);
        ed.undo();
        assert_eq!(numbers(&ed), [(50, 1), (150, 2), (250, 3)]);
        ed.mouse_down((150, 100));
        ed.mouse_move((150, 200));
        ed.mouse_up((150, 200));
        assert!(matches!(ed.marks[1], Mark::Step { at: (150.0, 200.0), tail: None, .. }), "a step moves");
        ed.mouse_down((350, 100));
        ed.mouse_move((350, 200));
        ed.mouse_up((350, 200));
        assert!(matches!(ed.marks[3], Mark::Step { at: (350.0, 100.0), tail: Some((350.0, 200.0)), .. }));
        assert_eq!(numbers(&ed).last(), Some(&(350, 4)));
    }

    #[test]
    fn text_is_typed_committed_and_undone() {
        let mut ed = editor();
        assert!(matches!(ed.key(0x54, false, false), Event::Repaint), "T picks the text tool");
        click(&mut ed, (100, 100));
        type_text(&mut ed, "hi 7\rN😀");
        assert!(matches!(ed.key(0x37, false, false), Event::None), "digits don't switch tools while typing");
        ed.key(0x08, false, false);
        ed.key(0x25, false, false);
        type_text(&mut ed, "!");
        assert_eq!(texts(&ed), ["hi 7\n!N"]);
        assert!(ed.caret_rect().is_some());
        assert!(matches!(ed.key(0x1B, false, false), Event::Repaint), "Esc commits, not cancels");
        assert!(ed.typing.is_none() && ed.caret_rect().is_none());
        assert_eq!(ed.history.len(), 1);
        ed.undo();
        assert!(texts(&ed).is_empty());
        ed.redo();
        assert_eq!(texts(&ed), ["hi 7\n!N"]);

        click(&mut ed, (300, 250));
        click(&mut ed, (20, 250));
        assert_eq!(texts(&ed).len(), 1, "an empty text goes away when the click commits it");
        assert_eq!(ed.history.len(), 1);
    }

    #[test]
    fn double_click_retypes_a_text() {
        let mut ed = editor();
        ed.marks.push(Mark::new(Tool::Text, (100, 100), 0, 24.0));
        let mut m = ed.marks[0].clone();
        if let Mark::Text { text, .. } = &mut m {
            *text = "abc".into();
        }
        ed.refit(&mut m);
        ed.marks[0] = m;
        let inside = (ed.marks[0].bbox().right() - 1, ed.marks[0].bbox().y + 5);
        ed.mouse_down(inside);
        ed.mouse_up(inside);
        assert!(ed.typing.is_none(), "a single click picks it");
        ed.mouse_down(inside);
        ed.mouse_up(inside);
        assert!(ed.typing.is_some());
        type_text(&mut ed, "d");
        assert_eq!(texts(&ed), ["abcd"]);
        assert!(matches!(ed.key(0x1B, false, false), Event::Repaint));
        ed.undo();
        assert_eq!(texts(&ed), ["abc"]);
    }

    #[test]
    fn shift_digits_pick_colours() {
        let mut ed = editor();
        ed.key(0x33, false, true);
        assert_eq!((ed.color, ed.tool), (2, None));
        ed.key(0x33, false, false);
        assert_eq!((ed.color, ed.tool), (2, Some(Tool::Arrow)));
        ed.key(0x38, false, false);
        assert_eq!(ed.tool, Some(Tool::Step));
    }

    #[test]
    fn canvas_mode_shoots_the_monitor_under_the_mouse() {
        let monitors = vec![
            Rect { x: 1920, y: 1440, w: 3440, h: 1440 },
            Rect { x: 0, y: 1980, w: 1920, h: 1080 },
            Rect { x: 2285, y: 0, w: 2560, h: 1440 },
        ];
        let screen = Rect { x: 0, y: 0, w: 5360, h: 3060 };
        let mut ed = Editor::canvas(screen, monitors.clone(), 1.0, (100, 2500));
        assert_eq!(ed.output_rect(), monitors[1]);
        assert_eq!(ed.bar.y, monitors[1].y + 10, "the bar sits at the top of that monitor");
        assert!(matches!(ed.key(0x51, false, false), Event::PickRegion));
        ed.mouse_move((3000, 700));
        assert_eq!(ed.output_rect(), monitors[2]);
        ed.mouse_move((1910, 1300));
        assert_eq!(ed.output_rect(), monitors[0], "in a gap, the nearest monitor");
        let sel = Rect { x: 2000, y: 1500, w: 300, h: 200 };
        ed.set_selection(sel, None);
        assert_eq!(ed.output_rect(), sel);
        assert!(ed.has_selection() && matches!(ed.key(0x51, false, false), Event::PickRegion));
        assert!(matches!(editor().key(0x51, false, false), Event::None), "no region tool select-first");
    }

    #[test]
    fn selection_snaps_to_nearby_edges() {
        let mut ed = editor();
        ed.set_snap(vec![Rect { x: 500, y: 0, w: 300, h: 400 }]);
        ed.mouse_down((200, 150));
        ed.mouse_move((295, 155));
        assert_eq!(ed.sel, Rect { x: 100, y: 0, w: 400, h: 300 }, "the right edge snaps to x 500");
        ed.mouse_up((295, 155));
        ed.mouse_down((500, 150));
        ed.mouse_move((793, 150));
        assert_eq!(ed.sel.right(), 800);
        ed.mouse_move((780, 150));
        assert_eq!(ed.sel.right(), 780);
    }

    #[test]
    fn dragging_a_mark_repaints_only_around_it() {
        let screen = Rect { x: 0, y: 0, w: 1920, h: 1080 };
        let mut ed = Editor::new(screen, None, vec![screen], 1.0);
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

    #[test]
    fn patched_view_matches_a_full_render() {
        let painter = Painter::new().unwrap();
        let font = painter.font(13.0).unwrap();
        let screen = Rect { x: 0, y: 0, w: 640, h: 480 };
        let mut bright = Dib::new(screen.w, screen.h).unwrap();
        for (i, p) in bright.pixels_mut().iter_mut().enumerate() {
            *p = (i as u32).wrapping_mul(2_654_435_761) >> 8;
        }
        let cut = vec![Rect { x: 50, y: 40, w: 300, h: 200 }, Rect { x: 320, y: 220, w: 250, h: 200 }];
        let mut ed = Editor::new(Rect { x: 20, y: 20, w: 600, h: 440 }, Some(cut), vec![screen], 1.0);
        ed.marks.push(Mark::Blur { rect: Rect { x: 100, y: 100, w: 200, h: 150 }, radius: 6.0 });
        for (tool, path) in [
            (Tool::Pen, [(60, 60), (200, 180), (400, 300), (500, 420)]),
            (Tool::Step, [(300, 300), (310, 310), (380, 260), (420, 200)]),
            (Tool::Rectangle, [(90, 90), (150, 120), (330, 250), (260, 400)]),
        ] {
            ed.tool = Some(tool);
            ed.mouse_down(path[0]);
            ed.render(&bright, &painter, &font).unwrap();
            for &q in &path[1..] {
                ed.mouse_move(q);
                ed.render(&bright, &painter, &font).unwrap();
            }
            ed.mouse_up(path[3]);
            ed.render(&bright, &painter, &font).unwrap();
        }
        ed.tool = None;
        ed.mouse_down((200, 180));
        ed.mouse_move((240, 150));
        ed.render(&bright, &painter, &font).unwrap();
        ed.mouse_up((240, 150));
        ed.render(&bright, &painter, &font).unwrap();
        assert_eq!(ed.marks.len(), 4);
        let patched = ed.view.as_ref().unwrap().pixels().to_vec();
        ed.render_view(&bright, &painter, None).unwrap();
        assert!(patched == ed.view.as_ref().unwrap().pixels(), "patches leave the view as a full render would");
    }
}
