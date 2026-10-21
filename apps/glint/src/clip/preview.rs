use eframe::egui::{
    self, Align, Align2, Color32, CursorIcon, Id, Layout, PointerButton, Pos2, Rect, Role, Sense, Shape, Stroke, StrokeKind,
    Ui, UiBuilder, Vec2, WidgetInfo, pos2, vec2,
};
use glint_core::clip::crop::{Aspect, Grip};
use glint_core::clip::math::fmt_clock;
use glint_core::clip::viewer;
use glint_ui::widgets::{self, Kind, Look, anim, ease, mix, mono, motion, sans};

use super::{ClipApp, LETTERBOX, Play};

/// The floating pills' glass: rgba(20, 20, 20, .75).
pub(super) const GLASS: Color32 = Color32::from_rgba_premultiplied(15, 15, 15, 191);
const ZOOM_SHOWN: f64 = 1.0;
const ZOOM_FADE: f64 = 0.3;

/// A small speaker, with waves when on and a cross when off.
pub(super) fn speaker(painter: &egui::Painter, c: Pos2, color: Color32, on: bool) {
    let body = vec![c + vec2(-6.0, -2.5), c + vec2(-3.5, -2.5), c + vec2(0.0, -6.0), c + vec2(0.0, 6.0), c + vec2(-3.5, 2.5), c + vec2(-6.0, 2.5)];
    painter.add(Shape::convex_polygon(body, color, Stroke::NONE));
    let stroke = Stroke::new(1.3, color);
    if on {
        for r in [3.5f32, 6.5] {
            let arc: Vec<Pos2> = (0..=8).map(|i| {
                let a = -0.9 + 1.8 * i as f32 / 8.0;
                c + vec2(1.0 + r * a.cos(), r * a.sin())
            }).collect();
            painter.add(Shape::line(arc, stroke));
        }
    } else {
        painter.line_segment([c + vec2(2.5, -3.0), c + vec2(8.0, 3.0)], stroke);
        painter.line_segment([c + vec2(2.5, 3.0), c + vec2(8.0, -3.0)], stroke);
    }
}

fn grip_point(g: Grip, r: Rect) -> Pos2 {
    let x = if g.left { r.left() } else if g.right { r.right() } else { r.center().x };
    let y = if g.top { r.top() } else if g.bottom { r.bottom() } else { r.center().y };
    pos2(x, y)
}

fn grip_cursor(g: Grip) -> CursorIcon {
    match (g.left, g.right, g.top, g.bottom) {
        (true, _, true, _) | (_, true, _, true) => CursorIcon::ResizeNwSe,
        (_, true, true, _) | (true, _, _, true) => CursorIcon::ResizeNeSw,
        (true, _, _, _) | (_, true, _, _) => CursorIcon::ResizeHorizontal,
        _ => CursorIcon::ResizeVertical,
    }
}

fn grip_name(g: Grip) -> &'static str {
    match (g.left, g.right, g.top, g.bottom) {
        (true, _, true, _) => "crop top left corner",
        (_, true, true, _) => "crop top right corner",
        (_, true, _, true) => "crop bottom right corner",
        (true, _, _, true) => "crop bottom left corner",
        (true, ..) => "crop left edge",
        (_, true, ..) => "crop right edge",
        (_, _, true, _) => "crop top edge",
        _ => "crop bottom edge",
    }
}

impl ClipApp {
    fn image_size(&self) -> Option<Vec2> {
        self.info().map(|i| vec2(i.size.0 as f32, i.size.1 as f32))
    }

    /// The frame's on-screen size in pixels at the zoom being headed to, and whether the zoom has settled.
    pub(super) fn decode_target(&self, ctx: &egui::Context, rect: Rect) -> (Vec2, bool) {
        let img = self.image_size().unwrap_or(rect.size());
        let scale = if self.viewer.fitted { viewer::fit(img, rect.size()).scale } else { self.viewer.target().scale };
        (img * scale * ctx.pixels_per_point(), !self.viewer.animating(self.now, motion()))
    }

