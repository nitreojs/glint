use eframe::egui::{
    self, Color32, CursorIcon, Id, Order, Painter, Pos2, Rect, Response, Role, Sense, Stroke, StrokeKind, Ui, WidgetInfo,
    pos2, vec2,
};
use glint_core::clip::clicks::visible;
use glint_core::clip::math;
use glint_core::record::clicks::{Button, Click, Shape, Style, state_at};
use glint_ui::widgets::{Look, anim, ease, medium, mix, sans};

use super::timeline::View;
use super::{ClipApp, Play};

const MARK_W: f32 = 12.0;
const MENU_W: f32 = 180.0;
const ITEM_H: f32 = 30.0;
const HANDLE: f32 = 20.0;
const HIDDEN: &str = "· hidden, not exported";

#[derive(Clone, Debug, PartialEq)]
pub(super) enum ClickDrag {
    Time { grab: usize, from_x: f32 },
    Box { from: i64, keep: Vec<usize> },
    Space { i: usize, from: (f32, f32), at: Pos2 },
}

pub(super) struct Axis {
    pub left: f32,
    pub width: f32,
    pub view: View,
    pub dur: i64,
    pub fps: f64,
    pub snapping: bool,
}

impl Axis {
    fn x(&self, t: i64) -> f32 {
        self.view.x(t, self.left, self.width)
    }

    fn t(&self, x: f32) -> i64 {
        self.view.start + (((x - self.left) / self.width) as f64 * self.view.span as f64) as i64
    }

    fn shows(&self, x: f32) -> bool {
        x >= self.left - MARK_W && x <= self.left + self.width + MARK_W
    }
}

pub(super) struct Lane {
    rect: Rect,
    chip: Rect,
    toggle: Response,
    marks: Vec<(usize, Response)>,
}

pub(super) fn color32(c: [f32; 4]) -> Color32 {
    let [r, g, b, a] = c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

fn count(n: usize) -> String {
    if n == 1 { "1 click".into() } else { format!("{n} clicks") }
}

fn button_name(b: Button) -> &'static str {
    match b {
        Button::Left => "left",
        Button::Right => "right",
        Button::Middle => "middle",
    }
}

#[derive(Clone, Copy)]
enum ClickItem {
    Delete,
    Set(Button),
}

impl ClickItem {
    fn label(self) -> &'static str {
        match self {
            ClickItem::Delete => "delete",
            ClickItem::Set(Button::Left) => "left click",
            ClickItem::Set(Button::Right) => "right click",
            ClickItem::Set(Button::Middle) => "middle click",
        }
    }
}

impl ClipApp {
    pub(super) fn click_style(&self) -> Option<Style> {
        let c = self.edit.clicks.as_ref()?;
        Some(self.saved.export.clicks.style(self.accent, c.scale))
    }

    pub(super) fn preview_shapes(&self) -> Vec<Shape> {
        match (&self.edit.clicks, self.click_style()) {
            (Some(c), Some(style)) if c.shown => state_at(&c.list, self.pos, &style),
            _ => Vec::new(),
        }
    }

    fn pick(&mut self, i: usize, add: bool) {
        if !add {
            self.click_sel.clear();
        }
        match self.click_sel.iter().position(|&s| s == i) {
            Some(at) if add => {
                self.click_sel.remove(at);
            }
            Some(_) => {}
            None => self.click_sel.push(i),
        }
    }

