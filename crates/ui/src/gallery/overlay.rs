//! What floats over the grid: a tile's menu and the toast at the bottom.

use eframe::egui::{
    self, Align2, Color32, CursorIcon, Id, Order, Pos2, Rect, Role, Sense, Stroke, StrokeKind, WidgetInfo, emath::TSTransform, pos2,
    vec2,
};
use glint_core::gallery::Kind;
use glint_ui::widgets::{self, Look, anim, ease, keycaps, mix, sans};

use super::{Action, Gallery};

const MENU_W: f32 = 220.0;
const ITEM_H: f32 = 32.0;
const SEPARATOR_H: f32 = 9.0;

#[derive(Clone, Copy, PartialEq)]
enum Entry {
    Open,
    Copy,
    CopyFile,
    CopyLink,
    Folder,
    Upload,
    Delete,
}

fn danger(light: bool) -> Color32 {
    if light { Color32::from_rgb(0xB4, 0x3A, 0x2C) } else { Color32::from_rgb(0xE7, 0x77, 0x6C) }
}

impl Gallery {
    fn entries(&self, id: u64) -> Vec<Option<(Entry, &'static str, &'static str)>> {
        let Some(item) = self.item(id) else { return Vec::new() };
        let shot = item.kind == Kind::Shot;
        let mut out = vec![Some((Entry::Open, if item.kind == Kind::Recording { "trim…" } else { "open" }, "enter"))];
        if shot {
            out.push(Some((Entry::Copy, "copy image", "ctrl+c")));
            out.push(Some((Entry::CopyFile, "copy file", "")));
        } else {
            out.push(Some((Entry::CopyFile, "copy file", "ctrl+c")));
        }
        if item.url.is_some() {
            out.push(Some((Entry::CopyLink, "copy link", "")));
        }
        out.push(Some((Entry::Folder, "show in folder", "")));
        if self.uploadable && !self.uploads.iter().any(|u| u.id == id) {
            out.push(Some((Entry::Upload, "upload", "")));
        }
        out.push(None);
        out.push(Some((Entry::Delete, "delete", "del")));
        out
    }

    pub(super) fn menu_popup(&mut self, ctx: &egui::Context, look: &Look) {
        let p = look.p;
        let open = self.menu.is_some();
        let t = ease(widgets::anim_ctx(ctx, Id::unique("gallery-menu-open"), open, 0.14));
        let Some(menu) = &self.menu else { return };
        if t <= 0.0 {
            return;
        }
        let (id, at) = (menu.id, menu.at);
        let entries = self.entries(id);
        let h = entries.iter().map(|e| if e.is_some() { ITEM_H } else { SEPARATOR_H }).sum::<f32>() + 8.0;
        // opens down and right of the pointer, or wherever it fits
        let screen = ctx.content_rect();
        let x = if at.x + MENU_W > screen.right() - 8.0 { at.x - MENU_W } else { at.x };
        let y = if at.y + h > screen.bottom() - 8.0 { at.y - h } else { at.y };
        let rect = Rect::from_min_size(pos2(x, y), vec2(MENU_W, h));
        let pivot: Pos2 = at;
        let s = 0.97 + 0.03 * t;
        let transform = TSTransform::from_translation(pivot.to_vec2()) * TSTransform::from_scaling(s) * TSTransform::from_translation(-pivot.to_vec2());
        let mut picked = None;
        let light = self.light == Some(true);
        let shown = egui::Area::new(Id::unique("gallery-menu")).order(Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
            ui.set_opacity(t);
            let painter = ui.painter();
            let shadow = egui::epaint::Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(70) };
            painter.add(shadow.as_shape(rect, 8));
            painter.rect(rect, 8, p.surf, Stroke::new(1.0, p.line2), StrokeKind::Inside);
            let mut y = rect.top() + 4.0;
            for (n, e) in entries.iter().enumerate() {
                let Some((entry, name, keys)) = e else {
                    ui.painter().hline(rect.left() + 4.0..=rect.right() - 4.0, y + 4.5, Stroke::new(1.0, p.line2));
                    y += SEPARATOR_H;
                    continue;
                };
                let r = Rect::from_min_size(pos2(rect.left() + 4.0, y), vec2(MENU_W - 8.0, ITEM_H));
                let resp = ui.interact(r, Id::unique(("gallery-menu-item", n)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
                resp.widget_info(|| WidgetInfo::labeled(Role::MenuItem, true, *name));
                let hov = anim(ui, resp.id.with("hov"), resp.hovered(), 0.1);
                let painter = ui.painter();
                painter.rect_filled(r, 5, p.ctlh.gamma_multiply(hov));
                let color = if *entry == Entry::Delete { danger(light) } else { mix(p.t2, p.t1, hov) };
                painter.text(pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, *name, sans(13.0), color);
                if !keys.is_empty() {
                    let g = painter.layout_no_wrap((*keys).to_string(), widgets::mono_medium(12.0), p.t1);
                    let x = r.right() - 10.0 - g.size().x - 12.0;
                    keycaps(painter, ui, &p, x, r.center().y, &[(*keys).to_string()], false);
                }
                if resp.clicked() {
                    picked = Some(*entry);
                }
                y += ITEM_H;
            }
        });
        ctx.set_transform_layer(shown.response.layer_id, transform);
        if !open {
            return;
        }
        let outside = ctx.input(|i| {
            (i.pointer.any_click() || i.pointer.secondary_pressed()) && i.pointer.interact_pos().is_some_and(|p| !rect.contains(p))
        });
        if outside && picked.is_none() {
            self.menu = None;
            // a right click elsewhere opens that tile's menu in the same frame, through the grid
            return;
        }
        let Some(entry) = picked else { return };
        self.menu = None;
        let url = self.item(id).and_then(|i| i.url.clone());
        let path = self.item(id).map(|i| i.path.clone());
        match entry {
            Entry::Open => self.open_item(id),
            Entry::Copy => self.copy(id, false),
            Entry::CopyFile => self.copy(id, true),
            Entry::CopyLink => {
                if let Some(u) = url {
                    self.copy_link(&u);
                }
            }
            Entry::Folder => {
                if let Some(p) = path {
                    glint_shell::explorer::reveal(&p);
                }
            }
            Entry::Upload => self.upload(id),
            Entry::Delete => self.delete(id),
        }
    }

