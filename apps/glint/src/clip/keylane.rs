use eframe::egui::{
    self, Color32, CornerRadius, CursorIcon, Id, Order, Painter, Rect, Response, Role, Sense, Stroke, StrokeKind, Ui,
    WidgetInfo, pos2, vec2,
};
use glint_core::clip::math;
use glint_core::record::keys::{self, Pill, Position, Style, state_at};
use glint_ui::widgets::{Look, anim, ease, medium, mix, sans, semibold};

use super::clicklane::{Axis, Lane, color32};
use super::ClipApp;

const MARK_W: f32 = 12.0;
const MENU_W: f32 = 180.0;
const ITEM_H: f32 = 30.0;
const HIDDEN: &str = "· hidden, not exported";

#[derive(Clone, Debug, PartialEq)]
pub(super) enum KeyDrag {
    Time { grab: usize, from_x: f32 },
    Box { from: i64, keep: Vec<usize> },
}

fn count(n: usize) -> String {
    if n == 1 { "1 key".into() } else { format!("{n} keys") }
}

#[derive(Clone, Copy)]
enum KeyItem {
    Delete,
    Place(Position),
}

impl KeyItem {
    fn label(self) -> &'static str {
        match self {
            KeyItem::Delete => "delete",
            KeyItem::Place(Position::Left) => "align left",
            KeyItem::Place(Position::Centre) => "align centre",
            KeyItem::Place(Position::Right) => "align right",
        }
    }
}

impl ClipApp {
    pub(super) fn key_style(&self) -> Option<Style> {
        let k = self.edit.keys.as_ref()?;
        Some(self.saved.export.keys.style(self.accent, k.scale))
    }

    pub(super) fn preview_pills(&self, ctx: &egui::Context) -> Vec<Pill> {
        let (Some(keys), Some(style), Some(size)) = (&self.edit.keys, self.key_style(), self.info().map(|i| i.size)) else {
            return Vec::new();
        };
        if !keys.shown {
            return Vec::new();
        }
        let region = (size.0 as f32, size.1 as f32);
        let measure = |text: &str| ctx.fonts_mut(|f| f.layout_no_wrap(text.to_string(), medium(style.font), Color32::WHITE).size().x);
        state_at(&keys.list, self.pos, &style, region, &measure)
    }

    fn pick_key(&mut self, i: usize, add: bool) {
        if !add {
            self.key_sel.clear();
        }
        match self.key_sel.iter().position(|&s| s == i) {
            Some(at) if add => {
                self.key_sel.remove(at);
            }
            Some(_) => {}
            None => self.key_sel.push(i),
        }
    }

