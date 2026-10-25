use eframe::egui::{
    self, Align, Align2, Color32, CursorIcon, Id, Layout, Order, Rect, RichText, Role, Sense, Stroke, StrokeKind, Ui,
    WidgetInfo, emath::TSTransform, pos2, vec2,
};
use glint_ui::widgets::{self, Kind, Look, anim, ease, hint, keycaps, mix, sans, semibold};

use super::{ClipApp, Note, Then, describe};

const MENU_W: f32 = 220.0;
const ITEM_H: f32 = 32.0;

enum Item {
    SaveAs,
    Copy,
    Upload,
    Folder,
    Delete,
}

const ITEMS: [(Item, &str, &str); 5] = [
    (Item::SaveAs, "save as…", "ctrl+shift+s"),
    (Item::Copy, "copy file", "ctrl+c"),
    (Item::Upload, "upload", "ctrl+u"),
    (Item::Folder, "open folder", ""),
    (Item::Delete, "delete recording…", ""),
];

/// The keys sheet: caps, what follows them, what they do.
const KEYS: [(&[&str], &str, &str); 18] = [
    (&["space"], "", "play / pause"),
    (&["←", "→"], "", "step a frame"),
    (&["I", "O"], "", "trim start / end here"),
    (&["S"], "", "cut here"),
    (&["delete"], "", "remove the selected piece or clicks"),
    (&["M"], "", "add marker"),
    (&["[", "]"], "", "previous / next marker"),
    (&["ctrl"], "+ drag", "no snapping"),
    (&["ctrl"], "+ wheel", "zoom the timeline or the video"),
    (&["space"], "+ drag", "pan the video (or middle-drag)"),
    (&["0"], "/ double-click", "fit the video"),
    (&["1"], "", "video at 100 %"),
    (&["C"], "", "crop"),
    (&["ctrl+z"], "", "undo (ctrl+y redoes)"),
    (&["enter"], "", "save"),
    (&["ctrl+shift+s"], "", "save as…"),
    (&["ctrl+c"], "", "copy the file"),
    (&["?"], "", "this sheet"),
];

fn caps(keys: &[&str]) -> Vec<String> {
    keys.iter().map(|k| (*k).to_string()).collect()
}

pub(super) fn danger(light: bool) -> Color32 {
    if light { Color32::from_rgb(0xB4, 0x3A, 0x2C) } else { Color32::from_rgb(0xE7, 0x77, 0x6C) }
}

impl ClipApp {
    pub(super) fn file_row(&mut self, ui: &mut Ui, look: &Look, rect: Rect) {
        let p = look.p;
        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        let meta = self.info().map(describe).unwrap_or_default();
        let meta = painter.layout_no_wrap(meta, sans(12.0), p.t3);
        let name_w = (rect.width() - meta.size().x - 12.0).max(80.0);
        let job = egui::text::LayoutJob {
            wrap: egui::text::TextWrapping::truncate_at_width(name_w),
            ..egui::text::LayoutJob::single_section(self.name.clone(), egui::TextFormat::simple(semibold(15.0), p.t1))
        };
        let name = painter.layout_job(job);
        let (nw, nh) = (name.size().x, name.size().y);
        painter.galley(rect.left_top(), name, p.t1);
        let my = rect.top() + nh - meta.size().y - 1.0;
        painter.galley(pos2(rect.left() + nw + 12.0, my), meta, p.t3);
    }