    pub(super) fn preview(&mut self, ui: &mut Ui, look: &Look, rect: Rect) {
        let p = look.p;
        ui.painter().rect_filled(rect, 8, LETTERBOX);
        let resp = ui.interact(rect, Id::unique("clip-preview"), Sense::click());
        let problem = self.info.as_ref().err().or(self.player_error.as_ref()).cloned();
        if let Some(e) = problem {
            let text = format!("can't play {}: {e}", self.name);
            let galley = ui.painter().layout(text, sans(13.0), p.warn, rect.width() - 80.0);
            ui.painter().galley(rect.center() - galley.size() / 2.0, galley, p.warn);
            return;
        }
        let Some(img) = self.image_size() else { return };
        let ctx = ui.ctx().clone();
        let (ppp, now) = (ctx.pixels_per_point(), self.now);
        let place = self.viewer.place(now, img, rect.size(), motion(), ease);
        if self.viewer.animating(now, motion()) {
            ctx.request_repaint();
        }
        let frame = Rect::from_min_size(rect.min + place.offset, img * place.scale);
        let playing = self.play != Play::Paused;
        resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, if playing { "pause" } else { "play" }));

        let blocked = self.sheet || self.menu || self.confirm_delete;
        let hover = ui.input(|i| i.pointer.hover_pos()).filter(|at| rect.contains(*at));
        if let Some(at) = hover
            && !blocked
        {
            let z = ui.input(|i| i.zoom_delta());
            if z != 1.0 {
                self.viewer.zoom(now, place, at - rect.min, z, ppp, img);
                ctx.request_repaint();
            }
        }
        let (primary, middle, down, delta, origin) = ui.input(|i| {
            (
                i.pointer.button_pressed(PointerButton::Primary),
                i.pointer.button_pressed(PointerButton::Middle),
                i.pointer.primary_down() || i.pointer.middle_down(),
                i.pointer.delta(),
                i.pointer.press_origin(),
            )
        });
        let inside = origin.is_some_and(|o| rect.contains(o));
        if !self.panning && inside && !blocked && ((primary && self.space.held) || middle) {
            self.panning = true;
        }
        if self.panning {
            if down {
                if delta != Vec2::ZERO {
                    self.viewer.pan(delta, place, img);
                    ctx.request_repaint();
                }
                self.space.panned |= self.space.held;
            } else {
                self.panning = false;
            }
        }
        if self.panning {
            ctx.set_cursor_icon(CursorIcon::Grabbing);
        } else if self.space.held && hover.is_some() {
            ctx.set_cursor_icon(CursorIcon::Grab);
        }

        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        if let Some(tex) = &self.tex {
            painter.image(tex.id(), frame, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        self.set_nearest(place.scale * ppp > 1.001);

        let cropping = self.cropping.is_some();
        let crop_t = ease(anim(ui, Id::unique("clip-cropping"), cropping, 0.2));
        if let Some(c) = self.edit.crop {
            let cr = Rect::from_min_max(
                frame.min + vec2(c.x as f32, c.y as f32) * place.scale,
                frame.min + vec2((c.x + c.w) as f32, (c.y + c.h) as f32) * place.scale,
            );
            let dim = Color32::from_black_alpha((175.0 - 35.0 * crop_t) as u8);
            for r in [
                Rect::from_min_max(frame.min, pos2(frame.right(), cr.top())),
                Rect::from_min_max(pos2(frame.left(), cr.bottom()), frame.max),
                Rect::from_min_max(pos2(frame.left(), cr.top()), pos2(cr.left(), cr.bottom())),
                Rect::from_min_max(pos2(cr.right(), cr.top()), pos2(frame.right(), cr.bottom())),
            ] {
                if r.is_positive() {
                    painter.rect_filled(r, 0, dim);
                }
            }
            painter.rect_stroke(cr, 0, Stroke::new(1.0 + 0.5 * crop_t, Color32::WHITE.gamma_multiply(0.45 + 0.55 * crop_t)), StrokeKind::Outside);
            if crop_t > 0.0 {
                let thirds = Stroke::new(1.0, Color32::WHITE.gamma_multiply(0.22 * crop_t));
                for k in [1.0, 2.0] {
                    painter.vline(cr.left() + cr.width() * k / 3.0, cr.y_range(), thirds);
                    painter.hline(cr.x_range(), cr.top() + cr.height() * k / 3.0, thirds);
                }
            }
            if cropping {
                self.crop_handles(ui, cr, place.scale, &painter);
            }
        }

        if resp.double_clicked() {
            self.viewer.fit_view(now, place, img);
            if !cropping && !self.space.held {
                self.toggle_play();
            }
        } else if resp.clicked() && !self.space.held && !cropping {
            self.toggle_play();
        }

        let t = ease(anim(ui, Id::unique("clip-playing"), self.play != Play::Paused, 0.25));
        if t < 1.0 && !cropping {
            let (c, r, a) = (rect.center(), 32.0 * (1.0 + 0.3 * t), 1.0 - t);
            painter.circle_filled(c, r, Color32::from_black_alpha((140.0 * a) as u8));
            let k = r / 32.0;
            let tri = vec![c + vec2(-7.0, -12.0) * k, c + vec2(13.0, 0.0) * k, c + vec2(-7.0, 12.0) * k];
            painter.add(Shape::convex_polygon(tri, Color32::WHITE.gamma_multiply(a), Stroke::NONE));
        }

        self.transport(ui, look, rect);
        if cropping {
            self.crop_bar(ui, look, rect);
        } else {
            self.crop_button(ui, look, rect);
        }
        let since = now - self.viewer.changed_at;
        if since < ZOOM_SHOWN + ZOOM_FADE {
            let a = (1.0 - ((since - ZOOM_SHOWN) / ZOOM_FADE).clamp(0.0, 1.0)) as f32;
            let text = format!("{:.0} %", self.viewer.target().scale * ppp * 100.0);
            let g = ui.painter().layout_no_wrap(text, mono(12.0), p.t1);
            let pill = Rect::from_min_size(pos2(rect.right() - 12.0 - g.size().x - 24.0, rect.top() + 12.0), vec2(g.size().x + 24.0, 28.0));
            painter.rect_filled(pill, 14, GLASS.gamma_multiply(a));
            painter.galley(pill.center() - g.size() / 2.0, g, Color32::WHITE.gamma_multiply(a));
            ctx.request_repaint();
        }
    }

    fn crop_handles(&mut self, ui: &mut Ui, cr: Rect, scale: f32, painter: &egui::Painter) {
        let Some(size) = self.info().map(|i| i.size) else { return };
        let Some(cropping) = &self.cropping else { return };
        let ratio = cropping.aspect.ratio(size);
        let free = !self.space.held && !self.panning;
        let mover = ui
            .interact(cr, Id::unique("clip-crop-move"), Sense::drag())
            .on_hover_cursor(if free { CursorIcon::Move } else { CursorIcon::Grab });
        mover.widget_info(|| WidgetInfo::labeled(Role::Button, true, "move the crop"));
        let mut grabbed = mover.drag_started().then_some(Grip::MOVE);
        let grips = Grip::all();
        let mut hovered = Vec::new();
        for (i, g) in grips.iter().enumerate() {
            let at = grip_point(*g, cr);
            let r = ui
                .interact(Rect::from_center_size(at, vec2(20.0, 20.0)), Id::unique(("clip-crop-grip", i)), Sense::drag())
                .on_hover_cursor(grip_cursor(*g));
            r.widget_info(|| WidgetInfo::labeled(Role::Button, true, grip_name(*g)));
            if r.drag_started() {
                grabbed = Some(*g);
            }
            hovered.push(r.hovered() || r.dragged());
        }
        let pointer = ui.input(|i| i.pointer.interact_pos());
        if let (Some(g), Some(at), true) = (grabbed, pointer, free)
            && let Some(c) = self.edit.crop
        {
            self.before_drag = Some(self.edit.clone());
            if let Some(cr) = &mut self.cropping {
                cr.grab = Some((g, c, at));
            }
        }
        let grab = self.cropping.as_ref().and_then(|c| c.grab);
        if let Some((g, from, start)) = grab {
            if ui.input(|i| i.pointer.primary_down())
                && let Some(at) = pointer
            {
                let d = (at - start) / scale;
                self.edit.crop = Some(from.dragged(g, (d.x, d.y), ratio, size));
            } else {
                if let Some(cr) = &mut self.cropping {
                    cr.grab = None;
                }
                if let Some(before) = self.before_drag.take() {
                    self.undo.record(before, &self.edit);
                }
            }
        }
        for (g, lit) in grips.iter().zip(hovered) {
            let at = grip_point(*g, cr);
            let grow = if lit { 2.0 } else { 0.0 };
            let corner = (g.left || g.right) && (g.top || g.bottom);
            let s = if corner {
                vec2(10.0, 10.0)
            } else if g.left || g.right {
                vec2(6.0, 16.0)
            } else {
                vec2(16.0, 6.0)
            } + Vec2::splat(grow);
            let r = Rect::from_center_size(at, s);
            painter.rect(r, 2, Color32::WHITE, Stroke::new(1.0, Color32::from_black_alpha(120)), StrokeKind::Outside);
        }
    }

    fn crop_bar(&mut self, ui: &mut Ui, look: &Look, rect: Rect) {
        let Some(size) = self.info().map(|i| i.size) else { return };
        let Some(aspect) = self.cropping.as_ref().map(|c| c.aspect) else { return };
        let w = (rect.width() - 24.0).min(520.0);
        let bar = Rect::from_min_size(pos2(rect.center().x - w / 2.0, rect.top() + 12.0), vec2(w, 44.0));
        ui.interact(bar, Id::unique("clip-crop-bar"), Sense::click());
        ui.painter().rect(bar, 10, GLASS, Stroke::new(1.0, look.p.line2), StrokeKind::Inside);
        let mut child = ui.new_child(UiBuilder::new().max_rect(bar.shrink2(vec2(6.0, 6.0))).layout(Layout::left_to_right(Align::Center)));
        child.spacing_mut().item_spacing = vec2(8.0, 0.0);
        let label = self.edit.crop.map_or(String::new(), |c| {
            let c = c.even(size);
            format!("{}×{}", c.w, c.h)
        });
        let seg_w = (w - 12.0 - 8.0 * 2.0 - 84.0 - 64.0).max(120.0);
        let mut picked = aspect;
        let changed = widgets::segmented(&mut child, look, &mut picked, &Aspect::ALL, Some(seg_w)).changed();
        let (r, _) = child.allocate_exact_size(vec2(84.0, 28.0), Sense::hover());
        child.painter().text(r.center(), Align2::CENTER_CENTER, label, mono(12.0), Color32::WHITE.gamma_multiply(0.8));
        let done = widgets::button(&mut child, look, "done", Kind::Primary, 32.0, true).on_hover_text("enter or esc").clicked();
        if changed {
            if let Some(c) = &mut self.cropping {
                c.aspect = picked;
            }
            if let (Some(r), Some(c)) = (picked.ratio(size), self.edit.crop) {
                self.change(|e| {
                    e.crop = Some(c.with_aspect(r, size));
                    true
                });
            }
        }
        if done {
            self.toggle_crop();
        }
    }

    fn crop_button(&mut self, ui: &mut Ui, look: &Look, rect: Rect) {
        let g = ui.painter().layout_no_wrap("crop".into(), sans(12.0), look.p.t1);
        let pill = Rect::from_min_size(pos2(rect.right() - 12.0 - g.size().x - 40.0, rect.bottom() - 12.0 - 28.0), vec2(g.size().x + 40.0, 28.0));
        let resp = ui
            .interact(pill, Id::unique("clip-crop-button"), Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("crop the video (C)");
        resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "crop"));
        let h = anim(ui, resp.id.with("hov"), resp.hovered(), 0.12);
        let painter = ui.painter();
        painter.rect_filled(pill, 14, mix(GLASS, Color32::from_rgba_premultiplied(40, 40, 40, 220), h));
        let icon = pos2(pill.left() + 17.0, pill.center().y);
        let stroke = Stroke::new(1.4, Color32::WHITE.gamma_multiply(0.75 + 0.25 * h));
        painter.line_segment([icon + vec2(-3.0, -6.0), icon + vec2(-3.0, 3.0)], stroke);
        painter.line_segment([icon + vec2(-6.0, 3.0), icon + vec2(4.0, 3.0)], stroke);
        painter.line_segment([icon + vec2(-6.0, -3.0), icon + vec2(3.0, -3.0)], stroke);
        painter.line_segment([icon + vec2(3.0, -3.0), icon + vec2(3.0, 6.0)], stroke);
        painter.galley(pos2(pill.left() + 30.0, pill.center().y - g.size().y / 2.0), g, Color32::WHITE.gamma_multiply(0.75 + 0.25 * h));
        if resp.clicked() {
            self.toggle_crop();
        }
    }

    fn transport(&mut self, ui: &mut Ui, look: &Look, rect: Rect) {
        let p = look.p;
        let time = ui.painter().layout_no_wrap(fmt_clock(self.pos), mono(12.0), p.t2);
        let total = ui.painter().layout_no_wrap(format!("/ {}", fmt_clock(self.duration())), mono(12.0), p.t3);
        let sound = self.audio.is_some() || self.info().is_some_and(|i| !i.tracks.is_empty());
        let vol_w = if sound { 10.0 + 14.0 + 10.0 + 70.0 } else { 0.0 };
        let w = 12.0 + time.size().x + 10.0 + total.size().x + vol_w + 12.0;
        let pill = Rect::from_min_size(pos2(rect.left() + 12.0, rect.bottom() - 12.0 - 28.0), vec2(w, 28.0));
        ui.interact(pill, Id::unique("clip-transport"), Sense::click());
        let painter = ui.painter();
        painter.rect_filled(pill, 14, GLASS);
        let y = pill.center().y;
        let mut x = pill.left() + 12.0;
        let (tw, totw) = (time.size(), total.size());
        painter.galley(pos2(x, y - tw.y / 2.0), time, p.t2);
        x += tw.x + 10.0;
        painter.galley(pos2(x, y - totw.y / 2.0), total, p.t3);
        x += totw.x + 10.0;
        if !sound {
            return;
        }
        speaker(painter, pos2(x + 6.0, y), Color32::WHITE.gamma_multiply(0.7), self.saved.volume > 0.0);
        x += 24.0;
        let track = Rect::from_min_size(pos2(x, y - 8.0), vec2(70.0, 16.0));
        let resp = ui
            .interact(track, Id::unique("clip-volume"), Sense::click_and_drag())
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("volume (wheel adjusts)");
        let mut v = self.saved.volume;
        if (resp.dragged() || resp.clicked())
            && let Some(at) = resp.interact_pointer_pos()
        {
            v = ((at.x - track.left()) / track.width()).clamp(0.0, 1.0);
        }
        if resp.hovered() && !ui.input(|i| i.modifiers.ctrl) {
            let wheel = ui.input(|i| i.smooth_scroll_delta.y);
            v = (v + wheel / 600.0).clamp(0.0, 1.0);
        }
        if v != self.saved.volume {
            self.saved.volume = v;
            self.sync_gains();
        }
        resp.widget_info(|| WidgetInfo::slider(true, (v * 100.0).round() as f64, "volume"));
        let painter = ui.painter();
        let line = Rect::from_min_max(pos2(track.left(), y - 1.5), pos2(track.right(), y + 1.5));
        painter.rect_filled(line, 2, Color32::from_white_alpha(50));
        let kx = track.left() + track.width() * v;
        painter.rect_filled(Rect::from_min_max(line.min, pos2(kx, line.bottom())), 2, p.acc);
        let lit = anim(ui, resp.id.with("hov"), resp.hovered() || resp.dragged(), 0.1);
        painter.circle_filled(pos2(kx, y), 4.5 + 1.0 * lit, Color32::WHITE);
    }
}

