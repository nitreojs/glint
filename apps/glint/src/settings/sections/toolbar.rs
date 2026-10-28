//! The editor toolbar page: the tools as rows to drag into order, each with its digit and letter
//! (a click on one waits for a new key) and a switch that shows or hides it, and separators that
//! split the toolbar into groups.

use std::collections::HashMap;
use std::time::Duration;

use eframe::egui::{
    self, Align, Align2, Color32, CursorIcon, Event, Id, Key, Layout, Rect, Response, Role, Sense, Shape, Stroke,
    StrokeKind, Ui, UiBuilder, WidgetInfo, pos2, vec2,
};
use glint_core::config::toolbar::{KeyKind, RESERVED, Slot, TOOLS, ToolKeys, hidden, kind, tidy};
use glint_ui::widgets::{self, Look, anim, ease, glide, keycaps, medium, mix, mono, mono_medium, row, sans, snap};

use crate::settings::SettingsApp;

/// What the rows call the tools.
const NAMES: [&str; TOOLS.len()] =
    ["pen", "highlighter", "arrow", "rectangle", "blur", "pixelate", "smart eraser", "text", "step", "cursor", "grab", "clipboard"];
const ROW_H: f32 = 34.0;
const SEP_H: f32 = 18.0;
const GAP: f32 = 4.0;
const SETTLE: f32 = 0.22;
/// How long a key that moved to another tool rings there, and says so above the list.
const SWAP_RING: f64 = 1.6;
const SWAP_NOTE: f64 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum RowId {
    Tool(usize),
    Separator(u64),
}

impl RowId {
    fn height(self) -> f32 {
        match self {
            RowId::Tool(_) => ROW_H,
            RowId::Separator(_) => SEP_H,
        }
    }

    fn id(self) -> Id {
        Id::unique(("toolbar-row", self))
    }
}

#[derive(Clone, Copy)]
struct Drag {
    row: RowId,
    /// Where the pointer holds the row, from its top.
    grab: f32,
    /// Where it would land: its index among the others.
    to: usize,
}

/// `tool` took its `kind` key from `other` at `at`.
#[derive(Clone, Copy)]
struct Swap {
    other: usize,
    kind: KeyKind,
    at: f64,
}

/// The page's list as it's edited: hidden tools keep their place in it while the page is open
/// (the settings file only lists the shown ones), separators keep theirs wherever they are.
#[derive(Default)]
pub(in crate::settings) struct ToolbarRows {
    /// Top to bottom, with whether each is shown.
    rows: Vec<(RowId, bool)>,
    next: u64,
    drag: Option<Drag>,
    dropped: Option<RowId>,
    /// The order drawn last frame, and where each row was, from the list's top.
    order: Vec<RowId>,
    tops: HashMap<RowId, f32>,
    top: f32,
    total: f32,
    /// The key chip waiting for a key, and where it is.
    listening: Option<(usize, KeyKind, Rect)>,
    /// Q was pressed for a letter.
    refused: bool,
    swap: Option<Swap>,
    /// Esc cancelled a rebind this frame: it doesn't close the window as well.
    esc_used: bool,
}

struct Chip {
    tool: usize,
    kind: KeyKind,
    key: Option<u8>,
    listening: bool,
    refused: bool,
    /// The accent ring of a key that just moved here.
    ring: f32,
    /// Hidden tools' keys show dimmed.
    dim: f32,
}

/// What a row's controls did this frame: its head (dragged, focused), a tool's key chips and
/// switch, a separator's cross.
struct Out {
    head: Response,
    /// A tool's digit and letter chips.
    chips: Option<[(Response, Rect); 2]>,
    shown: bool,
    remove: bool,
}

impl ToolbarRows {
    pub(in crate::settings) fn listening(&self) -> bool {
        self.listening.is_some()
    }

    /// Esc belongs to the page: it cancels a rebind, now or in this frame.
    pub(in crate::settings) fn busy(&self) -> bool {
        self.listening.is_some() || self.esc_used
    }

    pub(in crate::settings) fn tick(&mut self) {
        self.esc_used = false;
    }

    pub(in crate::settings) fn stop(&mut self) {
        self.listening = None;
        self.refused = false;
    }

