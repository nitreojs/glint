//! Screenshot markup on the frozen picker. Once a region is picked, a toolbar appears next to
//! it: pen, highlighter, arrow, rectangle, blur, pixelate, smart eraser, text, step markers,
//! cursors, grabs (a piece of the frozen screen, from anywhere on it, as a picture in the
//! shot) and the clipboard (pictures pasted with Ctrl+V or picked from a strip of recent ones),
//! five colours, three sizes, undo and redo, and copy / save / upload / pin / done / cancel. The
//! selection keeps handles for resizing (Ctrl moves it instead) and can be dragged while no tool
//! is active. In canvas mode ([`Editor::canvas`]) there is no selection until one is picked: the
//! toolbar is docked at the top of the monitor under the mouse (Tab brings it to
//! the mouse), the whole screen is drawable, and with no drawing tool the region tool is on: the
//! picker picks, and the picked region is the finished shot (or, with Shift, opens in here).
//!
//! Marks live in screen (client) coordinates, so resizing or moving the selection reframes
//! the shot instead of dragging the marks along. Blur and pixelate hide what's on screen, so
//! they always sit under the drawn shapes. Area tools (rectangle, blur, pixelate, eraser)
//! highlight the window, client area or UI element under the mouse; a click covers it.
//! Right-click deletes a mark, or cancels the drag in progress; in canvas mode off the marks,
//! with nothing picked yet, it cancels the shot.
//!
//! Texts are objects: a click places one (or, on an existing one, types into it), a drag moves
//! it, its corners scale it and its sides set the width it wraps at. While typing, it edits like a
//! text box (selection, Ctrl+A / C / X / V, word jumps, its own undo). The text bar beside the
//! toolbar sets the font, size, bold / italic / underline / strikethrough, alignment and backdrop
//! of the whole text; new texts start in the last style used. Typing repaints only the text.

use std::cell::Cell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glint_core::geom::rect::Rect;
use glint_core::geom::snap;
use glint_media::rect::WinRect;
use windows::Win32::Foundation::RECT;
use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use windows::Win32::UI::WindowsAndMessaging::{
    IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_IBEAM, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE,
};
use windows::core::PCWSTR;

use crate::platform::cursor::CursorImage;
use glint_shell::gfx::Dib;
use crate::platform::rect::ring;

#[cfg(test)]
mod checks;
mod effects;
mod marks;
mod paste;
mod render;
mod text;
mod textbar;
mod toolbar;
mod typing;

use marks::{COLORS, Edges, Grip, MIN_SIDE, Mark, Sprite, Strength, TOOLS, Tool, dist, resize, shift_within};
use paste::{Clip, Load};
pub(crate) use paste::{Recent, load_recents};
use text::{Style, Typesetter};
use textbar::{FRAME, FontPicker, Panel};
use toolbar::{Item, nearest};
use typing::Typing;

const NOTICE: Duration = Duration::from_millis(1200);
const WHEEL_GROUP: Duration = Duration::from_secs(1);
const WHEEL_DELTA: i32 = 120;
const VK_TAB: u32 = 0x09;
const SUMMON_REACH: f32 = 120.0;

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
    /// A mark being drawn; `anchor` is where it grows from, `press` where the button went down.
    /// `click` is the rect a release without a drag covers.
    Mark { mark: Mark, anchor: (i32, i32), press: (i32, i32), click: Option<Rect> },
    /// A mark moved (no grip) or resized; `retype`: a release without a move types into it;
    /// `fresh`: it was just pasted, and that undo step covers the move too.
    Edit { index: usize, grip: Option<Grip>, grab: (i32, i32), from: Mark, retype: bool, fresh: bool },
    Move { grab: (i32, i32), from: Rect },
    /// `from` and `grab` move along while Ctrl moves the selection; `start` is the selection before.
    Resize { edges: Edges, grab: (i32, i32), from: Rect, start: Rect },
    /// A toolbar button is held; it fires on release over the same button.
    Bar(Item),
    /// Selecting text in the text being typed.
    Select,
    /// A picture in the clipboard strip is held: a click pastes it, a drag pulls it out.
    Recent { index: usize, press: (i32, i32) },
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
    panel: Option<Panel>,
    /// How far the running animations have come, in steps (255 once none runs).
    anim: u8,
    style: (Style, u32),
    fonts: Option<(String, usize)>,
    recents: (usize, Load),
}

