use std::sync::atomic::Ordering;

use eframe::egui::{
    self, Align, Align2, Color32, CursorIcon, Id, Layout, Margin, Rect, Response, RichText, Role, Sense, Shape, Stroke,
    StrokeKind, Ui, UiBuilder, WidgetInfo, pos2, vec2,
};

use super::crop::Crop;
use super::export::{self, Format, Job, MB, TARGETS_MB};
use super::math::{GIF_FPS, GIF_WARN_BYTES, fmt_short};
use super::{ClipApp, PAD};
use crate::settings::widgets::{self, Look, anim, ease, glide, mix, mono, sans, semibold};

const ROW_GAP: f32 = 14.0;
const SAVE_H: f32 = 44.0;
const MORE_W: f32 = 40.0;

fn label(ui: &mut Ui, look: &Look, text: &str) {
    ui.label(RichText::new(text).font(sans(12.0)).color(look.p.t3));
    ui.add_space(-2.0);
}

/// A rounded size choice; the accent wash slides in when chosen.
fn pill(ui: &mut Ui, look: &Look, text: &str, on: bool) -> Response {
    let p = &look.p;
    let g = ui.painter().layout_no_wrap(text.to_string(), sans(12.0), p.t1);
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 20.0, 26.0), Sense::click());
    let t = ease(anim(ui, resp.id.with("on"), on, 0.2));
    let h = anim(ui, resp.id.with("hov"), resp.hovered(), 0.12);
    let fill = mix(p.field, p.acc.gamma_multiply(0.16), t);
    let ring = mix(mix(p.line2, p.line3, h), p.acc, t);
    ui.painter().rect(rect, 13, fill, Stroke::new(1.0, ring), StrokeKind::Inside);
    ui.painter().galley(rect.center() - g.size() / 2.0, g, mix(p.t2, p.t1, t.max(h)));
    resp.widget_info(|| WidgetInfo::selected(Role::RadioButton, true, on, text));
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

fn sound_line(job: &Job, tracks: usize) -> String {
    match job {
        Job::Gif(_) => "no sound (gif)".into(),
        Job::Video(v) if v.tracks.is_empty() => "no sound".into(),
        Job::Video(v) if v.combine => "sound combined into 1 track".into(),
        Job::Video(v) if v.tracks.len() == 1 && tracks == 1 => "with sound".into(),
        Job::Video(v) => format!("{} sound track{}", v.tracks.len(), if v.tracks.len() == 1 { "" } else { "s" }),
    }
}