    fn slots(&self) -> Vec<Slot> {
        tidy(self.rows.iter().filter(|r| r.1).map(|r| match r.0 {
            RowId::Tool(t) => Slot::Tool(t),
            RowId::Separator(_) => Slot::Separator,
        }))
    }

    /// Rows for `slots`, unless they already make them (a reset or a revert changes them).
    fn sync(&mut self, slots: &[Slot]) {
        if !self.rows.is_empty() && self.slots() == slots {
            return;
        }
        self.rows.clear();
        for &slot in slots {
            let id = match slot {
                Slot::Tool(t) => RowId::Tool(t),
                Slot::Separator => {
                    self.next += 1;
                    RowId::Separator(self.next)
                }
            };
            self.rows.push((id, true));
        }
        self.rows.extend(hidden(slots).map(|t| (RowId::Tool(t), false)));
        self.drag = None;
        self.stop();
    }

    /// A new separator at the top, fading in, to be dragged into place.
    fn add_separator(&mut self, ctx: &egui::Context) {
        self.next += 1;
        let id = RowId::Separator(self.next);
        self.rows.insert(0, (id, true));
        snap(ctx, id.id().with("in"), 0.0);
    }

    fn reorder(&mut self, from: usize, to: usize) {
        if from != to && to < self.rows.len() {
            let row = self.rows.remove(from);
            self.rows.insert(to, row);
        }
    }

    /// Keys pressed for the chip that's waiting: a letter or digit binds (taking it from the tool
    /// that had it, which gets this tool's old one), Backspace or Delete clears, Esc or a click
    /// elsewhere gives up. Nothing else on the page sees them.
    fn take_keys(&mut self, ui: &Ui, keys: &mut ToolKeys) {
        let Some((tool, want, rect)) = self.listening else { return };
        if ui.input(|i| i.pointer.any_pressed() && i.pointer.press_origin().is_some_and(|p| !rect.contains(p))) {
            self.stop();
            return;
        }
        let (pressed, now) = ui.input_mut(|i| {
            let pressed: Vec<Key> = i
                .events
                .iter()
                .filter_map(|e| match e {
                    Event::Key { key, pressed: true, modifiers, .. } if !modifiers.ctrl && !modifiers.alt => Some(*key),
                    _ => None,
                })
                .collect();
            i.events.retain(|e| !matches!(e, Event::Key { .. } | Event::Text(_)));
            (pressed, i.time)
        });
        for key in pressed {
            match key {
                Key::Escape => {
                    self.esc_used = true;
                    self.stop();
                }
                Key::Backspace | Key::Delete => {
                    keys.set(tool, want, None);
                    self.stop();
                }
                _ => {
                    let &[c] = key.name().as_bytes() else { continue };
                    if c == RESERVED && want == KeyKind::Letter {
                        self.refused = true;
                    } else if kind(c) == Some(want) {
                        if let Some(other) = keys.set(tool, want, Some(c)) {
                            self.swap = Some(Swap { other, kind: want, at: now });
                        }
                        self.stop();
                    }
                }
            }
            if self.listening.is_none() {
                break;
            }
        }
    }