    pub(super) fn hint_row(&mut self, ui: &mut Ui, look: &Look, rect: Rect) {
        let p = look.p;
        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        let y = rect.center().y;
        let mut x = rect.left();
        let text = |x: f32, s: &str| painter.text(pos2(x, y), Align2::LEFT_CENTER, s, sans(12.0), p.t3).right();
        x = text(x, "drag the red edges to trim · ") + 2.0;
        x = keycaps(&painter, ui, &p, x, y, &caps(&["space"]), false);
        x = text(x, "play · ") + 2.0;
        x = keycaps(&painter, ui, &p, x, y, &caps(&["S"]), false);
        x = text(x, "cut · ") + 2.0;
        x = keycaps(&painter, ui, &p, x, y, &caps(&["M"]), false);
        text(x, "marker");
        let all = painter.layout_no_wrap("all keys".into(), sans(12.0), p.t3);
        let cap = painter.layout_no_wrap("?".into(), widgets::mono_medium(12.0), p.t1);
        let w = cap.size().x + 12.0 + 5.0 + all.size().x;
        let r = Rect::from_min_size(pos2(rect.right() - w, y - 10.0), vec2(w, 20.0));
        let resp = ui.interact(r, Id::unique("clip-keys"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "all keys"));
        let h = anim(ui, resp.id.with("hov"), resp.hovered(), 0.12);
        let after = keycaps(&painter, ui, &p, r.left(), y, &caps(&["?"]), false);
        painter.text(pos2(after, y), Align2::LEFT_CENTER, "all keys", sans(12.0), mix(p.t3, p.t1, h));
        if resp.clicked() {
            self.sheet = !self.sheet;
            self.menu = false;
        }
    }