    pub(super) fn show_toast(&mut self, ctx: &egui::Context, look: &Look) {
        let p = look.p;
        let Some(toast) = &self.toast else {
            // known while hidden, so the first toast slides in too
            widgets::anim_ctx(ctx, Id::unique("gallery-toast-shown"), false, 0.22);
            return;
        };
        let life = if toast.warn { 6.0 } else if toast.action.is_some() { super::UNDO } else { 2.4 };
        let age = self.now - toast.at;
        let hover_id = Id::unique("gallery-toast");
        let held = ctx.read_response(hover_id).is_some_and(|r| r.hovered());
        if held && let Some(t) = &mut self.toast {
            t.at = self.now - age.min(life - 0.5);
            // reading it: what it can undo stays undoable as long
            self.pending.hold(self.now);
        }
        let visible = age < life;
        let t = ease(widgets::anim_ctx(ctx, Id::unique("gallery-toast-shown"), visible, 0.22));
        if !visible && t <= 0.0 {
            self.toast = None;
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        let Some(toast) = &self.toast else { return };
        let color = if toast.warn { p.warn } else { p.t1 };
        let (text, action) = (toast.text.clone(), toast.action.clone());
        let body = self.panel;
        let mut act = false;
        egui::Area::new(Id::unique("gallery-toast-area")).order(Order::Tooltip).fixed_pos(body.min).interactable(true).show(ctx, |ui| {
            ui.set_opacity(t);
            let painter = ui.painter();
            let g = painter.layout(text, sans(13.0), color, body.width() - 120.0);
            let lg = action.as_ref().map(|(l, _)| painter.layout_no_wrap((*l).to_string(), sans(13.0), p.t1));
            let gap = if lg.is_some() { 14.0 } else { 0.0 };
            let w = g.size().x + gap + lg.as_ref().map_or(0.0, |l| l.size().x) + 28.0;
            let h = g.size().y.max(18.0) + 16.0;
            let r = Rect::from_min_size(pos2(body.center().x - w / 2.0, body.bottom() - 24.0 - h + 10.0 * (1.0 - t)), vec2(w, h));
            ui.interact(r, hover_id, Sense::hover());
            let shadow = egui::epaint::Shadow { offset: [0, 6], blur: 20, spread: 0, color: Color32::from_black_alpha(60) };
            painter.add(shadow.as_shape(r, 8));
            painter.rect(r, 8, p.ctl, Stroke::new(1.0, p.line2), StrokeKind::Inside);
            let gx = r.left() + 14.0;
            let gw = g.size().x;
            painter.galley(pos2(gx, r.center().y - g.size().y / 2.0), g, color);
            if let (Some(lg), Some((label, _))) = (lg, &action) {
                let at = pos2(gx + gw + gap, r.center().y - lg.size().y / 2.0);
                let lr = Rect::from_min_size(at, lg.size());
                let resp = ui.interact(lr.expand(4.0), Id::unique("gallery-toast-action"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
                resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, *label));
                let hv = anim(ui, resp.id.with("hov"), resp.hovered(), 0.1);
                ui.painter().hline(lr.x_range(), lr.bottom() + 1.0, Stroke::new(1.0, mix(p.line3, p.t1, hv)));
                ui.painter().galley(at, lg, p.t1);
                act |= resp.clicked();
            }
        });
        if act && let Some((_, action)) = action {
            match action {
                Action::Undo => self.undo(),
                Action::Link(url) => super::open_url(&url),
            }
        }
    }
}