    /// One line over the list: how it works, what to press, or where a key went; beside it, while
    /// no key is awaited, "add separator". Returns whether that was clicked.
    fn status(&self, ui: &mut Ui, look: &Look, keys: &ToolKeys) -> bool {
        let p = look.p;
        let now = ui.input(|i| i.time);
        let swap = self.swap.filter(|s| now - s.at < SWAP_NOTE);
        let (text, color) = match (self.listening, swap) {
            (Some(_), _) if self.refused => ("q picks the region · press another letter".to_owned(), p.warn),
            (Some((_, KeyKind::Letter, _)), _) => ("press a letter · backspace clears · esc cancels".to_owned(), p.t2),
            (Some((_, KeyKind::Digit, _)), _) => ("press a digit · backspace clears · esc cancels".to_owned(), p.t2),
            (None, Some(s)) => {
                let what = if s.kind == KeyKind::Digit { "digit" } else { "letter" };
                let text = match keys.0[s.other].get(s.kind) {
                    Some(k) => format!("{} has {} now", NAMES[s.other], (k as char).to_ascii_lowercase()),
                    None => format!("{} has no {what} now", NAMES[s.other]),
                };
                ui.ctx().request_repaint_after(Duration::from_secs_f64(SWAP_NOTE - (now - s.at)));
                (text, p.t2)
            }
            (None, None) => ("drag to reorder · click a key to change it".to_owned(), p.t3),
        };
        let w = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(vec2(w, 18.0), Sense::hover());
        let mut room = w;
        let mut add = false;
        if self.listening.is_none() {
            let mut right = ui.new_child(UiBuilder::new().id_salt("toolbar-add").max_rect(rect).layout(Layout::right_to_left(Align::Center)));
            let link = widgets::link(&mut right, look, "add separator");
            add = link.clicked();
            room = link.rect.left() - rect.left() - 12.0;
        }
        let mut job = egui::text::LayoutJob::simple_singleline(text.clone(), sans(12.0), color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(room);
        let g = ui.painter().layout_job(job);
        ui.painter().galley(pos2(rect.left(), rect.center().y - g.size().y / 2.0), g, color);
        resp.widget_info(|| WidgetInfo::labeled(Role::Label, true, &text));
        add
    }

    /// The rows, laid out by hand so they can glide: each sits where the ones above end, plus an
    /// offset that eases to nothing after the order changes; the one being dragged follows the
    /// pointer, lifted over the others.
    fn list(&mut self, ui: &mut Ui, look: &Look, keys: &mut ToolKeys) {
        let ctx = ui.ctx().clone();
        let now = ui.input(|i| i.time);
        let drag = self.drag.filter(|d| self.rows.iter().any(|r| r.0 == d.row));
        let mut order: Vec<RowId> = self.rows.iter().map(|r| r.0).collect();
        if let Some(d) = drag {
            order.retain(|&r| r != d.row);
            order.insert(d.to.min(order.len()), d.row);
        }
        let mut natural = HashMap::new();
        let mut y = 0.0;
        for &r in &order {
            natural.insert(r, y);
            y += r.height() + GAP;
        }
        let total = (y - GAP).max(0.0);
        if order != self.order {
            for &r in &order {
                if let Some(prev) = self.tops.get(&r) {
                    snap(&ctx, r.id().with("offset"), prev - natural[&r]);
                }
            }
            self.order = order.clone();
        }
        if let Some(r) = self.dropped.take()
            && let (Some(prev), Some(now)) = (self.tops.get(&r), natural.get(&r))
        {
            snap(&ctx, r.id().with("offset"), prev - now);
        }

        let area = ui.available_rect_before_wrap();
        let top = ui.cursor().top();
        (self.top, self.total) = (top, total);
        let pointer = ctx.pointer_interact_pos();
        let swap = self.swap.map(|s| (s, (1.0 - (now - s.at) / SWAP_RING).clamp(0.0, 1.0) as f32));
        if swap.is_some_and(|(_, t)| t > 0.0) {
            ctx.request_repaint();
        }
        // the lifted row is drawn last, over the others
        let mut drawn = order.clone();
        drawn.sort_by_key(|r| drag.is_some_and(|d| d.row == *r));
        let mut outs = Vec::new();
        for r in drawn {
            let lifted = drag.is_some_and(|d| d.row == r);
            let lift = ease(anim(ui, r.id().with("lift"), lifted, 0.12));
            let at = match (drag, pointer) {
                (Some(d), Some(ptr)) if lifted => (ptr.y - d.grab - top).clamp(0.0, (total - r.height()).max(0.0)),
                _ => natural[&r] + glide(ui, r.id().with("offset"), 0.0, SETTLE),
            };
            let rect = Rect::from_min_size(pos2(area.left(), top + at), vec2(area.width(), r.height()));
            let shown = self.rows.iter().find(|x| x.0 == r).is_some_and(|x| x.1);
            let out = match r {
                RowId::Tool(t) => {
                    let ring = |kind| swap.filter(|(s, _)| s.other == t && s.kind == kind).map_or(0.0, |(_, v)| v);
                    let chip = |kind| Chip {
                        tool: t,
                        kind,
                        key: keys.0[t].get(kind),
                        listening: self.listening.is_some_and(|(lt, lk, _)| lt == t && lk == kind),
                        refused: self.refused,
                        ring: ring(kind),
                        dim: if shown { 1.0 } else { 0.5 },
                    };
                    tool_row(ui, look, rect, (t, shown), lift, [chip(KeyKind::Digit), chip(KeyKind::Letter)])
                }
                RowId::Separator(_) => separator_row(ui, look, rect, r, lift),
            };
            self.tops.insert(r, at);
            outs.push((r, out));
        }
        ui.advance_cursor_after_rect(Rect::from_min_size(pos2(area.left(), top), vec2(area.width(), total)));
        self.tops.retain(|r, _| order.contains(r));

        for (r, out) in outs {
            let Some(i) = self.rows.iter().position(|x| x.0 == r) else { continue };
            if out.remove {
                self.rows.remove(i);
                break;
            }
            self.rows[i].1 = out.shown;
            if let (RowId::Tool(t), Some(chips)) = (r, &out.chips) {
                for ((resp, rect), kind) in chips.iter().zip([KeyKind::Digit, KeyKind::Letter]) {
                    if resp.clicked() {
                        let mine = self.listening.is_some_and(|(lt, lk, _)| lt == t && lk == kind);
                        self.stop();
                        if !mine {
                            self.listening = Some((t, kind, *rect));
                            self.swap = None;
                        }
                    }
                }
            }
            let head = out.head;
            if head.has_focus() {
                let step = ctx.input_mut(|i| {
                    i.consume_key(egui::Modifiers::ALT, Key::ArrowDown) as i32
                        - i.consume_key(egui::Modifiers::ALT, Key::ArrowUp) as i32
                });
                let to = i as i32 + step;
                if step != 0 && (0..self.rows.len() as i32).contains(&to) {
                    self.reorder(i, to as usize);
                    break;
                }
            }
            if let Some(ptr) = pointer {
                if head.drag_started() {
                    self.drag = Some(Drag { row: r, grab: ptr.y - (self.top + self.tops[&r]), to: i });
                    self.stop();
                }
                if let Some(d) = self.drag.filter(|d| d.row == r)
                    && head.dragged()
                {
                    // it passes a row above once its top, as drawn, rises past that row's middle,
                    // and a row below once its bottom sinks past it
                    let h = r.height();
                    let at = (ptr.y - d.grab - self.top).clamp(0.0, (self.total - h).max(0.0));
                    let (mut y, mut to, mut above) = (0.0, 0, true);
                    for &(other, _) in &self.rows {
                        let middle = y + other.height() / 2.0;
                        y += other.height() + GAP;
                        if other == r {
                            above = false;
                        } else if (above && at >= middle) || (!above && at + h > middle) {
                            to += 1;
                        }
                    }
                    self.drag = Some(Drag { to, ..d });
                }
            }
            if head.drag_stopped()
                && let Some(d) = self.drag.take().filter(|d| d.row == r)
            {
                self.reorder(i, d.to.min(self.rows.len() - 1));
                self.dropped = Some(r);
                break;
            }
        }
    }
}

/// Six dots a row is held by.
fn grip(ui: &Ui, look: &Look, left: f32, y: f32, t: f32) {
    for (dx, dy) in [(0.0, -4.0), (0.0, 0.0), (0.0, 4.0), (4.0, -4.0), (4.0, 0.0), (4.0, 4.0)] {
        ui.painter().circle_filled(pos2(left + 10.0 + dx, y + dy), 1.1, mix(look.p.t4, look.p.t2, t));
    }
}

/// The lifted row's shadow and card.
fn lifted_card(ui: &Ui, look: &Look, rect: Rect, fill: Color32, lift: f32) {
    let p = look.p;
    if lift > 0.0 {
        let fall = rect.translate(vec2(0.0, 4.0 * lift)).expand(2.0 * lift);
        ui.painter().add(Shape::rect_filled(fall, 8, Color32::from_black_alpha((70.0 * lift) as u8)));
    }
    ui.painter().rect(rect, 6, fill, Stroke::new(1.0, mix(p.line2, p.line3, lift)), StrokeKind::Inside);
}

fn row_head(ui: &Ui, rect: Rect, r: RowId, name: &str) -> (Response, f32) {
    let resp = ui.interact(rect, r.id(), Sense::click_and_drag());
    let hov = anim(ui, resp.id.with("hov"), resp.hovered(), 0.08);
    resp.widget_info(|| WidgetInfo::labeled(Role::ListItem, true, name));
    (resp, hov)
}

/// A tool: grip, name, its digit and letter, and the switch that shows it.
fn tool_row(ui: &mut Ui, look: &Look, rect: Rect, (t, shown): (usize, bool), lift: f32, chips: [Chip; 2]) -> Out {
    let p = look.p;
    let y = rect.center().y;
    let (head, hov) = row_head(ui, rect, RowId::Tool(t), NAMES[t]);
    let fill = mix(mix(p.field, p.ctl, hov * 0.7), p.ctl, lift);
    lifted_card(ui, look, rect, fill, lift);
    grip(ui, look, rect.left(), y, hov.max(lift));
    let on = ease(anim(ui, head.id.with("shown"), shown, 0.14));
    let name = ui.painter().layout_no_wrap(NAMES[t].to_owned(), medium(13.0), mix(p.t3, p.t1, on));
    ui.painter().galley(pos2(rect.left() + 26.0, y - name.size().y / 2.0), name, p.t1);

    let switch = Rect::from_min_size(pos2(rect.right() - 48.0, y - 10.0), vec2(46.0, 20.0));
    let mut child = ui.new_child(UiBuilder::new().id_salt(("toolbar-shown", t)).max_rect(switch).layout(Layout::left_to_right(Align::Center)));
    let mut now_shown = shown;
    let toggle = widgets::toggle(&mut child, look, &mut now_shown, "");
    ui.ctx().accesskit_node_builder(toggle.id, |n| n.set_label(format!("show {}", NAMES[t])));

    let [digit, letter] = chips;
    let letter = chip(ui, look, switch.left() - 8.0, y, &letter);
    let digit = chip(ui, look, letter.1.left() - 4.0, y, &digit);
    let head = if lift > 0.0 { head.on_hover_and_drag_cursor(CursorIcon::Grabbing) } else { head.on_hover_cursor(CursorIcon::Grab) };
    if look.keyboard && head.has_focus() {
        ui.painter().rect_stroke(rect.expand(2.0), 8.0, Stroke::new(2.0, p.focus), StrokeKind::Middle);
    }
    Out { head, chips: Some([digit, letter]), shown: now_shown, remove: false }
}

/// A key as a keycap in a field, flush right at `right`: the field shows on hover, in the accent
/// while it waits for a key (saying which kind), in the warning colour for a refused one, and
/// rings in the accent a moment after a key moved here. Returns it and its rect.
fn chip(ui: &mut Ui, look: &Look, right: f32, y: f32, c: &Chip) -> (Response, Rect) {
    let p = look.p;
    let id = Id::unique(("toolbar-key", c.tool, c.kind == KeyKind::Digit));
    let prompt = if c.kind == KeyKind::Digit { "0-9" } else { "a-z" };
    let shown_key = c.key.map(|k| (k as char).to_ascii_lowercase().to_string());
    let cap_w = |text: &str| ui.painter().layout_no_wrap(text.to_owned(), mono_medium(12.0), p.t1).size().x + 12.0;
    let target = if c.listening {
        ui.painter().layout_no_wrap(prompt.to_owned(), mono(11.0), p.t2).size().x + 16.0
    } else {
        cap_w(shown_key.as_deref().unwrap_or("0")) + 8.0
    };
    let w = glide(ui, id.with("w"), target, 0.12);
    let field = Rect::from_min_max(pos2(right - w, y - 13.0), pos2(right, y + 13.0));
    let resp = ui.interact(field, id, Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
    let h = anim(ui, id.with("hov"), resp.hovered() || resp.has_focus(), 0.08);
    let l = ease(anim(ui, id.with("lis"), c.listening, 0.1));
    let e = ease(anim(ui, id.with("err"), c.listening && c.refused, 0.12));
    let shown = h.max(l).max(e).max(c.ring);
    let border = mix(mix(p.line2, p.acc, l.max(c.ring)), p.warn, e);
    let mut painter = ui.painter().clone();
    painter.multiply_opacity(c.dim.max(l));
    painter.rect(field, 5, p.field.gamma_multiply(shown), Stroke::new(1.0 + l.max(e).max(c.ring), border.gamma_multiply(shown)), StrokeKind::Inside);
    if c.listening {
        painter.text(field.center(), Align2::CENTER_CENTER, prompt, mono(11.0), if c.refused { p.warn } else { p.t2 });
    } else if let Some(k) = &shown_key {
        keycaps(&painter, ui, &p, field.center().x - cap_w(k) / 2.0, y, std::slice::from_ref(k), false);
    } else {
        painter.text(field.center(), Align2::CENTER_CENTER, "–", mono(12.0), p.t4);
    }
    if look.keyboard && resp.has_focus() {
        ui.painter().rect_stroke(field.expand(2.0), 7.0, Stroke::new(2.0, p.focus), StrokeKind::Middle);
    }
    let what = if c.kind == KeyKind::Digit { "digit" } else { "letter" };
    let value = match &shown_key {
        _ if c.listening => format!("press a {what}…"),
        Some(k) => k.clone(),
        None => "none".into(),
    };
    resp.widget_info(|| WidgetInfo {
        current_text_value: Some(value.clone()),
        ..WidgetInfo::labeled(Role::Button, true, format!("{} {what}", NAMES[c.tool]))
    });
    (resp, field)
}

/// A separator: grip, a dashed line, and a cross that takes it out. A new one fades in.
fn separator_row(ui: &mut Ui, look: &Look, rect: Rect, r: RowId, lift: f32) -> Out {
    let p = look.p;
    let y = rect.center().y;
    let fade = glide(ui, r.id().with("in"), 1.0, 0.16);
    let (head, hov) = row_head(ui, rect, r, "separator");
    let t = hov.max(lift);
    if lift > 0.0 {
        lifted_card(ui, look, rect, p.ctl, lift);
    }
    let mut painter = ui.painter().clone();
    painter.multiply_opacity(fade);
    for (dx, dy) in [(0.0, -2.0), (0.0, 2.0), (4.0, -2.0), (4.0, 2.0)] {
        painter.circle_filled(pos2(rect.left() + 10.0 + dx, y + dy), 1.1, mix(p.t4, p.t2, t));
    }
    let cross = Rect::from_center_size(pos2(rect.right() - 14.0, y), vec2(18.0, 18.0));
    let (x0, x1) = (rect.left() + 26.0, cross.left() - 8.0);
    let mut x = x0;
    while x < x1 {
        painter.hline(x..=(x + 4.0).min(x1), y, Stroke::new(1.0, mix(p.line3, p.t3, t)));
        x += 7.0;
    }
    let remove = ui.interact(cross, r.id().with("remove"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
    let rh = anim(ui, remove.id.with("hov"), remove.hovered(), 0.08);
    painter.circle_filled(cross.center(), 9.0, p.hov.gamma_multiply(rh));
    let c = cross.center();
    let ink = Stroke::new(1.3, mix(p.t3, p.t1, rh.max(hov * 0.5)));
    painter.line_segment([c + vec2(-3.5, -3.5), c + vec2(3.5, 3.5)], ink);
    painter.line_segment([c + vec2(-3.5, 3.5), c + vec2(3.5, -3.5)], ink);
    remove.widget_info(|| WidgetInfo::labeled(Role::Button, true, "remove separator"));
    let head = if lift > 0.0 { head.on_hover_and_drag_cursor(CursorIcon::Grabbing) } else { head.on_hover_cursor(CursorIcon::Grab) };
    if look.keyboard && head.has_focus() {
        ui.painter().rect_stroke(rect.expand(2.0), 8.0, Stroke::new(2.0, p.focus), StrokeKind::Middle);
    }
    Out { head, chips: None, shown: true, remove: remove.clicked() }
}

impl SettingsApp {
    pub(in crate::settings) fn toolbar_page(&mut self, ui: &mut Ui, look: &Look) {
        let (rows, cfg) = (&mut self.toolbar, &mut self.cfg);
        rows.sync(&cfg.toolbar);
        row(ui, look, "tools", 1.0, |ui| {
            rows.take_keys(ui, &mut cfg.toolbar_keys);
            if rows.status(ui, look, &cfg.toolbar_keys) {
                rows.add_separator(ui.ctx());
            }
            ui.add_space(4.0);
            rows.list(ui, look, &mut cfg.toolbar_keys);
        });
        row(ui, look, "other keys", 1.0, |ui| {
            let keys = [("q", "region, drawing first"), ("shift+1-5", "colours"), ("[ ]", "size")];
            widgets::shortcuts(ui, look, &keys);
        });
        cfg.toolbar = rows.slots();
    }
}