    pub(super) fn menu_popup(&mut self, ctx: &egui::Context, look: &Look, anchor: Rect) {
        let p = look.p;
        let t = ease(widgets::anim_ctx(ctx, Id::unique("clip-menu-open"), self.menu, 0.18));
        if t <= 0.0 {
            return;
        }
        let h = ITEMS.len() as f32 * ITEM_H + 9.0 + 8.0;
        let rect = Rect::from_min_size(pos2(anchor.right() - MENU_W, anchor.top() - 8.0 - h), vec2(MENU_W, h));
        let area = egui::Area::new(Id::unique("clip-menu")).order(Order::Foreground).fixed_pos(rect.min).interactable(self.menu);
        let pivot = rect.right_bottom().to_vec2();
        let s = 0.98 + 0.02 * t;
        let lift = 6.0 * (1.0 - t);
        let transform = TSTransform::from_translation(pivot + vec2(0.0, lift)) * TSTransform::from_scaling(s) * TSTransform::from_translation(-pivot);
        let mut picked = None;
        let shown = area.show(ctx, |ui| {
            ui.set_opacity(t);
            let painter = ui.painter();
            painter.rect(rect, 8, p.surf, Stroke::new(1.0, p.line2), StrokeKind::Inside);
            let mut y = rect.top() + 4.0;
            for (i, (item, name, keys)) in ITEMS.iter().enumerate() {
                if i == 3 {
                    ui.painter().hline(rect.left() + 4.0..=rect.right() - 4.0, y + 4.5, Stroke::new(1.0, p.line2));
                    y += 9.0;
                }
                let r = Rect::from_min_size(pos2(rect.left() + 4.0, y), vec2(MENU_W - 8.0, ITEM_H));
                let mut resp = ui.interact(r, Id::unique(("clip-menu-item", i)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
                if matches!(item, Item::Upload) {
                    resp = resp.on_hover_text(format!("uploads to {}", self.host));
                }
                resp.widget_info(|| WidgetInfo::labeled(Role::MenuItem, true, *name));
                let hov = anim(ui, resp.id.with("hov"), resp.hovered(), 0.1);
                let danger = matches!(item, Item::Delete);
                let painter = ui.painter();
                painter.rect_filled(r, 5, p.ctlh.gamma_multiply(hov));
                let color = if danger { self::danger(self.light == Some(true)) } else { mix(p.t2, p.t1, hov) };
                painter.text(pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, *name, sans(13.0), color);
                if !keys.is_empty() {
                    let g = painter.layout_no_wrap((*keys).to_string(), widgets::mono_medium(12.0), p.t1);
                    let x = r.right() - 10.0 - g.size().x - 12.0;
                    keycaps(painter, ui, &p, x, r.center().y, &[(*keys).to_string()], false);
                }
                if resp.clicked() {
                    picked = Some(i);
                }
                y += ITEM_H;
            }
        });
        ctx.set_transform_layer(shown.response.layer_id, transform);
        if self.menu {
            let clicked_outside = ctx.input(|i| {
                i.pointer.any_click() && i.pointer.interact_pos().is_some_and(|at| !rect.contains(at) && !anchor.contains(at))
            });
            if clicked_outside {
                self.menu = false;
            }
        }
        let Some(i) = picked else { return };
        self.menu = false;
        match ITEMS[i].0 {
            Item::SaveAs => self.save_as(),
            Item::Copy => self.export(None, Then::Copy),
            Item::Upload => self.export(None, Then::Upload),
            Item::Folder => super::actions::show_in_folder(&self.path),
            Item::Delete => {
                self.pause();
                self.confirm_delete = true;
            }
        }
    }

    pub(super) fn keys_sheet(&mut self, ctx: &egui::Context, look: &Look, body: Rect) {
        let p = look.p;
        let t = ease(widgets::anim_ctx(ctx, Id::unique("clip-sheet-open"), self.sheet, 0.2));
        if t <= 0.0 {
            return;
        }
        let area = egui::Area::new(Id::unique("clip-sheet")).order(Order::Foreground).fixed_pos(body.min).interactable(self.sheet);
        let half = KEYS.len().div_ceil(2);
        let (row_h, col_w) = (28.0, 300.0);
        let card = Rect::from_center_size(body.center(), vec2(2.0 * col_w + 48.0 + 24.0, half as f32 * row_h + 40.0 + 40.0));
        let pivot = card.center().to_vec2();
        let s = 0.97 + 0.03 * t;
        let mut close = false;
        let shown = area.show(ctx, |ui| {
            ui.set_opacity(t);
            let back = ui.interact(body, Id::unique("clip-sheet-back"), Sense::click());
            close |= back.clicked();
            let painter = ui.painter();
            painter.rect_filled(body, 0, Color32::from_rgba_premultiplied(10, 10, 10, 204));
            painter.rect(card, 10, p.surf, Stroke::new(1.0, p.line2), StrokeKind::Inside);
            painter.text(card.left_top() + vec2(24.0, 20.0), Align2::LEFT_TOP, "keys", semibold(15.0), p.t1);
            for (i, (keys, after, what)) in KEYS.iter().enumerate() {
                let (col, row) = (i / half, i % half);
                let x = card.left() + 24.0 + col as f32 * (col_w + 24.0);
                let y = card.top() + 60.0 + row as f32 * row_h;
                let mut cx = x;
                for (k, key) in keys.iter().enumerate() {
                    if k > 0 {
                        cx += 2.0;
                    }
                    cx = keycaps(painter, ui, &p, cx, y, &[(*key).to_string()], false);
                }
                if !after.is_empty() {
                    painter.text(pos2(cx, y), Align2::LEFT_CENTER, *after, sans(13.0), p.t2);
                }
                painter.text(pos2(x + 128.0, y), Align2::LEFT_CENTER, *what, sans(13.0), p.t2);
            }
        });
        ctx.set_transform_layer(
            shown.response.layer_id,
            TSTransform::from_translation(pivot) * TSTransform::from_scaling(s) * TSTransform::from_translation(-pivot),
        );
        if close && self.sheet {
            self.sheet = false;
        }
    }

    pub(super) fn show_toast(&mut self, ctx: &egui::Context, look: &Look, body: Rect) {
        let p = look.p;
        let Some(toast) = &self.toast else { return };
        let life = match toast.note {
            Note::Failed(_) => 6.0,
            Note::Saved(..) | Note::Uploaded { .. } | Note::Copied(Some(_)) => 4.0,
            _ => 2.2,
        };
        let age = self.now - toast.at;
        let hovered_id = Id::unique("clip-toast");
        let held = ctx.read_response(hovered_id).is_some_and(|r| r.hovered());
        if held && let Some(t) = &mut self.toast {
            t.at = self.now - age.min(life - 0.5);
        }
        let visible = age < life;
        let t = ease(widgets::anim_ctx(ctx, Id::unique("clip-toast-shown"), visible, 0.25));
        if !visible && t <= 0.0 {
            self.toast = None;
            return;
        }
        ctx.request_repaint();
        let Some(toast) = &self.toast else { return };
        let (text, link, color) = match &toast.note {
            Note::Saved(path, saving) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let saving = saving.as_ref().map_or(String::new(), |s| format!(" · {s}"));
                (format!("saved {name}{saving} · "), Some("show in folder"), p.t1)
            }
            Note::Uploaded { url, copied } => {
                (format!("{} {url} · ", if *copied { "uploaded, link copied:" } else { "uploaded:" }), Some("open"), p.t1)
            }
            Note::Copied(saving) => {
                let saving = saving.as_ref().map_or(String::new(), |s| format!(" · {s}"));
                (format!("copied, paste it anywhere{saving}"), None, p.t1)
            }
            Note::Info(s) => (s.clone(), None, p.t2),
            Note::Failed(s) => (s.clone(), None, p.warn),
        };
        let note = toast.note.clone();
        let mut open = false;
        egui::Area::new(Id::unique("clip-toast-area")).order(Order::Tooltip).fixed_pos(body.min).interactable(true).show(ctx, |ui| {
            ui.set_opacity(t);
            let painter = ui.painter();
            let g = painter.layout(text, sans(13.0), color, body.width() - 120.0);
            let lg = link.map(|l| painter.layout_no_wrap(l.to_string(), sans(13.0), p.t1));
            let w = g.size().x + lg.as_ref().map_or(0.0, |l| l.size().x) + 28.0;
            let h = g.size().y.max(18.0) + 16.0;
            let r = Rect::from_min_size(pos2(body.center().x - w / 2.0, body.bottom() - 24.0 - h + 10.0 * (1.0 - t)), vec2(w, h));
            ui.interact(r, hovered_id, Sense::hover());
            painter.rect(r, 8, p.ctl, Stroke::new(1.0, p.line2), StrokeKind::Inside);
            let gx = r.left() + 14.0;
            let gw = g.size().x;
            painter.galley(pos2(gx, r.center().y - g.size().y / 2.0), g, color);
            if let Some(lg) = lg {
                let at = pos2(gx + gw, r.center().y - lg.size().y / 2.0);
                let lr = Rect::from_min_size(at, lg.size());
                let lresp = ui.interact(lr.expand(3.0), Id::unique("clip-toast-link"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
                lresp.widget_info(|| WidgetInfo::labeled(Role::Link, true, link.unwrap_or_default()));
                let hv = anim(ui, lresp.id.with("hov"), lresp.hovered(), 0.1);
                ui.painter().hline(lr.x_range(), lr.bottom() + 1.0, Stroke::new(1.0, mix(p.line3, p.t1, hv)));
                ui.painter().galley(at, lg, p.t1);
                open |= lresp.clicked();
            }
        });
        if open {
            match note {
                Note::Saved(path, _) => super::actions::show_in_folder(&path),
                Note::Uploaded { url, .. } => super::actions::open_url(&url),
                _ => {}
            }
        }
    }

    pub(super) fn delete_dialog(&mut self, ctx: &egui::Context, look: &Look) {
        if !self.confirm_delete {
            return;
        }
        let p = look.p;
        let frame = egui::Frame::new()
            .fill(p.surf)
            .stroke(Stroke::new(1.0, p.line2))
            .corner_radius(8)
            .inner_margin(egui::Margin::same(20))
            .shadow(egui::epaint::Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(80) });
        let modal = egui::Modal::new(Id::unique("clip-delete-dialog")).frame(frame).backdrop_color(Color32::from_black_alpha(90));
        let mut delete = false;
        let resp = modal.show(ctx, |ui| {
            ui.set_width(360.0);
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            ui.label(RichText::new("delete this recording?").font(semibold(15.0)).color(p.t1));
            hint(ui, format!("{} goes to the recycle bin.", self.name), p.t3);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    delete = widgets::button(ui, look, "delete", Kind::Primary, 30.0, true).clicked();
                    if widgets::button(ui, look, "cancel", Kind::Secondary, 30.0, true).clicked() {
                        self.confirm_delete = false;
                    }
                });
            });
        });
        if resp.should_close() {
            self.confirm_delete = false;
        }
        if delete {
            self.confirm_delete = false;
            self.delete(ctx);
        }
    }
}