pub(crate) struct Frame {
    canvas: Rect,
    picked: Option<(usize, Rect)>,
    caret: Vec<Rect>,
    pick: Option<Rect>,
    ink: Vec<Rect>,
    rev: u64,
    marks: u64,
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
    /// Canvas mode: the toolbar has the region tool, which is on while no drawing tool is.
    region: bool,
    /// Last mouse position; with no selection, the shot is the monitor under it.
    mouse: (i32, i32),
    monitors: Vec<Rect>,
    /// Edges the selection snaps to while it's moved or resized (Alt held: no snapping).
    snap: snap::Edges,
    /// Ctrl, Alt and Shift are held, as of the last mouse or key message.
    ctrl: bool,
    alt: bool,
    shift: bool,
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
    /// What new texts look like: the last style used.
    text_style: Style,
    /// The font list, while it's open.
    fonts_open: Option<FontPicker>,
    /// Where Ctrl+V pastes from.
    clip: Box<dyn Clip>,
    /// The clipboard tool's strip is open.
    strip_open: bool,
    /// The strip's pictures, with when each arrived (they fade in).
    recents: Vec<(Recent, Instant)>,
    recents_load: Load,
    /// The panel showing and when it appeared (it slides out).
    panel_since: Cell<Option<(Panel, Instant)>>,
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
    /// Bumped by every change to the marks, however little of the view it repaints.
    marks_rev: u64,
    back: Option<Dib>,
    /// Toolbar cells (client coordinates).
    cells: Vec<(Item, Rect)>,
    pub bar: Rect,
    pill: Option<(Rect, Item, String)>,
    /// Canvas mode: the toolbar is at the mouse instead of docked at the top of its monitor.
    summoned: bool,
    windows: Vec<(Rect, Option<Rect>)>,
    element: Option<Rect>,
    pick: Option<Rect>,
    /// The picture the cursor tool places.
    arrow: Option<Sprite>,
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
        let (text_style, text_size) = text::last_style().unwrap_or((Style::default(), 24.0));
        let mut strength = TOOLS.map(|(t, ..)| t.strength().map_or(0.0, |s| s.presets[1]));
        strength[Tool::Text as usize] = text_size;
        let mut ed = Editor {
            sel,
            cutout,
            selected,
            region: !selected,
            mouse,
            monitors,
            snap: snap::Edges::default(),
            ctrl: false,
            alt: false,
            shift: false,
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
            strength,
            drag: None,
            typing: None,
            surrogate: None,
            last_click: None,
            fonts: Typesetter::new(),
            text_style,
            fonts_open: None,
            clip: paste::default_clip(),
            strip_open: false,
            recents: Vec::new(),
            recents_load: Load::Idle,
            panel_since: Cell::new(None),
            hover: None,
            notice: None,
            scale,
            base: None,
            view: None,
            view_dirty: true,
            patch: None,
            rev: 0,
            marks_rev: 0,
            back: None,
            cells: Vec::new(),
            bar: Rect::default(),
            pill: None,
            summoned: false,
            windows: Vec::new(),
            element: None,
            pick: None,
            arrow: None,
        };
        ed.place_bar();
        ed
    }

    /// Canvas mode picked a region: from now on it behaves like select-first, keeping the marks.
    fn set_selection(&mut self, sel: Rect, cutout: Option<Vec<Rect>>) {
        self.commit();
        self.sel = sel;
        self.cutout = cutout;
        self.selected = true;
        self.summoned = false;
        self.pick = None;
        self.base = None;
        self.place_bar();
        self.touch();
    }

    /// Canvas mode with no drawing tool: the region tool is on, and the picker picks.
    pub(crate) fn picks_region(&self) -> bool {
        self.region && !self.selected && self.tool.is_none()
    }

    /// The region tool picked `sel`: that's the shot, finished, unless `edit` opens it in here.
    pub(crate) fn region_picked(&mut self, sel: Rect, cutout: Option<Vec<Rect>>, edit: bool) -> Event {
        self.set_selection(sel, cutout);
        if edit { Event::Repaint } else { Event::Finish(Output::Default) }
    }

    /// What the selection snaps to (client coordinates).
    pub(crate) fn set_snap(&mut self, edges: snap::Edges) {
        self.snap = edges;
    }

    /// Ctrl moves the selection instead of resizing it; Alt stops snapping; Shift keeps a
    /// picture's proportions.
    pub(crate) fn set_mods(&mut self, ctrl: bool, alt: bool, shift: bool) {
        (self.ctrl, self.alt, self.shift) = (ctrl, alt, shift);
    }

    pub(crate) fn set_windows(&mut self, windows: Vec<(Rect, Option<Rect>)>) {
        self.windows = windows;
    }

    /// The picture the cursor tool places.
    pub(crate) fn set_arrow(&mut self, image: Arc<CursorImage>) {
        self.arrow = Some(Sprite(image));
    }

    /// The cursor as it was on screen, with its hotspot at `at` (client coordinates): a mark that
    /// can be moved, resized or deleted like the ones the cursor tool adds. Not an undo step.
    pub(crate) fn add_cursor(&mut self, image: Arc<CursorImage>, at: (i32, i32)) {
        let at = (at.0 as f32, at.1 as f32);
        self.marks.push(Mark::Cursor { at, sprite: Sprite(image), size: 100.0 });
        self.touch();
    }

    pub(crate) fn wants_elements(&self) -> bool {
        self.tool.is_some_and(Tool::picks_rects)
    }

    pub(crate) fn set_element(&mut self, element: Option<Rect>) {
        self.element = element;
    }

    pub(crate) fn refresh_pick(&mut self) -> Event {
        if self.drag.is_some() {
            return Event::None;
        }
        let pick = self.hover_pick(self.mouse);
        if pick == self.pick {
            return Event::None;
        }
        self.pick = pick;
        Event::Repaint
    }

    /// The marks changed since `before` was taken: anything drawn from them is stale.
    pub(crate) fn marks_changed(&self, before: &Frame) -> bool {
        self.marks_rev != before.marks
    }

    pub(crate) fn has_marks(&self) -> bool {
        !self.marks.is_empty()
    }

    /// A press is in progress (in canvas mode's region tool, only ever on the toolbar).
    pub(crate) fn dragging(&self) -> bool {
        self.drag.is_some()
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
        // a grab's source can be anywhere on the screen
        v.extend(self.pick);
        v.extend(self.chrome());
        v
    }

    fn drawn(&self) -> Rect {
        self.sel.inflate(self.px(8.0) as i32)
    }

    /// The toolbar, its panels and the pill, over everything else.
    pub(crate) fn chrome(&self) -> Vec<Rect> {
        let mut v = vec![self.bar];
        v.extend(self.panel_rects());
        v.extend(self.pill.as_ref().map(|p| p.0));
        v
    }

    /// How far the slowest running animation has come (0 to 1), if one is running.
    fn animation(&self) -> Option<f32> {
        let mut times = Vec::new();
        if self.panel().is_some() {
            times.push(self.panel_t());
        }
        times.extend(self.fonts_open.as_ref().map(|f| textbar::progress(f.since, textbar::PANEL_MS)));
        if self.panel() == Some(Panel::Strip) {
            times.extend(self.recents.iter().map(|(_, at)| textbar::progress(*at, textbar::PANEL_MS)));
        }
        times.into_iter().filter(|t| *t < 1.0).reduce(f32::min)
    }

    /// What the dragged mark and the text being typed can repaint; editing an effect reorders the
    /// whole effect stack.
    fn ink(&self) -> Vec<Rect> {
        let mut v: Vec<Rect> = self.typing.iter().map(|t| self.marks[t.index].ink(self.scale)).collect();
        let (m, edit) = match &self.drag {
            Some(Drag::Mark { mark, .. }) => (mark, false),
            Some(Drag::Edit { index, .. }) => (&self.marks[*index], true),
            _ => return v,
        };
        v.push(m.ink(self.scale));
        if edit && m.is_effect() {
            v.extend(self.marks.iter().filter(|m| m.is_effect()).map(Mark::bbox));
        }
        v
    }

    fn touch(&mut self) {
        self.view_dirty = true;
        self.rev += 1;
        self.marks_rev += 1;
    }

    /// The caret and the selected text's boxes, while typing.
    fn overlay(&self) -> Vec<Rect> {
        let mut v = self.selection_rects();
        v.extend(self.caret_rect());
        v
    }

    pub(crate) fn frame(&self) -> Frame {
        Frame {
            canvas: self.drawn(),
            picked: self.picked.zip(self.picked_extent()),
            caret: self.overlay(),
            pick: self.pick,
            ink: self.ink(),
            rev: self.rev,
            marks: self.marks_rev,
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
            v.extend(self.patch);
        }
        if now.picked != before.picked {
            v.extend(before.picked.map(|(_, r)| r));
            v.extend(now.picked.map(|(_, r)| r));
        }
        if now.caret != before.caret {
            v.extend(&before.caret);
            v.extend(&now.caret);
        }
        if now.pick != before.pick {
            for r in before.pick.into_iter().chain(now.pick) {
                v.extend(pick_ring(r).iter().map(Rect::from_win));
            }
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
            size: self.current().map(|(t, _, v)| (t, v)),
            undo: !self.history.is_empty(),
            redo: !self.future.is_empty(),
            pill: self.pill_text(),
            panel: self.panel(),
            anim: self.animation().map_or(255, |t| (t * 64.0) as u8),
            style: {
                let (style, size) = self.shown_style();
                (style, size.to_bits())
            },
            fonts: self.fonts_open.as_ref().map(|f| (f.query.clone(), f.scroll)),
            recents: (self.recents.len(), self.recents_load),
        }
    }

    fn pressed(&self) -> Option<Item> {
        match self.drag {
            Some(Drag::Bar(item)) => Some(item),
            Some(Drag::Recent { index, .. }) => Some(Item::Recent(index)),
            _ => None,
        }
    }

    fn picked_mark(&self) -> Option<&Mark> {
        self.picked.map(|i| &self.marks[i])
    }

    fn shown_color(&self) -> usize {
        self.picked_mark().and_then(Mark::color).unwrap_or(self.color)
    }

    fn current(&self) -> Option<(Tool, Strength, f32)> {
        let (tool, v) = match self.picked_mark() {
            Some(m) => (m.tool(), m.strength()?),
            None => self.tool.map(|t| (t, self.strength[t as usize]))?,
        };
        tool.strength().map(|s| (tool, s, v))
    }

    fn tip(&self, item: Item) -> Option<String> {
        match (item, self.current()) {
            (Item::Size(n), Some((_, s, _))) => Some(format!("{}  ·  {}", s.name, s.presets[n])),
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
            Mark::Stroke { width, .. } => m.bbox().inflate((width * self.scale / 2.0 + self.px(4.0)).ceil() as i32),
            _ => m.bbox().inflate(self.px(4.0) as i32),
        }
    }

    fn picked_extent(&self) -> Option<Rect> {
        let m = self.picked_mark()?;
        Some(match m {
            Mark::Stroke { .. } => self.outline(m).inflate(2),
            Mark::Text { .. } => self.outline(m).inflate(self.px(8.0) as i32 + 1),
            _ => m.bbox().inflate(self.px(8.0) as i32 + 1),
        })
    }

    /// A mark's grips; a text's sit on its dashed outline, clear of the letters.
    fn grips_of(&self, m: &Mark) -> Vec<(Grip, (f32, f32))> {
        match m {
            Mark::Text { .. } => {
                let o = self.outline(m);
                marks::TEXT_GRIPS.iter().map(|&e| (Grip::Edge(e), marks::edge_point(o, e))).collect()
            }
            _ => m.grips(),
        }
    }

    fn hit(&self, p: (i32, i32)) -> Hit {
        if let Some((item, _)) = self.all_cells().into_iter().find(|(_, r)| r.contains(p.0, p.1)) {
            return Hit::Bar(item);
        }
        if self.bar.contains(p.0, p.1) || self.panel_rects().iter().any(|r| r.contains(p.0, p.1)) {
            return Hit::Outside; // the padding of the bar and its panels
        }
        let at = (p.0 as f32, p.1 as f32);
        let reach = self.px(8.0);
        let mut grips = self.picked_mark().into_iter().flat_map(|m| self.grips_of(m));
        if let Some((grip, _)) = grips.find(|(_, q)| dist(at, *q) <= reach) {
            return Hit::Grip(grip);
        }
        if let Some(t) = &self.typing {
            // the letters take the caret, the rim around them moves the text
            let m = &self.marks[t.index];
            if m.bbox().contains(p.0, p.1) {
                return Hit::Typing;
            }
            if self.outline(m).contains(p.0, p.1) {
                return Hit::Mark(t.index);
            }
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
            // a grab can start anywhere on the frozen screen
            let grabs = self.tool == Some(Tool::Grab) && self.area.contains(p.0, p.1);
            return if grabs { Hit::Inside } else { Hit::Outside };
        }
        match self.mark_at(at, false) {
            Some(i) => Hit::Mark(i),
            None => Hit::Inside,
        }
    }

    /// The mark at `at`, the picked one first, then shapes over effects; `any` ignores the tool,
    /// which otherwise reaches only its own kind.
    fn mark_at(&self, at: (f32, f32), any: bool) -> Option<usize> {
        let n = self.marks.len();
        let shapes = (0..n).rev().filter(|&i| !self.marks[i].is_effect());
        let effects = (0..n).rev().filter(|&i| self.marks[i].is_effect());
        self.picked
            .into_iter()
            .chain(shapes)
            .chain(effects)
            .filter(|&i| any || self.tool.is_none_or(|t| t == self.marks[i].tool()))
            .find(|&i| self.marks[i].hit(at, self.scale))
    }

    pub(crate) fn cursor(&self, p: (i32, i32)) -> PCWSTR {
        match &self.drag {
            Some(Drag::Mark { .. }) => return IDC_CROSS,
            Some(Drag::Edit { grip, .. }) => return grip_cursor(*grip),
            Some(Drag::Select) => return IDC_IBEAM,
            _ => {}
        }
        match self.hit(p) {
            Hit::Bar(_) => IDC_HAND,
            Hit::Grip(grip) => grip_cursor(Some(grip)),
            Hit::Handle(e) => edge_cursor(e),
            Hit::Mark(i) if self.picked == Some(i) => IDC_SIZEALL,
            Hit::Mark(i) if self.tool == Some(Tool::Text) && matches!(self.marks[i], Mark::Text { .. }) => IDC_IBEAM,
            Hit::Mark(_) => IDC_HAND,
            Hit::Typing => IDC_IBEAM,
            Hit::Inside => match self.tool {
                Some(Tool::Text) => IDC_IBEAM,
                Some(Tool::Image) => IDC_ARROW,
                Some(_) => IDC_CROSS,
                None if self.selected => IDC_SIZEALL,
                None => IDC_ARROW,
            },
            Hit::Outside => IDC_ARROW,
        }
    }

    fn grab(&self, index: usize, grip: Option<Grip>, p: (i32, i32), retype: bool) -> Drag {
        Drag::Edit { index, grip, grab: p, from: self.marks[index].clone(), retype, fresh: false }
    }

    fn pick_at(&self, p: (i32, i32)) -> Option<Rect> {
        let inside = |r: &Rect| r.contains(p.0, p.1);
        let window = || self.windows.iter().find(|(w, _)| inside(w)).map(|&(w, client)| client.filter(inside).unwrap_or(w));
        let r = self.element.filter(inside).or_else(window).unwrap_or_else(|| nearest(&self.monitors, p));
        let clip = if self.selected && self.tool != Some(Tool::Grab) { self.sel } else { self.area };
        r.intersect(&clip).filter(|r| r.w >= MIN_SIDE && r.h >= MIN_SIDE)
    }

    fn hover_pick(&self, p: (i32, i32)) -> Option<Rect> {
        let on = self.wants_elements() && matches!(self.hit(p), Hit::Inside);
        on.then(|| self.pick_at(p)).flatten()
    }

    /// Where a grab of `src` lands: where it was when that's inside the shot, else in the middle of
    /// the shot, shrunk to fit.
    fn place_grab(&self, src: Rect) -> Rect {
        let s = self.output_rect();
        if s.intersect(&src) == Some(src) {
            return src;
        }
        let k = (0.8 * s.w as f32 / src.w.max(1) as f32).min(0.8 * s.h as f32 / src.h.max(1) as f32).min(1.0);
        let side = |v: i32| ((v as f32 * k).round() as i32).max(MIN_SIDE);
        let (w, h) = (side(src.w), side(src.h));
        Rect { x: s.x + (s.w - w) / 2, y: s.y + (s.h - h) / 2, w, h }
    }

    fn snap_threshold(&self) -> i32 {
        if self.alt { -1 } else { snap::threshold(self.scale) }
    }

    /// `(x, y)` with each coordinate on a nearby edge that runs past it.
    fn snapped(&self, (x, y): (i32, i32), th: i32) -> (i32, i32) {
        (snap::snap_edge(x, (y, y), &self.snap.xs, th), snap::snap_edge(y, (x, x), &self.snap.ys, th))
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

    pub(crate) fn mouse_down(&mut self, p: (i32, i32), under: u32) -> Event {
        self.mouse = p;
        let hit = self.hit(p);
        let double = self.double_click(p);
        let typing = self.typing.as_ref().map(|t| t.index);
        match hit {
            Hit::Typing if double => return self.select_word(p),
            Hit::Typing => {
                self.place_caret(p, self.shift);
                self.drag = Some(Drag::Select);
                return Event::Repaint;
            }
            Hit::Bar(_) => {}
            // the text being typed moves and resizes without ending the typing
            Hit::Grip(_) if typing.is_some() => {}
            Hit::Mark(i) if Some(i) == typing => {}
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
            Hit::Bar(Item::Recent(index)) => Some(Drag::Recent { index, press: p }),
            Hit::Bar(item) => Some(Drag::Bar(item)),
            Hit::Grip(grip) => self.picked.map(|index| self.grab(index, Some(grip), p, false)),
            Hit::Mark(index) => {
                // a click on a text types into it: with the text tool, or once it's picked
                let text = matches!(self.marks[index], Mark::Text { .. }) && Some(index) != typing;
                let retype = text && (self.tool == Some(Tool::Text) || picked == Some(index));
                Some(self.grab(index, None, p, retype))
            }
            Hit::Handle(edges) => Some(Drag::Resize { edges, grab: p, from: self.sel, start: self.sel }),
            Hit::Inside => match self.tool {
                None if self.selected => Some(Drag::Move { grab: p, from: self.sel }),
                None | Some(Tool::Image) => None,
                Some(Tool::Text) => return self.start_typing(p),
                Some(tool) => {
                    let area = tool.picks_rects();
                    let anchor = if area { self.snapped(p, self.snap_threshold()) } else { p };
                    let mut mark = Mark::new(tool, anchor, self.color, self.strength[tool as usize]);
                    match &mut mark {
                        Mark::Eraser { fill, .. } => *fill = under & 0x00FF_FFFF,
                        Mark::Cursor { sprite, .. } => match &self.arrow {
                            Some(arrow) => *sprite = arrow.clone(),
                            None => return Event::None,
                        },
                        _ => {}
                    }
                    self.refit(&mut mark);
                    let click = if area { self.pick_at(p) } else { None };
                    Some(Drag::Mark { mark, anchor, press: p, click })
                }
            },
            Hit::Typing | Hit::Outside => None,
        };
        if let Some(Drag::Mark { .. }) = self.drag {
            self.repaint_ink(Vec::new());
        }
        let pick = match &self.drag {
            Some(Drag::Mark { click, .. }) => *click,
            _ => None,
        };
        let changed = std::mem::replace(&mut self.pick, pick) != pick || self.picked != picked;
        if self.drag.is_some() || changed { Event::Repaint } else { Event::None }
    }

    pub(crate) fn mouse_move(&mut self, p: (i32, i32)) -> Event {
        let last = std::mem::replace(&mut self.mouse, p);
        let b = self.area;
        let slop = self.px(4.0);
        let th = self.snap_threshold();
        let before = self.ink();
        let snap = &self.snap;
        match &mut self.drag {
            Some(Drag::Mark { mark, anchor, press, click }) => {
                let q = (p.0.clamp(b.x, b.right()), p.1.clamp(b.y, b.bottom()));
                let f = (q.0 as f32, q.1 as f32);
                if (p.0 - press.0).abs() as f32 > slop || (p.1 - press.1).abs() as f32 > slop {
                    *click = None;
                }
                let clicking = click.is_some();
                match mark {
                    Mark::Stroke { points, .. } => {
                        if points.last().is_some_and(|l| (l.0 - f.0).abs() + (l.1 - f.1).abs() >= 1.0) {
                            points.push(f);
                        }
                    }
                    Mark::Arrow { to, .. } => *to = f,
                    Mark::Rectangle { rect, .. }
                    | Mark::Blur { rect, .. }
                    | Mark::Pixelate { rect, .. }
                    | Mark::Eraser { rect, .. }
                    | Mark::Grab { src: rect, .. } => {
                        if !clicking {
                            let x = snap::snap_edge(q.0, (q.1, q.1), &snap.xs, th);
                            let y = snap::snap_edge(q.1, (q.0, q.0), &snap.ys, th);
                            *rect = Rect::from_points(anchor.0, anchor.1, x, y);
                        }
                    }
                    Mark::Step { at, tail, r, .. } => *tail = (dist(*at, f) > *r + slop).then_some(f),
                    Mark::Cursor { at, .. } => *at = f,
                    Mark::Text { .. } | Mark::Image { .. } => {}
                }
                // a grab's source shows as the pick outline while it's dragged
                let source = match mark {
                    Mark::Grab { src, .. } => Some(*src),
                    _ => None,
                };
                if !clicking {
                    self.pick = source;
                }
                self.repaint_ink(before);
                Event::Repaint
            }
            Some(Drag::Edit { index, grip, grab, from, .. }) => {
                let d = (p.0 - grab.0, p.1 - grab.1);
                let (from, grip, i) = (from.clone(), *grip, *index);
                let mark = match grip {
                    Some(Grip::Edge(e)) if matches!(from, Mark::Text { .. }) => self.resize_text(&from, e, d),
                    Some(grip) => from.resized(grip, d, b, self.shift),
                    None => {
                        let (dx, dy) = shift_within(from.bbox(), d, b);
                        from.shifted(dx, dy)
                    }
                };
                if self.marks[i] == mark {
                    return Event::None;
                }
                self.marks[i] = mark;
                self.repaint_ink(before);
                Event::Repaint
            }
            &mut Some(Drag::Move { grab, from }) => {
                let moved =
                    snap_moved(Rect { x: from.x + p.0 - grab.0, y: from.y + p.1 - grab.1, ..from }, snap, th);
                let x = moved.x.clamp(b.x, (b.right() - from.w).max(b.x));
                let y = moved.y.clamp(b.y, (b.bottom() - from.h).max(b.y));
                self.set_sel(Rect { x, y, ..from })
            }
            &mut Some(Drag::Resize { edges, start, .. }) if self.ctrl => {
                let (dx, dy) = shift_within(self.sel, (p.0 - last.0, p.1 - last.1), b);
                let moved = self.sel.offset(dx, dy);
                // resizing picks up from here once Ctrl is let go
                self.drag = Some(Drag::Resize { edges, grab: p, from: moved, start });
                self.set_sel(moved)
            }
            &mut Some(Drag::Resize { edges, grab, from, .. }) => {
                let resized = resize(from, edges, (p.0 - grab.0, p.1 - grab.1), b, 1);
                self.set_sel(snap_resized(resized, edges, p, snap, th))
            }
            Some(Drag::Bar(_)) => Event::None,
            &mut Some(Drag::Recent { index, press }) => self.drag_recent(index, press, p),
            Some(Drag::Select) => self.place_caret(p, true),
            None => {
                let docked = !self.selected && !self.summoned;
                let away = self.summoned && far(self.bar, p, self.px(SUMMON_REACH));
                let redock = away || (docked && nearest(&self.monitors, p) != self.bar_monitor);
                if redock {
                    self.summoned = false;
                    self.place_bar();
                }
                let hover = match self.hit(p) {
                    Hit::Bar(item) => Some(item),
                    _ => None,
                };
                let pick = self.hover_pick(p);
                if !redock && hover == self.hover && pick == self.pick {
                    return Event::None;
                }
                self.hover = hover;
                self.pick = pick;
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
            Some(Drag::Recent { index, .. }) => match self.hit(p) {
                Hit::Bar(Item::Recent(at)) if at == index => self.paste_recent(index, None),
                _ => Event::Repaint,
            },
            Some(Drag::Mark { mut mark, click, .. }) => {
                let drawn = mark.ink(self.scale);
                if let Mark::Grab { src, rect } = &mut mark {
                    *src = click.unwrap_or(*src);
                    *rect = self.place_grab(*src);
                } else if let (Some(r), Some(area)) = (click, mark.area_mut()) {
                    *area = r;
                }
                self.repaint_ink(vec![drawn]);
                self.settled(&mark);
                if mark.worth_keeping() {
                    self.remember();
                    self.marks.push(mark);
                    self.picked = Some(self.marks.len() - 1);
                    self.marks_rev += 1;
                }
                self.pick = self.hover_pick(p);
                Event::Repaint
            }
            Some(Drag::Edit { index, from, retype, fresh, .. }) => {
                self.settled(&self.marks[index].clone());
                if self.marks[index] == from {
                    return if retype { self.retype(index, p) } else { Event::Repaint };
                }
                self.marks_rev += 1;
                if fresh {
                    // pasting it already made the undo step
                } else if self.typing_into(index) {
                    self.typing_checkpoint(index, from);
                } else {
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
        match self.picked {
            Some(i) => self.delete_at(i),
            None => Event::None,
        }
    }

    fn delete_at(&mut self, i: usize) -> Event {
        self.remember();
        self.marks.remove(i);
        self.picked = self.picked.filter(|&p| p != i).map(|p| if p > i { p - 1 } else { p });
        self.touch();
        self.pick = self.hover_pick(self.mouse);
        Event::Repaint
    }

    fn escape(&mut self) -> Event {
        if self.fonts_open.take().is_some() {
            return Event::Repaint;
        }
        if self.panel() == Some(Panel::Strip) && self.picked.is_none() {
            self.strip_open = false;
            return Event::Repaint;
        }
        if self.summoned && !self.selected {
            return self.dismiss_bar();
        }
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
        if let Item::Region | Item::Tool(_) | Item::Color(_) | Item::Size(_) = item
            && self.summoned
        {
            self.summoned = false;
            self.place_bar();
        }
        match item {
            Item::Region => {
                self.commit();
                self.tool = None;
                self.picked = None;
                self.pick = None;
                if self.selected {
                    // back to the whole screen, marks and all, to pick again
                    self.selected = false;
                    self.sel = self.area;
                    self.cutout = None;
                    self.base = None;
                    self.touch();
                }
                self.place_bar();
            }
            Item::Tool(Tool::Image) => {
                self.commit();
                self.clipboard_tool();
                if self.picked_mark().is_some_and(|m| self.tool.is_some_and(|t| m.tool() != t)) {
                    self.picked = None;
                }
                self.pick = self.hover_pick(self.mouse);
            }
            Item::Tool(t) => {
                self.commit();
                self.tool = if self.tool == Some(t) { None } else { Some(t) };
                self.strip_open = false;
                self.fonts_open = None;
                if self.tool.is_some_and(|t| self.picked_mark().is_some_and(|m| m.tool() != t)) {
                    self.picked = None;
                }
                self.pick = self.hover_pick(self.mouse);
            }
            Item::Color(c) => {
                self.color = c;
                if let Some(i) = self.picked
                    && self.marks[i].color().is_some_and(|old| old != c)
                {
                    let was = self.marks[i].clone();
                    if self.typing_into(i) {
                        self.typing_checkpoint(i, was);
                    } else {
                        self.remember();
                    }
                    if let Some(slot) = self.marks[i].color_mut() {
                        *slot = c;
                    }
                    self.touch();
                }
            }
            Item::Size(n) => {
                if let Some((_, s, _)) = self.current() {
                    return self.set_strength(s.presets[n], false);
                }
            }
            Item::Undo => return self.undo(),
            Item::Redo => return self.redo(),
            Item::Copy => return Event::Finish(Output::Copy),
            Item::Save => return Event::Finish(Output::Save),
            Item::Upload => return Event::Finish(Output::Upload),
            Item::Pin => return Event::Finish(Output::Pin),
            Item::Done => return Event::Finish(Output::Default),
            Item::Close => return Event::Cancel,
            Item::Recent(i) => return self.paste_recent(i, None),
            Item::Font
            | Item::FontRow(_)
            | Item::Smaller
            | Item::Bigger
            | Item::Bold
            | Item::Italic
            | Item::Underline
            | Item::Strike
            | Item::Align
            | Item::Back => return self.activate_text(item),
        }
        Event::Repaint
    }

    fn set_strength(&mut self, v: f32, wheel: bool) -> Event {
        let Some((tool, spec, _)) = self.current() else { return Event::None };
        let v = v.clamp(spec.min, spec.max);
        self.strength[tool as usize] = v;
        if let Some(i) = self.picked
            && self.marks[i].strength() != Some(v)
        {
            let grouped = wheel && self.wheel_group.is_some_and(|(g, at)| g == i && at.elapsed() < WHEEL_GROUP);
            if self.typing_into(i) {
                self.typing_checkpoint(i, self.marks[i].clone());
            } else if !grouped {
                self.remember();
            }
            let mut mark = self.marks[i].clone();
            if let Some(slot) = mark.strength_mut() {
                *slot = v;
            }
            self.refit(&mut mark);
            self.marks[i] = mark;
            if wheel {
                self.wheel_group = Some((i, Instant::now()));
            }
            self.touch();
        }
        if tool == Tool::Text {
            text::remember_style(&self.text_style, v);
        }
        if wheel {
            self.notice = Some((format!("{}  ·  {v}", spec.name), Instant::now()));
        }
        Event::Repaint
    }

    fn nudge(&mut self, steps: i32) -> Event {
        let Some((_, spec, v)) = self.current() else { return Event::None };
        self.set_strength(v + steps as f32 * spec.step, true)
    }

    fn summon(&mut self) -> Event {
        if self.selected || self.drag.is_some() {
            return Event::None;
        }
        self.summoned = true;
        self.hover = None;
        self.pick = None;
        self.place_bar();
        Event::Repaint
    }

    fn dismiss_bar(&mut self) -> Event {
        self.summoned = false;
        self.hover = None;
        self.pick = self.hover_pick(self.mouse);
        self.place_bar();
        Event::Repaint
    }

    /// Right-click: cancels the drag in progress, else deletes the mark under `p`, else in
    /// canvas mode cancels the shot.
    pub(crate) fn right_click(&mut self, p: (i32, i32)) -> Event {
        self.mouse = p;
        if let Some(drag) = self.drag.take() {
            return self.abandon(drag);
        }
        if let Some(i) = self.typing.as_ref().map(|t| t.index)
            && matches!(self.hit(p), Hit::Typing)
        {
            let n = self.marks.len();
            self.commit();
            // an empty text is gone already
            return if self.marks.len() < n { Event::Repaint } else { self.delete_at(i) };
        }
        let on_canvas = self.sel.contains(p.0, p.1) && !self.bar.contains(p.0, p.1);
        match self.mark_at((p.0 as f32, p.1 as f32), true).filter(|_| on_canvas) {
            Some(i) => self.delete_at(i),
            None if !self.selected => Event::Cancel,
            None => Event::None,
        }
    }

    /// Puts back what `drag` changed, as if the button never went down.
    fn abandon(&mut self, drag: Drag) -> Event {
        match drag {
            Drag::Mark { mark, .. } => self.repaint_ink(vec![mark.ink(self.scale)]),
            Drag::Edit { index, from, .. } => {
                self.marks[index] = from;
                self.touch();
            }
            Drag::Move { from: start, .. } | Drag::Resize { start, .. } => {
                self.set_sel(start);
            }
            Drag::Bar(_) | Drag::Select | Drag::Recent { .. } => {}
        }
        self.pick = self.hover_pick(self.mouse);
        Event::Repaint
    }

    pub(crate) fn key_up(&mut self, vk: u32) -> Event {
        if vk == VK_TAB && self.summoned && !self.selected && !self.bar.contains(self.mouse.0, self.mouse.1) {
            return self.dismiss_bar();
        }
        Event::None
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
        if self.fonts_open.is_some() && !ctrl {
            return self.font_key(vk);
        }
        if self.typing.is_some()
            && let Some(event) = self.typing_key(vk, ctrl, shift)
        {
            return event;
        }
        let texts = self.tool == Some(Tool::Text) || self.picked_mark().is_some_and(|m| m.tool() == Tool::Text);
        match (vk, ctrl) {
            (VK_TAB, false) => self.summon(),
            (VK_RETURN | VK_SPACE, _) => Event::Finish(Output::Default),
            (0x43, true) => Event::Finish(Output::Copy),   // C
            (0x53, true) => Event::Finish(Output::Save),   // S
            (0x55, true) if texts => self.restyle(|s| s.underline = !s.underline),
            (0x42, true) if texts => self.restyle(|s| s.bold = !s.bold), // B
            (0x49, true) if texts => self.restyle(|s| s.italic = !s.italic), // I
            (0x55, true) => Event::Finish(Output::Upload), // U
            (0x56, true) => self.paste(),                  // V
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
            (_, false) => match TOOLS.iter().find(|(_, _, letter, digit)| tool_key(vk, *digit, *letter)) {
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
        let on_fonts = self.font_list().is_some_and(|l| l.rect.contains(self.mouse.0, self.mouse.1));
        match notches {
            0 => Event::None,
            n if on_fonts => self.scroll_fonts(-n * 3),
            n => self.nudge(n),
        }
    }

    pub(crate) fn next_tick(&self) -> Option<Duration> {
        let notice = self.notice.as_ref().map(|(_, at)| NOTICE.saturating_sub(at.elapsed()));
        let anim = self.animation().map(|_| FRAME);
        notice.into_iter().chain(anim).min()
    }

    /// A timer from [`Editor::next_tick`] fired: the notice may be due to go, an animation has its
    /// next frame (or, just after its last, settles).
    pub(crate) fn tick(&mut self) -> Event {
        if self.notice.as_ref().is_some_and(|(_, at)| at.elapsed() >= NOTICE) {
            self.notice = None;
        }
        Event::Repaint
    }
}

fn far(r: Rect, (x, y): (i32, i32), reach: f32) -> bool {
    let dx = (r.x - x).max(x - (r.right() - 1)).max(0) as f32;
    let dy = (r.y - y).max(y - (r.bottom() - 1)).max(0) as f32;
    dx.hypot(dy) > reach
}

fn pick_ring(r: Rect) -> [RECT; 4] {
    ring(r, r.inflate(-2))
}

/// `r` moved so whichever of its sides is nearer an edge running alongside it (per axis) lands on it.
fn snap_moved(r: Rect, to: &snap::Edges, th: i32) -> Rect {
    let axis = |lo: i32, len: i32, across: (i32, i32), edges: &[snap::Segment]| {
        let a = snap::snap_edge(lo, across, edges, th) - lo;
        let b = snap::snap_edge(lo + len, across, edges, th) - (lo + len);
        lo + if a != 0 && (b == 0 || a.abs() <= b.abs()) { a } else { b }
    };
    let x = axis(r.x, r.w, (r.y, r.bottom()), &to.xs);
    Rect { x, y: axis(r.y, r.h, (r.x, r.right()), &to.ys), ..r }
}

/// `r` with its dragged `edges` snapped to edges running past the cursor `at`, never inverting it.
fn snap_resized(r: Rect, edges: Edges, at: (i32, i32), to: &snap::Edges, th: i32) -> Rect {
    let (mut x0, mut y0, mut x1, mut y1) = (r.x, r.y, r.right(), r.bottom());
    let x = |v: i32| snap::snap_edge(v, (at.1, at.1), &to.xs, th);
    let y = |v: i32| snap::snap_edge(v, (at.0, at.0), &to.ys, th);
    if edges.l {
        x0 = x(x0).min(x1 - 1);
    }
    if edges.r {
        x1 = x(x1).max(x0 + 1);
    }
    if edges.t {
        y0 = y(y0).min(y1 - 1);
    }
    if edges.b {
        y1 = y(y1).max(y0 + 1);
    }
    Rect::from_points(x0, y0, x1, y1)
}

fn tool_key(vk: u32, digit: Option<u32>, letter: u8) -> bool {
    const VK_0: u32 = 0x30;
    const VK_NUMPAD0: u32 = 0x60;
    vk == letter as u32 || digit.is_some_and(|d| vk == VK_0 + d || vk == VK_NUMPAD0 + d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glint_shell::gfx::Painter;

    fn editor() -> Editor {
        Editor::new(Rect { x: 0, y: 0, w: 400, h: 300 }, None, vec![Rect { x: 0, y: 0, w: 1920, h: 1080 }], 1.0)
    }

    fn click(ed: &mut Editor, p: (i32, i32)) {
        ed.mouse_down(p, 0);
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
        ed.mouse_down((150, 100), 0);
        ed.mouse_move((150, 200));
        ed.mouse_up((150, 200));
        assert!(matches!(ed.marks[1], Mark::Step { at: (150.0, 200.0), tail: None, .. }), "a step moves");
        ed.mouse_down((350, 100), 0);
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
        let b = ed.marks[0].bbox();
        let inside = (b.x + b.w / 2, b.y + b.h / 2);
        ed.mouse_down(inside, 0);
        ed.mouse_up(inside);
        assert!(ed.typing.is_none(), "a single click picks it");
        ed.mouse_down(inside, 0);
        ed.mouse_up(inside);
        assert!(ed.typing.is_some());
        ed.key(0x23, false, false); // End
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
        ed.key(0x39, false, false);
        assert_eq!(ed.tool, Some(Tool::Eraser), "9 is the smart eraser, though it sits after pixelate");
        ed.key(0x45, false, false);
        assert_eq!(ed.tool, None, "E toggles it off");
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
        assert!(ed.picks_region(), "the region tool is on from the start");
        ed.mouse_move((3000, 700));
        assert_eq!(ed.output_rect(), monitors[2]);
        ed.mouse_move((1910, 1300));
        assert_eq!(ed.output_rect(), monitors[0], "in a gap, the nearest monitor");
        let sel = Rect { x: 2000, y: 1500, w: 300, h: 200 };
        ed.set_selection(sel, None);
        assert_eq!(ed.output_rect(), sel);
        assert!(!ed.picks_region());
        assert!(matches!(ed.key(0x51, false, false), Event::Repaint) && ed.picks_region(), "Q picks again");
        assert_eq!((ed.sel, ed.selected), (screen, false));
        let mut select_first = editor();
        select_first.key(0x51, false, false);
        assert!(select_first.selected && !select_first.picks_region(), "no region tool select-first");
    }

    #[test]
    fn draw_first_captures_on_release_or_edits_with_shift() {
        let screen = Rect { x: 0, y: 0, w: 1920, h: 1080 };
        let sel = Rect { x: 100, y: 200, w: 300, h: 150 };
        let mut ed = Editor::canvas(screen, vec![screen], 1.0, (500, 400));
        assert!(ed.picks_region());
        assert!(matches!(ed.region_picked(sel, None, false), Event::Finish(Output::Default)), "release: done");
        assert_eq!(ed.output_rect(), sel);

        let mut ed = Editor::canvas(screen, vec![screen], 1.0, (500, 400));
        ed.key(0x50, false, false); // P
        assert!(!ed.picks_region(), "a drawing tool draws");
        ed.mouse_down((150, 250), 0);
        ed.mouse_move((200, 300));
        ed.mouse_up((200, 300));
        assert_eq!(ed.marks.len(), 1);
        let q = cell(&ed, Item::Region);
        click(&mut ed, q);
        assert!(ed.picks_region(), "the region button switches back");
        assert!(matches!(ed.region_picked(sel, None, true), Event::Repaint), "Shift: keep editing");
        assert!(ed.selected && !ed.picks_region() && ed.marks.len() == 1, "the marks come along");
        assert_eq!(ed.bar.y, sel.bottom() + 10, "docked under the selection");
        assert!(matches!(ed.key(0x0D, false, false), Event::Finish(Output::Default)), "Enter finishes");
    }

    #[test]
    fn selection_snaps_to_nearby_edges() {
        let mut ed = editor();
        ed.set_snap(snap::edges(&[Rect { x: 500, y: 0, w: 300, h: 400 }]));
        ed.mouse_down((200, 150), 0);
        ed.mouse_move((295, 155));
        assert_eq!(ed.sel, Rect { x: 100, y: 0, w: 400, h: 300 }, "the right edge snaps to x 500");
        ed.mouse_up((295, 155));
        ed.mouse_down((500, 150), 0);
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
        ed.mouse_down((100, 150), 0);
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
        ed.mouse_down((100, 140), 0);
        ed.mouse_move((130, 150));
        ed.mouse_up((130, 150));
        let moved = Rect { x: 130, y: 110, ..start };
        assert_eq!(area(&ed), moved);
        ed.mouse_down((230, 190), 0);
        ed.mouse_move((250, 200));
        ed.mouse_up((250, 200));
        let resized = Rect { w: 120, h: 90, ..moved };
        assert_eq!(area(&ed), resized);
        ed.wheel(WHEEL_DELTA);
        ed.wheel(WHEEL_DELTA);
        assert_eq!(ed.marks[0].strength(), Some(6.0));

        ed.undo();
        assert_eq!(ed.marks[0].strength(), Some(4.0), "one wheel gesture is one step");
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
            ed.mouse_down(path[0], 0);
            ed.render(&bright, &painter, &font).unwrap();
            for &q in &path[1..] {
                ed.mouse_move(q);
                ed.render(&bright, &painter, &font).unwrap();
            }
            ed.mouse_up(path[3]);
            ed.render(&bright, &painter, &font).unwrap();
        }
        ed.tool = None;
        ed.mouse_down((200, 180), 0);
        ed.mouse_move((240, 150));
        ed.render(&bright, &painter, &font).unwrap();
        ed.mouse_up((240, 150));
        ed.render(&bright, &painter, &font).unwrap();
        assert_eq!(ed.marks.len(), 4);
        let patched = ed.view.as_ref().unwrap().pixels().to_vec();
        ed.render_view(&bright, &painter, None).unwrap();
        assert!(patched == ed.view.as_ref().unwrap().pixels(), "patches leave the view as a full render would");
    }

    fn shown(ed: &Editor) -> bool {
        ed.chrome().contains(&ed.bar)
    }

    fn cell(ed: &Editor, item: Item) -> (i32, i32) {
        let r = ed.cells.iter().find(|(i, _)| *i == item).unwrap().1;
        (r.x + r.w / 2, r.y + r.h / 2)
    }

    fn area_of(m: &Mark) -> Rect {
        *m.clone().area_mut().unwrap()
    }

    #[test]
    fn canvas_toolbar_docks_and_comes_to_the_cursor() {
        let screen = Rect { x: 0, y: 0, w: 1920, h: 1080 };
        let second = Rect { x: 1920, y: 0, w: 1280, h: 1024 };
        let mut ed = Editor::canvas(screen, vec![screen, second], 1.0, (500, 400));
        let docked = |ed: &Editor, m: Rect| ed.bar.y == m.y + 10 && (ed.bar.x + ed.bar.w / 2 - m.x - m.w / 2).abs() <= 1;
        assert!(shown(&ed) && docked(&ed, screen), "docked at the top from the start");
        ed.mouse_move((2500, 600));
        assert!(docked(&ed, second), "it follows the mouse to its monitor");
        ed.mouse_move((960, 600));
        ed.key(VK_TAB, false, false);
        assert_eq!((ed.bar.x + ed.bar.w / 2 - 960, ed.bar.y), (0, 610), "Tab brings the bar");
        ed.mouse_move((960, 520));
        assert!(!docked(&ed, screen), "still close enough");
        assert!(matches!(ed.mouse_move((960, 300)), Event::Repaint));
        assert!(docked(&ed, screen), "moving away docks it again");

        ed.key(VK_TAB, false, false);
        assert!(ed.bar.y == 310);
        ed.key_up(VK_TAB);
        assert!(docked(&ed, screen), "releasing Tab off the bar docks it");
        ed.key(VK_TAB, false, false);
        ed.mouse_move(cell(&ed, Item::Color(2)));
        ed.key_up(VK_TAB);
        assert!(ed.bar.y == 310, "releasing Tab on the bar keeps it here");
        let pen = cell(&ed, Item::Tool(Tool::Pen));
        click(&mut ed, pen);
        assert_eq!(ed.tool, Some(Tool::Pen));
        assert!(docked(&ed, screen), "picking a tool docks it");

        ed.key(VK_TAB, false, false);
        assert!(matches!(ed.key(0x1B, false, false), Event::Repaint) && docked(&ed, screen), "Esc docks it first");
        assert!(matches!(ed.key(0x1B, false, false), Event::Cancel));
        assert!(matches!(ed.right_click((960, 500)), Event::Cancel), "right-click off the marks cancels too");

        ed.set_selection(Rect { x: 100, y: 100, w: 400, h: 300 }, None);
        assert!(shown(&ed) && ed.bar.y == 410, "a picked region docks it under the selection");
        assert!(matches!(ed.right_click((960, 500)), Event::None), "select-first: nothing");
    }

    #[test]
    fn right_click_deletes_marks_and_cancels_drags() {
        let mut ed = editor();
        ed.key(0x52, false, false); // R
        for x in [50, 200] {
            ed.mouse_down((x, 50), 0);
            ed.mouse_move((x + 100, 150));
            ed.mouse_up((x + 100, 150));
        }
        assert_eq!(ed.marks.len(), 2);
        ed.key(0x50, false, false); // P: right-click reaches every kind of mark
        assert!(matches!(ed.right_click((50, 100)), Event::Repaint));
        assert_eq!(ed.marks.len(), 1);
        assert_eq!(area_of(&ed.marks[0]).x, 200);
        ed.undo();
        assert_eq!(ed.marks.len(), 2, "undoable");

        ed.mouse_down((60, 200), 0);
        ed.mouse_move((150, 250));
        assert!(matches!(ed.right_click((150, 250)), Event::Repaint));
        assert!(matches!(ed.mouse_up((150, 250)), Event::None));
        assert_eq!(ed.marks.len(), 2, "the stroke being drawn is dropped");

        ed.tool = None;
        let start = ed.sel;
        ed.mouse_down((400, 150), 0);
        ed.mouse_move((450, 150));
        assert_eq!(ed.sel.w, 450);
        ed.right_click((450, 150));
        ed.mouse_up((450, 150));
        assert_eq!(ed.sel, start, "a resize is undone");
        assert_eq!(ed.marks.len(), 2, "cancelling deletes nothing");
        assert!(matches!(ed.right_click((350, 280)), Event::None), "off the marks: nothing select-first");
    }

    #[test]
    fn ctrl_moves_the_selection_and_alt_stops_snapping() {
        let mut ed = editor();
        ed.set_snap(snap::edges(&[Rect { x: 500, y: 0, w: 300, h: 400 }]));
        ed.mouse_down((400, 150), 0);
        ed.mouse_move((420, 150));
        assert_eq!(ed.sel, Rect { x: 0, y: 0, w: 420, h: 300 });
        ed.set_mods(true, false, false);
        ed.mouse_move((450, 170));
        assert_eq!(ed.sel, Rect { x: 30, y: 20, w: 420, h: 300 }, "Ctrl moves the whole box");
        ed.set_mods(false, false, false);
        ed.mouse_move((460, 170));
        assert_eq!(ed.sel, Rect { x: 30, y: 20, w: 430, h: 300 }, "letting go resizes from there");
        ed.mouse_move((495, 170));
        assert_eq!(ed.sel.right(), 500, "and snaps");
        ed.set_mods(false, true, false);
        ed.mouse_move((495, 170));
        assert_eq!(ed.sel.right(), 495, "unless Alt is held");
        ed.right_click((495, 170));
        assert_eq!(ed.sel, Rect { x: 0, y: 0, w: 400, h: 300 }, "cancelling goes back to before the drag");
    }

    #[test]
    fn area_tools_click_to_cover_what_is_under_the_cursor() {
        let mut ed = editor();
        let (window, client) = (Rect { x: 50, y: 50, w: 200, h: 150 }, Rect { x: 58, y: 80, w: 184, h: 112 });
        ed.set_windows(vec![(window, Some(client)), (Rect { x: 350, y: 250, w: 200, h: 200 }, None)]);
        ed.mouse_move((100, 100));
        assert_eq!(ed.pick, None, "no area tool, no highlight");
        ed.key(0x42, false, false); // B
        assert_eq!(ed.pick, Some(client), "picking the tool highlights what's under the cursor");
        ed.mouse_move((100, 60));
        assert_eq!(ed.pick, Some(window), "the title bar picks the whole window");
        ed.mouse_move((380, 280));
        assert_eq!(ed.pick, Some(Rect { x: 350, y: 250, w: 50, h: 50 }), "clipped to the selection");
        ed.mouse_move((300, 20));
        assert_eq!(ed.pick, Some(ed.sel), "off every window, the monitor");

        click(&mut ed, (100, 100));
        assert!(matches!(ed.marks[0], Mark::Blur { rect, .. } if rect == client), "a click covers the client area");
        ed.mouse_move((100, 100));
        assert_eq!(ed.pick, None, "over its own mark, the tool edits instead");

        ed.mouse_down((300, 20), 0);
        ed.mouse_move((302, 22));
        assert_eq!(ed.pick, Some(ed.sel), "a wobble is still a click");
        ed.mouse_move((380, 60));
        assert_eq!(ed.pick, None, "a drag draws freely");
        ed.mouse_up((380, 60));
        assert_eq!(area_of(&ed.marks[1]), Rect { x: 300, y: 20, w: 80, h: 40 });

        ed.key(0x52, false, false); // R
        ed.set_element(Some(Rect { x: 70, y: 90, w: 40, h: 20 }));
        ed.mouse_move((80, 95));
        assert_eq!(ed.pick, Some(Rect { x: 70, y: 90, w: 40, h: 20 }), "a UI element beats its window");
        ed.set_element(None);
        assert!(matches!(ed.refresh_pick(), Event::Repaint));
        assert_eq!(ed.pick, Some(client));
        ed.undo();
        ed.undo();
        assert!(ed.marks.is_empty());
    }

    #[test]
    fn area_drags_snap_to_edges() {
        let mut ed = editor();
        ed.set_snap(snap::edges(&[Rect { x: 100, y: 100, w: 100, h: 100 }]));
        ed.key(0x52, false, false);
        ed.mouse_down((104, 97), 0);
        ed.mouse_move((195, 150));
        ed.mouse_up((195, 150));
        assert_eq!(area_of(&ed.marks[0]), Rect { x: 100, y: 100, w: 100, h: 50 });
        ed.key(0x50, false, false); // P
        ed.mouse_down((104, 97), 0);
        ed.mouse_move((195, 150));
        ed.mouse_up((195, 150));
        assert!(matches!(&ed.marks[1], Mark::Stroke { points, .. } if points[0] == (104.0, 97.0)), "pens don't snap");
    }

    #[test]
    fn smart_eraser_fills_with_the_colour_under_the_press() {
        let mut ed = editor();
        ed.key(0x45, false, false); // E
        ed.mouse_down((120, 130), 0xAB12_3456);
        ed.mouse_move((200, 200));
        ed.mouse_up((200, 200));
        assert_eq!(ed.marks[0], Mark::Eraser { rect: Rect { x: 120, y: 130, w: 80, h: 70 }, fill: 0x12_3456 });
        assert_eq!(ed.current().map(|(t, ..)| t), None, "no sizes for the eraser");

        let window = Rect { x: 250, y: 20, w: 100, h: 60 };
        ed.set_windows(vec![(window, None)]);
        ed.mouse_move((300, 40));
        ed.mouse_down((300, 40), 0x00FF_EE00);
        ed.mouse_up((300, 40));
        assert_eq!(ed.marks[1], Mark::Eraser { rect: window, fill: 0xFF_EE00 }, "a click erases the window");

        ed.mouse_down((150, 150), 0);
        ed.mouse_move((160, 150));
        ed.mouse_up((160, 150));
        assert_eq!(area_of(&ed.marks[0]), Rect { x: 130, y: 130, w: 80, h: 70 }, "it moves like a rectangle");
        ed.undo();
        assert_eq!(area_of(&ed.marks[0]).x, 120);
        ed.undo();
        assert_eq!(ed.marks.len(), 1);
    }

    #[test]
    fn grabs_come_from_anywhere_on_the_screen_and_land_in_the_shot() {
        let mut ed = editor();
        ed.key(0x47, false, false); // G
        ed.mouse_down((600, 500), 0);
        ed.mouse_move((800, 600));
        let src = Rect { x: 600, y: 500, w: 200, h: 100 };
        assert_eq!(ed.pick, Some(src), "the source is outlined while it's dragged");
        assert!(ed.extent().contains(&src), "outside the shot too");
        ed.mouse_up((800, 600));
        assert_eq!(ed.marks[0], Mark::Grab { src, rect: Rect { x: 100, y: 100, w: 200, h: 100 } }, "centred in the shot");

        ed.mouse_down((10, 10), 0);
        ed.mouse_move((60, 40));
        ed.mouse_up((60, 40));
        let inside = Rect { x: 10, y: 10, w: 50, h: 30 };
        assert_eq!(ed.marks[1], Mark::Grab { src: inside, rect: inside }, "a piece of the shot stays where it was");

        ed.picked = Some(0);
        ed.mouse_down((300, 200), 0);
        ed.mouse_move((340, 200));
        ed.mouse_up((340, 200));
        assert_eq!(area_of(&ed.marks[0]), Rect { x: 100, y: 100, w: 240, h: 120 }, "corners keep the proportions");
        ed.undo();
        assert_eq!(area_of(&ed.marks[0]).w, 200);

        ed.activate(Item::Tool(Tool::Rectangle));
        ed.mouse_down((600, 500), 0);
        ed.mouse_move((700, 600));
        ed.mouse_up((700, 600));
        assert_eq!(ed.marks.len(), 2, "other tools stay inside the shot");
    }

    #[test]
    fn cursors_are_placed_sized_and_deleted() {
        let image = |hot| Arc::new(CursorImage { w: 12, h: 20, hot, px: vec![0xFF00_0000; 240] });
        let mut ed = editor();
        ed.key(0x4D, false, false); // M
        click(&mut ed, (100, 100));
        assert!(ed.marks.is_empty(), "nothing to place without a picture");

        ed.set_arrow(image((0, 0)));
        click(&mut ed, (100, 100));
        assert_eq!(ed.marks[0].bbox(), Rect { x: 100, y: 100, w: 12, h: 20 });
        ed.activate(Item::Size(2));
        assert_eq!(ed.marks[0].bbox(), Rect { x: 100, y: 100, w: 18, h: 30 }, "150 % grows it from the hotspot");
        ed.mouse_down((110, 110), 0);
        ed.mouse_move((150, 130));
        ed.mouse_up((150, 130));
        assert_eq!(ed.marks[0].bbox(), Rect { x: 140, y: 120, w: 18, h: 30 }, "it moves");

        ed.activate(Item::Tool(Tool::Cursor));
        let steps = ed.history.len();
        ed.add_cursor(image((3, 4)), (50, 60));
        assert_eq!(ed.marks[1].bbox(), Rect { x: 47, y: 56, w: 12, h: 20 }, "the kept cursor sits on its hotspot");
        assert_eq!(ed.history.len(), steps, "keeping the cursor isn't an undo step");
        ed.right_click((48, 58));
        assert_eq!(ed.marks.len(), 1);
    }

    #[test]
    fn grabs_and_cursors_render_their_pixels() {
        let painter = Painter::new().unwrap();
        let mut bright = Dib::new(640, 480).unwrap();
        for (i, p) in bright.pixels_mut().iter_mut().enumerate() {
            *p = 0xFF00_0000 | (i as u32).wrapping_mul(2_654_435_761) >> 8;
        }
        let mut ed = editor();
        let src = Rect { x: 500, y: 400, w: 40, h: 30 };
        ed.marks.push(Mark::Grab { src, rect: Rect { x: 10, y: 20, ..src } });
        let red = Arc::new(CursorImage { w: 4, h: 4, hot: (0, 0), px: vec![0xFFFF_0000; 16] });
        ed.add_cursor(red, (200, 100));
        ed.render_view(&bright, &painter, None).unwrap();
        let view = ed.view.as_ref().unwrap();
        for (x, y) in [(0, 0), (39, 0), (0, 29), (39, 29), (17, 11)] {
            let shown = view.pixel(10 + x, 20 + y).unwrap() & 0x00FF_FFFF;
            assert_eq!(shown, bright.pixel(src.x + x, src.y + y).unwrap() & 0x00FF_FFFF, "({x}, {y})");
        }
        assert_eq!(view.pixel(201, 102).unwrap() & 0x00FF_FFFF, 0xFF_0000);
    }

    /// Keystroke-to-pixels time on a 3440x1440 selection: the editor's event, its damage, the
    /// view render and the composed paint of the damaged part (into an off-screen DIB).
    #[test]
    #[ignore = "timing: cargo test --release -- --ignored --nocapture typing_latency"]
    fn typing_latency_on_a_big_selection() {
        use windows::Win32::Graphics::Gdi::GdiFlush;
        let painter = Painter::new().unwrap();
        let font = painter.font(13.0).unwrap();
        let screen = Rect { x: 0, y: 0, w: 3440, h: 1440 };
        let mut bright = Dib::new(screen.w, screen.h).unwrap();
        for (i, p) in bright.pixels_mut().iter_mut().enumerate() {
            *p = (i as u32).wrapping_mul(2_654_435_761) >> 8;
        }
        let dim = Dib::new(screen.w, screen.h).unwrap();
        let window = Dib::new(screen.w, screen.h).unwrap();
        let mut ed = Editor::new(screen, None, vec![screen], 1.0);
        ed.marks.push(Mark::Blur { rect: Rect { x: 200, y: 200, w: 900, h: 600 }, radius: 8.0 });
        ed.marks.push(Mark::Rectangle { rect: Rect { x: 1500, y: 300, w: 900, h: 700 }, color: 0, width: 4.0 });
        ed.activate(Item::Tool(Tool::Text));
        click(&mut ed, (1700, 700));
        ed.render(&bright, &painter, &font).unwrap();
        ed.paint(window.dc, screen, &dim, &painter, &font).unwrap();
        let mut times = Vec::new();
        for u in "the quick brown fox jumps over the lazy dog".encode_utf16() {
            let t0 = Instant::now();
            let before = ed.frame();
            ed.char(u);
            let damage = ed.damage(&before);
            let rp = damage.iter().copied().reduce(hull).unwrap();
            ed.render(&bright, &painter, &font).unwrap();
            ed.paint(window.dc, rp, &dim, &painter, &font).unwrap();
            unsafe {
                let _ = GdiFlush();
            }
            times.push(t0.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        let n = times.len();
        println!(
            "typing on 3440x1440: median {:.2} ms, p90 {:.2} ms, max {:.2} ms over {n} keys",
            times[n / 2],
            times[n * 9 / 10],
            times[n - 1]
        );
    }
}