    pub(super) fn clicks_input(&mut self, ui: &mut Ui, look: &Look, lane: Rect, axis: &Axis) -> Option<Lane> {
        let clicks = self.edit.clicks.clone()?;
        let p = look.p;
        let bg = ui.interact(lane, Id::unique("clip-clicks-lane"), Sense::click_and_drag());
        bg.widget_info(|| WidgetInfo::labeled(Role::Group, true, "clicks"));
        let marks: Vec<(usize, Response)> = (clicks.list.iter().enumerate())
            .filter(|(_, c)| axis.shows(axis.x(c.down)))
            .map(|(i, c)| {
                let r = Rect::from_center_size(pos2(axis.x(c.down), lane.center().y), vec2(MARK_W, lane.height()));
                let resp = ui
                    .interact(r, Id::unique(("clip-click", i)), Sense::click_and_drag())
                    .on_hover_cursor(CursorIcon::PointingHand)
                    .on_hover_text("drag to move it in time · right-click to delete it or change the button");
                let chosen = self.click_sel.contains(&i);
                let name = format!("{} click at {}", button_name(c.button), math::fmt_time(c.down));
                resp.widget_info(|| WidgetInfo::selected(Role::Button, true, chosen, &name));
                (i, resp)
            })
            .collect();
        let label = ui.painter().layout_no_wrap(count(clicks.list.len()), medium(12.0), p.t1).size().x;
        let extra = if clicks.shown { 0.0 } else { ui.painter().layout_no_wrap(HIDDEN.into(), sans(12.0), p.t3).size().x + 6.0 };
        let h = (lane.height() - 4.0).min(20.0);
        let chip = Rect::from_min_size(pos2(lane.left() + 8.0, lane.center().y - h / 2.0), vec2(h + 4.0 + label + extra + 8.0, h));
        let tip = if clicks.shown { "click to hide them: hidden clicks aren't in the file" } else { "hidden: not in the file · click to show them" };
        let toggle = ui.interact(chip, Id::unique("clip-clicks-toggle"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip);
        toggle.widget_info(|| WidgetInfo::selected(Role::CheckBox, true, clicks.shown, "show clicks"));

        let add = ui.input(|i| i.modifiers.shift);
        if toggle.clicked() {
            self.change(|e| {
                e.clicks.as_mut().map(|c| c.shown = !c.shown).is_some()
            });
        }
        for (i, r) in &marks {
            if r.clicked() {
                self.pick(*i, add);
                self.click_focus = true;
                self.jump(clicks.list[*i].down);
            }
            if r.secondary_clicked() {
                if !self.click_sel.contains(i) {
                    self.pick(*i, false);
                }
                self.click_focus = true;
                let at = r.interact_pointer_pos().unwrap_or(r.rect.left_bottom());
                self.click_menu = Some((at, r.rect));
            }
            if r.drag_started()
                && let Some(o) = ui.input(|i| i.pointer.press_origin())
            {
                if !self.click_sel.contains(i) {
                    self.pick(*i, add);
                }
                self.click_focus = true;
                self.before_drag = Some(self.edit.clone());
                self.click_drag = Some(ClickDrag::Time { grab: *i, from_x: o.x });
            }
        }
        if bg.clicked() {
            if !add {
                self.click_sel.clear();
            }
            self.click_focus = true;
        }
        if bg.drag_started()
            && let Some(o) = ui.input(|i| i.pointer.press_origin())
        {
            let keep = if add { self.click_sel.clone() } else { Vec::new() };
            self.click_drag = Some(ClickDrag::Box { from: axis.t(o.x), keep });
            self.click_focus = true;
        }

        let (down, at) = ui.input(|i| (i.pointer.any_down(), i.pointer.interact_pos()));
        match (self.click_drag.clone(), at) {
            (Some(ClickDrag::Time { grab, from_x }), Some(at)) if down => {
                if let Some(mut base) = self.before_drag.as_ref().and_then(|b| b.clicks.clone())
                    && let Some(from) = base.list.get(grab).map(|c| c.down)
                {
                    let raw = from + axis.t(at.x) - axis.t(from_x);
                    let to = if axis.snapping { math::snap(raw.clamp(0, axis.dur - 1), axis.fps) } else { raw };
                    base.shift(&self.click_sel, to - from, axis.dur);
                    self.edit.clicks = Some(base);
                }
            }
            (Some(ClickDrag::Box { from, keep }), Some(at)) if down => {
                let mut picked = keep;
                for i in clicks.between(from, axis.t(at.x)) {
                    if !picked.contains(&i) {
                        picked.push(i);
                    }
                }
                self.click_sel = picked;
            }
            _ => {}
        }
        if self.click_drag.is_some() && !down {
            if !matches!(self.click_drag, Some(ClickDrag::Box { .. }))
                && let Some(before) = self.before_drag.take()
            {
                self.undo.record(before, &self.edit);
            }
            self.click_drag = None;
        }
        Some(Lane { rect: lane, chip, toggle, marks })
    }

    pub(super) fn clicks_paint(&self, ui: &Ui, look: &Look, lane: &Lane, axis: &Axis, painter: &Painter) {
        let (Some(clicks), Some(style)) = (&self.edit.clicks, self.click_style()) else { return };
        let p = look.p;
        let clip = painter.with_clip_rect(lane.rect.intersect(painter.clip_rect()));
        let fade = 1.0 - 0.62 * ease(anim(ui, Id::unique("clip-clicks-fade"), !clicks.shown, 0.3));
        clip.rect_filled(lane.rect, 6, p.ctl.gamma_multiply(fade));
        let y = lane.rect.center().y;
        let tint = |c: [f32; 4]| color32(c).gamma_multiply(fade);
        let colour = |b: Button| tint(match b {
            Button::Right => style.right,
            _ => style.left,
        });
        for c in &clicks.list {
            let end = c.up.map_or(lane.rect.right(), |u| axis.x(u));
            let start = axis.x(c.down);
            if end - start > 3.0 {
                clip.rect_filled(Rect::from_min_max(pos2(start, y - 2.0), pos2(end, y + 2.0)), 2, colour(c.button).gamma_multiply(0.45));
            }
        }
        for (i, resp) in &lane.marks {
            let c = clicks.list[*i];
            let at = pos2(axis.x(c.down), y);
            let r = if resp.hovered() || resp.dragged() { 5.5 } else { 4.5 };
            match c.button {
                Button::Middle => {
                    clip.circle_stroke(at, r - 0.5, Stroke::new(2.0, colour(c.button)));
                }
                b => {
                    clip.circle_filled(at, r, colour(b));
                }
            }
            if self.click_sel.contains(i) {
                clip.circle_stroke(at, r + 3.0, Stroke::new(1.5, p.t1));
            }
        }
        if let (Some(ClickDrag::Box { from, .. }), Some(at)) = (&self.click_drag, ui.input(|i| i.pointer.interact_pos())) {
            let (a, b) = (axis.x(*from), at.x);
            let r = Rect::from_min_max(pos2(a.min(b), lane.rect.top() + 1.0), pos2(a.max(b), lane.rect.bottom() - 1.0));
            clip.rect(r, 3, p.acc.gamma_multiply(0.15), Stroke::new(1.0, p.acc), StrokeKind::Inside);
        }
    }

    pub(super) fn clicks_chip(&self, ui: &Ui, look: &Look, lane: &Lane, painter: &Painter) {
        let Some(clicks) = &self.edit.clicks else { return };
        let p = look.p;
        let clip = painter.with_clip_rect(lane.rect.intersect(painter.clip_rect()));
        let chip = lane.chip;
        let fade = if clicks.shown { 1.0 } else { 0.6 };
        clip.rect_filled(chip, 5, p.ctl.gamma_multiply(0.9));
        let h = anim(ui, lane.toggle.id.with("hov"), lane.toggle.hovered(), 0.12);
        let icon = Rect::from_min_size(chip.min, vec2(chip.height(), chip.height()));
        clip.rect_filled(icon, 5, p.line2.gamma_multiply(h));
        let c = icon.center();
        clip.circle_stroke(c, 5.0, Stroke::new(1.5, p.t1.gamma_multiply(fade)));
        if clicks.shown {
            clip.circle_filled(c, 2.0, p.t1);
        } else {
            clip.line_segment([c + vec2(-6.0, 6.0), c + vec2(6.0, -6.0)], Stroke::new(1.5, p.t1.gamma_multiply(fade)));
        }
        let g = clip.layout_no_wrap(count(clicks.list.len()), medium(12.0), p.t1.gamma_multiply(fade));
        let at = pos2(icon.right() + 4.0, chip.center().y - g.size().y / 2.0);
        let w = g.size().x;
        clip.galley(at, g, p.t1);
        if !clicks.shown {
            clip.text(pos2(at.x + w + 6.0, chip.center().y), egui::Align2::LEFT_CENTER, HIDDEN, sans(12.0), p.t3);
        }
    }

    pub(super) fn clicks_overlay(&mut self, ui: &mut Ui, painter: &Painter, frame: Rect, scale: f32, cropping: bool) {
        let at = |x: f32, y: f32| frame.min + vec2(x, y) * scale;
        for s in self.preview_shapes() {
            let c = at(s.x, s.y);
            if s.radius > 0.0 {
                painter.circle_stroke(c, s.radius * scale, Stroke::new(s.width * scale, color32(s.color)));
            } else {
                painter.circle_filled(c, s.width * scale / 2.0, color32(s.color));
            }
        }
        let (Some(clicks), Some(style), Some(size)) = (self.edit.clicks.clone(), self.click_style(), self.info().map(|i| i.size)) else {
            return;
        };
        if self.play != Play::Paused || cropping || !clicks.shown {
            return;
        }
        let pos = self.pos;
        let near = |c: &Click| {
            let (from, to) = visible(c, &style);
            (from - style.ripple / 2..=to.max(from + style.ripple)).contains(&pos)
        };
        let (down, pointer) = ui.input(|i| (i.pointer.primary_down(), i.pointer.interact_pos()));
        for (i, c) in clicks.list.iter().enumerate().filter(|(_, c)| near(c)) {
            let centre = at(c.x, c.y);
            let resp = ui
                .interact(Rect::from_center_size(centre, vec2(HANDLE, HANDLE)), Id::unique(("clip-click-handle", i)), Sense::click_and_drag())
                .on_hover_cursor(CursorIcon::Move)
                .on_hover_text("drag to move this click");
            resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, format!("move the {} click", button_name(c.button))));
            if resp.clicked() {
                self.pick(i, false);
                self.click_focus = true;
            }
            if resp.drag_started()
                && let Some(o) = ui.input(|i| i.pointer.press_origin())
            {
                self.pick(i, false);
                self.click_focus = true;
                self.before_drag = Some(self.edit.clone());
                self.click_drag = Some(ClickDrag::Space { i, from: (c.x, c.y), at: o });
            }
            let lit = resp.hovered() || resp.dragged() || self.click_sel.contains(&i);
            painter.circle_stroke(centre, 7.0, Stroke::new(3.0, Color32::from_black_alpha(140)));
            painter.circle_stroke(centre, 7.0, Stroke::new(1.5, if lit { Color32::WHITE } else { Color32::from_white_alpha(170) }));
        }
        if let (Some(ClickDrag::Space { i, from, at: o }), Some(now), true) = (self.click_drag.clone(), pointer, down) {
            let d = (now - o) / scale;
            if let Some(c) = &mut self.edit.clicks {
                c.place(i, from.0 + d.x, from.1 + d.y, size);
            }
        }
    }

    pub(super) fn add_click_at(&mut self, frame: Rect, scale: f32, at: Pos2) {
        let Some(size) = self.info().map(|i| i.size) else { return };
        if !self.click_focus || self.edit.clicks.is_none() || !frame.contains(at) {
            return;
        }
        let v = (at - frame.min) / scale;
        let (x, y) = (v.x.clamp(0.0, size.0 as f32), v.y.clamp(0.0, size.1 as f32));
        let t = self.pos;
        let mut added = None;
        self.change(|e| {
            added = e.clicks.as_mut().map(|c| c.add(t, x, y));
            added.is_some()
        });
        if let Some(i) = added {
            self.click_sel = vec![i];
        }
    }

    pub(super) fn click_menu_popup(&mut self, ctx: &egui::Context, look: &Look) {
        let Some((at, anchor)) = self.click_menu else { return };
        let Some(clicks) = self.edit.clicks.clone() else {
            self.click_menu = None;
            return;
        };
        let picked_buttons: Vec<Button> = self.click_sel.iter().filter_map(|&i| clicks.list.get(i)).map(|c| c.button).collect();
        if picked_buttons.is_empty() {
            self.click_menu = None;
            return;
        }
        let p = look.p;
        let same = |b: Button| picked_buttons.iter().all(|&x| x == b);
        let items = [
            (ClickItem::Delete, true),
            (ClickItem::Set(Button::Left), !same(Button::Left)),
            (ClickItem::Set(Button::Right), !same(Button::Right)),
            (ClickItem::Set(Button::Middle), !same(Button::Middle)),
        ];
        let size = vec2(MENU_W, items.len() as f32 * ITEM_H + 8.0);
        let screen = ctx.content_rect();
        let y = if at.y + size.y > screen.bottom() - 4.0 { anchor.top() - 4.0 - size.y } else { at.y };
        let rect = Rect::from_min_size(pos2(at.x.min(screen.right() - 4.0 - size.x), y), size);
        let red = super::overlay::danger(self.light == Some(true));
        let mut chosen = None;
        egui::Area::new(Id::unique("clip-click-menu")).order(Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
            ui.painter().rect(rect, 8, p.surf, Stroke::new(1.0, p.line2), StrokeKind::Inside);
            let mut y = rect.top() + 4.0;
            for (i, (item, on)) in items.into_iter().enumerate() {
                let r = Rect::from_min_size(pos2(rect.left() + 4.0, y), vec2(MENU_W - 8.0, ITEM_H));
                let resp = ui.interact(r, Id::unique(("clip-click-menu-item", i)), if on { Sense::click() } else { Sense::hover() });
                let resp = if on { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp };
                resp.widget_info(|| WidgetInfo::labeled(Role::MenuItem, on, item.label()));
                let hov = anim(ui, resp.id.with("hov"), on && resp.hovered(), 0.1);
                let painter = ui.painter();
                painter.rect_filled(r, 5, p.ctlh.gamma_multiply(hov));
                let color = match (item, on) {
                    (_, false) => p.t3,
                    (ClickItem::Delete, _) => red,
                    _ => mix(p.t2, p.t1, hov),
                };
                painter.text(pos2(r.left() + 10.0, r.center().y), egui::Align2::LEFT_CENTER, item.label(), sans(13.0), color);
                if on && resp.clicked() {
                    chosen = Some(item);
                }
                y += ITEM_H;
            }
        });
        let outside = ctx.input(|i| {
            i.pointer.any_click() && i.pointer.interact_pos().is_some_and(|at| !rect.contains(at) && !anchor.contains(at))
        });
        if outside {
            self.click_menu = None;
        }
        let Some(item) = chosen else { return };
        self.click_menu = None;
        let picked = self.click_sel.clone();
        match item {
            ClickItem::Delete => {
                self.change(|e| e.clicks.as_mut().is_some_and(|c| c.delete(&picked)));
                self.click_sel.clear();
            }
            ClickItem::Set(b) => {
                self.change(|e| e.clicks.as_mut().is_some_and(|c| c.set_button(&picked, b)));
            }
        }
    }
}