    pub(super) fn keys_input(&mut self, ui: &mut Ui, look: &Look, lane: Rect, axis: &Axis) -> Option<Lane> {
        let keys = self.edit.keys.clone()?;
        let p = look.p;
        let bg = ui.interact(lane, Id::unique("clip-keys-lane"), Sense::click_and_drag());
        bg.widget_info(|| WidgetInfo::labeled(Role::Group, true, "keys"));
        let marks: Vec<(usize, Response)> = (keys.list.iter().enumerate())
            .filter(|(_, k)| axis.shows(axis.x(k.at)))
            .map(|(i, k)| {
                let r = Rect::from_center_size(pos2(axis.x(k.at), lane.center().y), vec2(MARK_W, lane.height()));
                let resp = ui
                    .interact(r, Id::unique(("clip-key", i)), Sense::click_and_drag())
                    .on_hover_cursor(CursorIcon::PointingHand)
                    .on_hover_text("drag to move it in time · right-click to delete it or change where it sits");
                let chosen = self.key_sel.contains(&i);
                let name = format!("{} at {}", keys::label(k), math::fmt_time(k.at));
                resp.widget_info(|| WidgetInfo::selected(Role::Button, true, chosen, &name));
                (i, resp)
            })
            .collect();
        let label = ui.painter().layout_no_wrap(count(keys.list.len()), medium(12.0), p.t1).size().x;
        let extra = if keys.shown { 0.0 } else { ui.painter().layout_no_wrap(HIDDEN.into(), sans(12.0), p.t3).size().x + 6.0 };
        let h = (lane.height() - 4.0).min(20.0);
        let chip = Rect::from_min_size(pos2(lane.left() + 8.0, lane.center().y - h / 2.0), vec2(h + 4.0 + label + extra + 8.0, h));
        let tip = if keys.shown { "click to hide them: hidden keys aren't in the file" } else { "hidden: not in the file · click to show them" };
        let toggle = ui.interact(chip, Id::unique("clip-keys-toggle"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip);
        toggle.widget_info(|| WidgetInfo::selected(Role::CheckBox, true, keys.shown, "show keys"));

        let add = ui.input(|i| i.modifiers.shift);
        if toggle.clicked() {
            self.change(|e| e.keys.as_mut().map(|k| k.shown = !k.shown).is_some());
        }
        for (i, r) in &marks {
            if r.clicked() {
                self.pick_key(*i, add);
                self.key_focus = true;
                self.jump(keys.list[*i].at);
            }
            if r.secondary_clicked() {
                if !self.key_sel.contains(i) {
                    self.pick_key(*i, false);
                }
                self.key_focus = true;
                let at = r.interact_pointer_pos().unwrap_or(r.rect.left_bottom());
                self.key_menu = Some((at, r.rect));
            }
            if r.drag_started()
                && let Some(o) = ui.input(|i| i.pointer.press_origin())
            {
                if !self.key_sel.contains(i) {
                    self.pick_key(*i, add);
                }
                self.key_focus = true;
                self.before_drag = Some(self.edit.clone());
                self.key_drag = Some(KeyDrag::Time { grab: *i, from_x: o.x });
            }
        }
        if bg.clicked() {
            if !add {
                self.key_sel.clear();
            }
            self.key_focus = true;
        }
        if bg.drag_started()
            && let Some(o) = ui.input(|i| i.pointer.press_origin())
        {
            let keep = if add { self.key_sel.clone() } else { Vec::new() };
            self.key_drag = Some(KeyDrag::Box { from: axis.t(o.x), keep });
            self.key_focus = true;
        }

        let (down, at) = ui.input(|i| (i.pointer.any_down(), i.pointer.interact_pos()));
        match (self.key_drag.clone(), at) {
            (Some(KeyDrag::Time { grab, from_x }), Some(at)) if down => {
                if let Some(mut base) = self.before_drag.as_ref().and_then(|b| b.keys.clone())
                    && let Some(from) = base.list.get(grab).map(|k| k.at)
                {
                    let raw = from + axis.t(at.x) - axis.t(from_x);
                    let to = if axis.snapping { math::snap(raw.clamp(0, axis.dur - 1), axis.fps) } else { raw };
                    base.shift(&self.key_sel, to - from, axis.dur);
                    self.edit.keys = Some(base);
                }
            }
            (Some(KeyDrag::Box { from, keep }), Some(at)) if down => {
                let mut picked = keep;
                for i in keys.between(from, axis.t(at.x)) {
                    if !picked.contains(&i) {
                        picked.push(i);
                    }
                }
                self.key_sel = picked;
            }
            _ => {}
        }
        if self.key_drag.is_some() && !down {
            if !matches!(self.key_drag, Some(KeyDrag::Box { .. }))
                && let Some(before) = self.before_drag.take()
            {
                self.undo.record(before, &self.edit);
            }
            self.key_drag = None;
        }
        Some(Lane { rect: lane, chip, toggle, marks })
    }

    pub(super) fn keys_paint(&self, ui: &Ui, look: &Look, lane: &Lane, axis: &Axis, painter: &Painter) {
        let Some(keys) = &self.edit.keys else { return };
        let p = look.p;
        let clip = painter.with_clip_rect(lane.rect.intersect(painter.clip_rect()));
        let fade = 1.0 - 0.62 * ease(anim(ui, Id::unique("clip-keys-fade"), !keys.shown, 0.3));
        clip.rect_filled(lane.rect, 6, p.ctl.gamma_multiply(fade));
        let y = lane.rect.center().y;
        let accent = p.acc.gamma_multiply(fade);
        for k in &keys.list {
            let start = axis.x(k.at);
            let end = k.hold.map_or(start, |h| axis.x(h));
            if end - start > 3.0 {
                clip.rect_filled(Rect::from_min_max(pos2(start, y - 2.0), pos2(end, y + 2.0)), 2, accent.gamma_multiply(0.45));
            }
        }
        for (i, resp) in &lane.marks {
            let k = &keys.list[*i];
            let at = pos2(axis.x(k.at), y);
            let r = if resp.hovered() || resp.dragged() { 5.5 } else { 4.5 };
            let cap = Rect::from_center_size(at, vec2(2.0 * r, 2.0 * r));
            clip.rect_filled(cap, 2, accent);
            if self.key_sel.contains(i) {
                clip.rect_stroke(cap.expand(2.5), 3, Stroke::new(1.5, p.t1), StrokeKind::Outside);
            }
        }
        if let (Some(KeyDrag::Box { from, .. }), Some(at)) = (&self.key_drag, ui.input(|i| i.pointer.interact_pos())) {
            let (a, b) = (axis.x(*from), at.x);
            let r = Rect::from_min_max(pos2(a.min(b), lane.rect.top() + 1.0), pos2(a.max(b), lane.rect.bottom() - 1.0));
            clip.rect(r, 3, p.acc.gamma_multiply(0.15), Stroke::new(1.0, p.acc), StrokeKind::Inside);
        }
    }

    pub(super) fn keys_chip(&self, ui: &Ui, look: &Look, lane: &Lane, painter: &Painter) {
        let Some(keys) = &self.edit.keys else { return };
        let p = look.p;
        let clip = painter.with_clip_rect(lane.rect.intersect(painter.clip_rect()));
        let chip = lane.chip;
        let fade = if keys.shown { 1.0 } else { 0.6 };
        clip.rect_filled(chip, 5, p.ctl.gamma_multiply(0.9));
        let h = anim(ui, lane.toggle.id.with("hov"), lane.toggle.hovered(), 0.12);
        let icon = Rect::from_min_size(chip.min, vec2(chip.height(), chip.height()));
        clip.rect_filled(icon, 5, p.line2.gamma_multiply(h));
        let c = icon.center();
        let cap = Rect::from_center_size(c, vec2(11.0, 11.0));
        clip.rect_stroke(cap, 2, Stroke::new(1.5, p.t1.gamma_multiply(fade)), StrokeKind::Inside);
        if keys.shown {
            clip.rect_filled(Rect::from_center_size(c, vec2(3.0, 3.0)), 1, p.t1);
        } else {
            clip.line_segment([c + vec2(-6.0, 6.0), c + vec2(6.0, -6.0)], Stroke::new(1.5, p.t1.gamma_multiply(fade)));
        }
        let g = clip.layout_no_wrap(count(keys.list.len()), medium(12.0), p.t1.gamma_multiply(fade));
        let at = pos2(icon.right() + 4.0, chip.center().y - g.size().y / 2.0);
        let w = g.size().x;
        clip.galley(at, g, p.t1);
        if !keys.shown {
            clip.text(pos2(at.x + w + 6.0, chip.center().y), egui::Align2::LEFT_CENTER, HIDDEN, sans(12.0), p.t3);
        }
    }

    pub(super) fn keys_overlay(&self, ui: &Ui, painter: &Painter, frame: Rect, scale: f32) {
        for pill in self.preview_pills(ui.ctx()) {
            let min = frame.min + pill.rect.min.to_vec2() * scale;
            let max = frame.min + pill.rect.max.to_vec2() * scale;
            let rect = Rect::from_min_max(min, max);
            let a = pill.alpha;
            let tint = |c: [f32; 4]| color32(c).gamma_multiply(a);
            let r = pill.radius * scale;
            let d = pill.depth * scale;
            let top_r = r.round().max(0.0) as u8;
            let bot_r = (r - d).max(1.0).round() as u8;
            let face_cr = CornerRadius { nw: top_r, ne: top_r, sw: bot_r, se: bot_r };
            // drop shadow, offset down by the cap depth
            painter.rect_filled(rect.translate(vec2(0.0, d)), r, tint(pill.shadow));
            // raised bottom edge: fills the whole cap
            painter.rect_filled(rect, r, tint(pill.edge));
            // face, leaving the bottom `d` px showing the edge as the keycap lip
            let face = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - d));
            painter.rect_filled(face, face_cr, tint(pill.fill));
            // thin top highlight just inside the top of the face
            let hl = face.top() + 0.75 * scale;
            painter.hline(face.left() + r..=face.right() - r, hl, Stroke::new(1.5 * scale, tint(pill.top)));
            // thin outline around the face
            painter.rect_stroke(face, face_cr, Stroke::new(scale, tint(pill.border)), StrokeKind::Inside);
            // centred semibold label in the face area above the lip
            painter.text(face.center(), egui::Align2::CENTER_CENTER, &pill.text, semibold(pill.font * scale), tint(pill.text_color));
        }
    }

    pub(super) fn key_menu_popup(&mut self, ctx: &egui::Context, look: &Look) {
        let Some((at, anchor)) = self.key_menu else { return };
        let valid = self.edit.keys.as_ref().is_some_and(|k| self.key_sel.iter().any(|&i| i < k.list.len()));
        if !valid {
            self.key_menu = None;
            return;
        }
        let p = look.p;
        let here = self.saved.export.keys.pos;
        let items = [
            (KeyItem::Delete, true),
            (KeyItem::Place(Position::Left), here != Position::Left),
            (KeyItem::Place(Position::Centre), here != Position::Centre),
            (KeyItem::Place(Position::Right), here != Position::Right),
        ];
        let size = vec2(MENU_W, items.len() as f32 * ITEM_H + 8.0);
        let screen = ctx.content_rect();
        let y = if at.y + size.y > screen.bottom() - 4.0 { anchor.top() - 4.0 - size.y } else { at.y };
        let rect = Rect::from_min_size(pos2(at.x.min(screen.right() - 4.0 - size.x), y), size);
        let red = super::overlay::danger(self.light == Some(true));
        let mut chosen = None;
        egui::Area::new(Id::unique("clip-key-menu")).order(Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
            ui.painter().rect(rect, 8, p.surf, Stroke::new(1.0, p.line2), StrokeKind::Inside);
            let mut y = rect.top() + 4.0;
            for (i, (item, on)) in items.into_iter().enumerate() {
                let r = Rect::from_min_size(pos2(rect.left() + 4.0, y), vec2(MENU_W - 8.0, ITEM_H));
                let resp = ui.interact(r, Id::unique(("clip-key-menu-item", i)), if on { Sense::click() } else { Sense::hover() });
                let resp = if on { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp };
                resp.widget_info(|| WidgetInfo::labeled(Role::MenuItem, on, item.label()));
                let hov = anim(ui, resp.id.with("hov"), on && resp.hovered(), 0.1);
                let painter = ui.painter();
                painter.rect_filled(r, 5, p.ctlh.gamma_multiply(hov));
                let color = match (item, on) {
                    (_, false) => p.t3,
                    (KeyItem::Delete, _) => red,
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
            self.key_menu = None;
        }
        let Some(item) = chosen else { return };
        self.key_menu = None;
        match item {
            KeyItem::Delete => {
                let picked = self.key_sel.clone();
                self.change(|e| e.keys.as_mut().is_some_and(|k| k.delete(&picked)));
                self.key_sel.clear();
            }
            KeyItem::Place(pos) => self.saved.export.keys.pos = pos,
        }
    }
}