impl ClipApp {
    pub(super) fn panel(&mut self, ui: &mut Ui, look: &Look, rect: Rect) {
        let p = look.p;
        ui.painter().vline(rect.left() + 0.5, rect.y_range(), Stroke::new(1.0, p.line));
        let inner = rect.shrink(PAD);
        let save_row = Rect::from_min_max(pos2(inner.left(), inner.bottom() - SAVE_H), inner.max);
        let est = Rect::from_min_max(pos2(inner.left(), save_row.top() - 14.0 - 54.0), pos2(inner.right(), save_row.top() - 14.0));
        let top = Rect::from_min_max(inner.min, pos2(inner.right(), est.top() - ROW_GAP));
        let Some(size) = self.info().map(|i| i.size) else {
            ui.painter().text(inner.left_top(), Align2::LEFT_TOP, "export", semibold(15.0), p.t1);
            return;
        };
        let mut ui = ui.new_child(UiBuilder::new().max_rect(top).layout(Layout::top_down(Align::Min)));
        let ui = &mut ui;
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        ui.label(RichText::new("export").font(semibold(15.0)).color(p.t1));
        ui.add_space(ROW_GAP - 6.0);

        let s = &mut self.saved.export;
        label(ui, look, "as");
        let formats = [(Format::Video, "video"), (Format::Gif, "gif")];
        widgets::segmented(ui, look, &mut s.format, &formats, Some(top.width()));
        ui.add_space(ROW_GAP - 6.0);

        label(ui, look, "size");
        let mut target = s.target_mb;
        let custom = &mut self.custom_mb;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            if pill(ui, look, "original", target.is_none() && custom.is_empty()).clicked() {
                target = None;
                custom.clear();
            }
            for mb in TARGETS_MB {
                if pill(ui, look, &format!("≤ {mb} MB"), target == Some(mb) && custom.is_empty()).clicked() {
                    target = Some(mb);
                    custom.clear();
                }
            }
            let (r, _) = ui.allocate_exact_size(vec2(64.0, 26.0), Sense::hover());
            let bg = ui.painter().add(Shape::Noop);
            let edit = egui::TextEdit::singleline(custom)
                .frame(egui::Frame::NONE.inner_margin(Margin::symmetric(9, 0)))
                .font(sans(12.0))
                .text_color(p.t1)
                .hint_text(RichText::new("… MB").font(sans(12.0)).color(p.t3))
                .align(Align2::LEFT_CENTER)
                .desired_width(64.0)
                .min_size(vec2(64.0, 26.0))
                .char_limit(6);
            let resp = ui.put(r, edit).on_hover_text("your own limit in MB");
            ui.ctx().accesskit_node_builder(resp.id, |n| n.set_label("size limit in MB"));
            if resp.changed() {
                custom.retain(|c| c.is_ascii_digit());
                target = custom.parse::<u32>().ok().filter(|&mb| mb > 0);
            }
            let f = anim(ui, resp.id.with("foc"), resp.has_focus(), 0.12);
            let on = !custom.is_empty();
            let ring = if on { p.acc } else { mix(p.line2, p.t2, f) };
            let fill = if on { p.acc.gamma_multiply(0.16) } else { p.field };
            ui.painter().set(bg, egui::epaint::RectShape::new(r, 13, fill, Stroke::new(1.0, ring), StrokeKind::Inside));
        });
        s.target_mb = target;
        ui.add_space(ROW_GAP - 6.0);

        let crop = self.edit.crop.filter(|c| !c.is_full(size)).map(|c| c.even(size));
        let base = export::base_size(size, crop);
        let labels = |h: Option<u32>| {
            let (w, h) = export::scaled(base, h);
            if crop.is_some() { format!("{w}×{h}") } else { format!("{h}p") }
        };
        let heights = export::heights(base);
        let chosen = s.height.filter(|h| heights.contains(h));
        label(ui, look, "resolution");
        let mut items = vec![format!("{} (original)", labels(None))];
        items.extend(heights.iter().map(|&h| labels(Some(h))));
        let current = match chosen {
            Some(h) => labels(Some(h)),
            None => items[0].clone(),
        };
        if let Some(i) = widgets::dropdown(ui, look, &current, &items, top.width()) {
            s.height = i.checked_sub(1).map(|i| heights[i]);
        }

        if s.format == Format::Gif {
            ui.add_space(ROW_GAP - 6.0);
            label(ui, look, "frame rate");
            let items: Vec<String> = GIF_FPS.iter().map(|f| format!("{f} fps")).collect();
            if let Some(i) = widgets::dropdown(ui, look, &format!("{} fps", s.gif_fps), &items, top.width()) {
                s.gif_fps = GIF_FPS[i];
            }
        }
        let unmuted = self.edit.muted.iter().filter(|&&m| !m).count();
        if s.format == Format::Video && unmuted > 1 {
            ui.add_space(ROW_GAP - 6.0);
            widgets::checkbox(ui, look, &mut s.combine, "combine sound into one track")
                .on_hover_text("mixes the sound you hear into a single track, for players that only play the first one");
        }
        if let Some(c) = crop {
            ui.add_space(ROW_GAP - 6.0);
            self.crop_row(ui, look, c);
        }

        let job = self.job();
        let src = self.source();
        if let (Some(job), Some(src)) = (&job, &src) {
            let bytes = export::estimate(job, src);
            let over = self.saved.export.target_mb.is_some_and(|mb| bytes > mb as u64 * MB)
                || matches!(job, Job::Gif(_)) && bytes > GIF_WARN_BYTES;
            let color = mix(p.t1, p.warn, anim(ui, Id::unique("clip-est-over"), over, 0.3));
            let painter = ui.painter();
            painter.text(est.left_top(), Align2::LEFT_TOP, format!("≈ {}", export::size_label(bytes)), semibold(26.0), color);
            let tracks = src.info.tracks.len();
            let sub = format!("{} of {} · {}", fmt_short(job.len()), fmt_short(src.info.duration), sound_line(job, tracks));
            painter.text(est.left_bottom(), Align2::LEFT_BOTTOM, sub, sans(12.0), p.t3);
        }
        let gif = self.saved.export.format == Format::Gif;
        self.save_button(ui, look, save_row, gif);
    }

    fn crop_row(&mut self, ui: &mut Ui, look: &Look, c: Crop) {
        let p = look.p;
        label(ui, look, "crop");
        let mut reset = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 0.0);
            ui.label(RichText::new(format!("{}×{}", c.w, c.h)).font(mono(12.0)).color(p.t1));
            ui.label(RichText::new("·").font(sans(13.0)).color(p.t3));
            reset = widgets::link(ui, look, "reset").clicked();
        });
        if reset {
            self.cropping = None;
            self.change(|e| {
                e.crop = None;
                true
            });
        }
    }

    fn save_button(&mut self, ui: &mut Ui, look: &Look, row: Rect, gif: bool) {
        let p = look.p;
        self.save_rect = row;
        let go = Rect::from_min_max(row.min, pos2(row.right() - MORE_W, row.bottom()));
        let more = Rect::from_min_max(pos2(go.right(), row.top()), row.max);
        let ready = self.info.is_ok();
        let go_resp = ui.interact(go, Id::unique("clip-save"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        let more_resp = ui
            .interact(more, Id::unique("clip-more"), Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("more ways to save");
        let since = self.now - self.flash;
        let progress = self.task.as_ref().map(|t| t.permille.load(Ordering::Relaxed) as f32 / 1000.0);
        let label = match (&self.task, &self.upload) {
            (Some(_), _) if go_resp.hovered() => "cancel".to_string(),
            (Some(t), _) if t.copy => "saving…".to_string(),
            (Some(_), _) => format!("saving {} %", (progress.unwrap_or(0.0) * 100.0).round()),
            (None, Some(u)) => format!("uploading to {}…", u.host),
            _ if since < super::SAVED_FLASH => "saved ✓".to_string(),
            _ if gif => "save gif".to_string(),
            _ => "save".to_string(),
        };
        if since < super::SAVED_FLASH {
            ui.ctx().request_repaint();
        }
        go_resp.widget_info(|| WidgetInfo::labeled(Role::Button, ready, &label));
        more_resp.widget_info(|| WidgetInfo::labeled(Role::Button, ready, "more ways to save"));
        let h = anim(ui, go_resp.id.with("hov"), go_resp.hovered(), 0.12);
        let hm = anim(ui, more_resp.id.with("hov"), more_resp.hovered() || self.menu, 0.12);
        let d = anim(ui, go_resp.id.with("prs"), go_resp.is_pointer_button_down_on(), 0.08);
        let painter = ui.painter();
        let left = egui::CornerRadius { nw: 8, sw: 8, ne: 0, se: 0 };
        let right = egui::CornerRadius { nw: 0, sw: 0, ne: 8, se: 8 };
        painter.rect_filled(go, left, mix(mix(p.acc, p.acch, h), p.accp, d));
        painter.rect_filled(more, right, mix(p.acc, p.acch, hm));
        painter.vline(more.left(), more.y_range(), Stroke::new(1.0, Color32::from_black_alpha(50)));
        let target = match (progress, since < super::SAVED_FLASH) {
            (Some(f), _) => f,
            (None, true) => 1.0,
            _ => 0.0,
        };
        let fill = glide(ui, Id::unique("clip-save-fill"), target, if target == 0.0 { 0.0 } else { 0.1 });
        let fade = if self.task.is_none() && since >= super::SAVED_FLASH { 0.0 } else { 1.0 };
        if fill > 0.0 && fade > 0.0 {
            let r = Rect::from_min_size(go.min, vec2(go.width() * fill, go.height()));
            painter.rect_filled(r, left, Color32::from_white_alpha(56));
        }
        painter.text(go.center(), Align2::CENTER_CENTER, &label, semibold(15.0), p.on_acc);
        let c = more.center();
        let chevron = vec![c + vec2(-4.5, -2.0), c + vec2(0.0, 2.5), c + vec2(4.5, -2.0)];
        painter.add(Shape::line(chevron, Stroke::new(1.6, p.on_acc)));
        if go_resp.clicked() && ready {
            match &self.task {
                Some(t) => t.cancel.store(true, Ordering::Relaxed),
                None if self.upload.is_none() => self.save(),
                None => {}
            }
        }
        if more_resp.clicked() {
            self.menu = !self.menu;
            self.sheet = false;
        }
    }
}
