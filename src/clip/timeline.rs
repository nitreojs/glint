use eframe::egui::{
    self, Color32, CornerRadius, CursorIcon, Id, Painter, Rect, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2,
};

use super::edit::snap_to;
use super::math::{self, SECOND};
use super::{ClipApp, Selected, scan};
use crate::settings::widgets::{self, Look};

const TRACK_H: f32 = 36.0;
const LANE_H: f32 = 14.0;
const SCROLL_H: f32 = 6.0;
const SNAP_PX: f64 = 8.0;

pub(super) fn height() -> f32 {
    LANE_H + TRACK_H + 4.0 + SCROLL_H
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Drag {
    Start { head: i64 },
    End { head: i64 },
    Head,
    Scroll { grab: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct View {
    pub start: i64,
    pub span: i64,
}

impl View {
    pub(super) fn full(duration: i64) -> View {
        View { start: 0, span: duration.max(1) }
    }

    fn clamp(&mut self, duration: i64, min_span: i64) {
        let duration = duration.max(1);
        self.span = self.span.clamp(min_span.clamp(1, duration), duration);
        self.start = self.start.clamp(0, duration - self.span);
    }

    pub(super) fn reveal(&mut self, t: i64, duration: i64) {
        if t < self.start || t > self.start + self.span {
            self.start = t - self.span / 2;
            self.clamp(duration, 1);
        }
    }

    pub(super) fn follow(&mut self, t: i64, duration: i64) {
        if t < self.start || t > self.start + self.span {
            self.start = t - self.span / 10;
            self.clamp(duration, 1);
        }
    }

    fn zoom(&mut self, anchor: i64, frac: f64, factor: f64, duration: i64, min_span: i64) {
        self.span = (self.span as f64 / factor) as i64;
        self.clamp(duration, min_span);
        self.start = anchor - (frac * self.span as f64) as i64;
        self.clamp(duration, min_span);
    }
}

fn hatch(painter: &Painter, r: Rect, color: Color32) {
    let painter = painter.with_clip_rect(r.intersect(painter.clip_rect()));
    let mut x = r.left() - r.height();
    while x < r.right() {
        painter.line_segment([pos2(x, r.bottom()), pos2(x + r.height(), r.top())], Stroke::new(1.0, color));
        x += 7.0;
    }
}

impl ClipApp {
    fn snapped(&self, t: i64, extra: &[i64], within: i64, on: bool) -> i64 {
        if !on {
            return t;
        }
        let targets = (self.edit.markers.iter().copied())
            .chain(self.cuts.iter().copied())
            .chain(self.edit.splits())
            .chain([0, self.duration()])
            .chain(extra.iter().copied());
        snap_to(t, targets, within)
    }

    pub(super) fn timeline(&mut self, ui: &mut Ui, look: &Look, area: Rect) {
        let p = look.p;
        let lane = Rect::from_min_size(area.min, vec2(area.width(), LANE_H));
        let track = Rect::from_min_size(pos2(area.left(), lane.bottom()), vec2(area.width(), TRACK_H));
        let bar = Rect::from_min_size(pos2(area.left(), track.bottom() + 4.0), vec2(area.width(), SCROLL_H));
        let Some(info) = self.info().cloned() else {
            ui.painter().rect(track, 6, p.field, Stroke::new(1.0, p.line2), StrokeKind::Inside);
            return;
        };
        let dur = info.duration.max(1);
        let frame = math::frame_len(info.fps);
        let inner = track.shrink2(vec2(6.0, 0.0));
        let min_span = (frame * 12).max(SECOND / 5);
        self.view.clamp(dur, min_span);

        let hover = ui.input(|i| i.pointer.hover_pos()).filter(|at| area.contains(*at));
        if let Some(at) = hover
            && !self.gif_dialog
            && !self.confirm_delete
        {
            let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta()));
            if zoom != 1.0 {
                let frac = ((at.x - inner.left()) / inner.width()).clamp(0.0, 1.0) as f64;
                let anchor = self.view.start + (frac * self.view.span as f64) as i64;
                self.view.zoom(anchor, frac, zoom as f64, dur, min_span);
            } else if self.view.span < dur && scroll.x + scroll.y != 0.0 {
                let shift = (scroll.x + scroll.y) as f64 / inner.width() as f64 * self.view.span as f64;
                self.view.start -= shift as i64;
                self.view.clamp(dur, min_span);
            }
        }

        let v = self.view;
        let x_of = move |t: i64| inner.left() + inner.width() * ((t - v.start) as f64 / v.span as f64) as f32;
        let t_of = move |x: f32| {
            let t = v.start + (((x - inner.left()) / inner.width()) as f64 * v.span as f64) as i64;
            t.clamp(0, dur)
        };
        let visible = move |x: f32| x >= inner.left() - 6.0 && x <= inner.right() + 6.0;
        let within = (SNAP_PX / inner.width() as f64 * v.span as f64) as i64;
        let snapping = !ui.input(|i| i.modifiers.ctrl);
        let zoomed = v.span < dur;

        let body = ui.interact(track.union(lane), Id::unique("clip-track"), Sense::click_and_drag());
        let markers: Vec<(usize, egui::Response)> = (self.edit.markers.iter().copied().enumerate())
            .filter(|(_, m)| visible(x_of(*m)))
            .map(|(i, m)| {
                let r = Rect::from_center_size(pos2(x_of(m), lane.center().y), vec2(12.0, LANE_H));
                let resp = ui.interact(r, Id::unique(("clip-marker", i)), Sense::click());
                (i, resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("click jumps here · right-click removes"))
            })
            .collect();
        let handle = |t: i64| Rect::from_center_size(pos2(x_of(t), track.center().y), vec2(12.0, TRACK_H + 4.0));
        let (hs, he) = (handle(self.edit.range.start), handle(self.edit.range.end));
        let grip = |r: Rect, id: &str| {
            visible(r.center().x)
                .then(|| ui.interact(r, Id::unique(id), Sense::drag()).on_hover_cursor(CursorIcon::ResizeHorizontal))
        };
        let (start, end) = (grip(hs, "clip-in"), grip(he, "clip-out"));
        let left = bar.left() + bar.width() * (v.start as f32 / dur as f32);
        let thumb = Rect::from_min_max(
            pos2(left, bar.top()),
            pos2((left + bar.width() * (v.span as f32 / dur as f32)).max(left + 12.0), bar.bottom()),
        );
        let scroll = zoomed.then(|| ui.interact(bar.expand2(vec2(0.0, 3.0)), Id::unique("clip-scroll"), Sense::drag()));

        let mut removed = None;
        for (i, r) in &markers {
            if r.clicked() {
                self.selected = Some(Selected::Marker(*i));
                self.jump(self.edit.markers[*i]);
            }
            if r.secondary_clicked() {
                removed = Some(*i);
            }
        }
        if let Some(i) = removed {
            self.change(|e| {
                e.markers.remove(i);
                true
            });
            self.selected = None;
        }

        let head = self.pos;
        let pressed = |r: &Option<egui::Response>| r.as_ref().is_some_and(|r| r.drag_started());
        if pressed(&start) {
            self.drag = Some(Drag::Start { head });
            self.before_drag = Some(self.edit.clone());
        } else if pressed(&end) {
            self.drag = Some(Drag::End { head });
            self.before_drag = Some(self.edit.clone());
        } else if let Some(s) = &scroll
            && s.drag_started()
            && let Some(at) = s.interact_pointer_pos()
        {
            let grab = if thumb.x_range().contains(at.x) { at.x - thumb.left() } else { thumb.width() / 2.0 };
            self.drag = Some(Drag::Scroll { grab });
        } else if body.drag_started() || body.clicked() {
            self.drag = Some(Drag::Head);
            if body.clicked()
                && let Some(at) = body.interact_pointer_pos()
            {
                let many = self.edit.segments.len() > 1 && track.contains(at);
                self.selected = many.then(|| Selected::Segment(self.edit.segment_at(t_of(at.x))));
            }
        }

        if let (Some(drag), Some(at)) = (self.drag, ui.input(|i| i.pointer.interact_pos())) {
            let raw = math::snap(t_of(at.x), info.fps);
            match drag {
                Drag::Start { head } => {
                    let t = self.snapped(raw, &[head], within, snapping);
                    let before = self.edit.range.start;
                    self.edit.range.set_start(t, frame);
                    if self.edit.range.start != before || self.pos != self.edit.range.start {
                        self.pos = self.edit.range.start;
                        self.seek(self.pos, false);
                    }
                }
                Drag::End { head } => {
                    let t = self.snapped(raw, &[head], within, snapping);
                    let before = self.edit.range.end;
                    self.edit.range.set_end(t, info.duration, frame);
                    if self.edit.range.end != before || self.pos != self.edit.range.end - 1 {
                        self.pos = self.edit.range.end - 1;
                        self.seek(self.pos, false);
                    }
                }
                Drag::Head => {
                    let range = [self.edit.range.start, self.edit.range.end];
                    let t = self.snapped(raw, &range, within, snapping).min(info.duration - 1);
                    if t != self.pos {
                        self.pos = t;
                        self.seek(t, false);
                    }
                }
                Drag::Scroll { grab } => {
                    self.view.start = ((at.x - grab - bar.left()) / bar.width() * dur as f32) as i64;
                    self.view.clamp(dur, min_span);
                }
            }
        }
        if self.drag.is_some() && !ui.input(|i| i.pointer.any_down()) {
            if let Some(before) = self.before_drag.take() {
                self.undo.record(before, &self.edit);
            }
            self.drag = None;
        }

        let painter = ui.painter().clone();
        painter.rect(track, 6, p.field, Stroke::new(1.0, p.line2), StrokeKind::Inside);
        let clip = painter.with_clip_rect(track.shrink(1.0));
        let (x_start, x_end) = (x_of(0), x_of(dur));
        if let Some((_, first)) = self.thumbs.first() {
            let [tw, th] = first.size();
            let h = track.height() - 2.0;
            let w = (h * tw as f32 / th.max(1) as f32).max(8.0);
            let every = scan::interval(dur);
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            let tint = Color32::WHITE.gamma_multiply(if self.light == Some(true) { 0.7 } else { 0.45 });
            let mut x = x_start.max(inner.left() - w);
            x -= (x - x_start).rem_euclid(w);
            while x < inner.right().min(x_end) {
                let t = t_of(x + w / 2.0);
                let i = self.thumbs.partition_point(|(ts, _)| *ts <= t).saturating_sub(1);
                let (ts, tex) = &self.thumbs[i];
                if t - ts <= 2 * every {
                    clip.image(tex.id(), Rect::from_min_size(pos2(x, track.top() + 1.0), vec2(w, h)), uv, tint);
                }
                x += w;
            }
        }
        let shade = Color32::from_black_alpha(if self.light == Some(true) { 40 } else { 90 });
        let span_rect = |a: i64, b: i64| Rect::from_min_max(pos2(x_of(a), track.top()), pos2(x_of(b), track.bottom()));
        for s in self.edit.segments.iter().filter(|s| s.removed) {
            let r = span_rect(s.start, s.end);
            clip.rect_filled(r, 0, shade);
            clip.rect_filled(r, 0, p.surf.gamma_multiply(0.55));
            hatch(&clip, r, p.t4.gamma_multiply(0.6));
        }
        let (x0, x1) = (x_of(self.edit.range.start), x_of(self.edit.range.end));
        clip.rect_filled(Rect::from_min_max(pos2(inner.left() - 6.0, track.top()), pos2(x0, track.bottom())), 0, shade);
        clip.rect_filled(Rect::from_min_max(pos2(x1, track.top()), pos2(inner.right() + 6.0, track.bottom())), 0, shade);
        clip.rect_filled(span_rect(self.edit.range.start, self.edit.range.end), 0, p.acc.gamma_multiply(0.10));
        clip.hline(x0..=x1, track.top() + 1.0, Stroke::new(2.0, p.acc));
        clip.hline(x0..=x1, track.bottom() - 1.0, Stroke::new(2.0, p.acc));
        for t in self.edit.splits() {
            let x = x_of(t);
            clip.vline(x, track.y_range(), Stroke::new(3.0, p.surf));
            clip.vline(x, track.y_range(), Stroke::new(1.0, p.t2));
        }
        if let Some(Selected::Segment(i)) = self.selected
            && let Some(s) = self.edit.segments.get(i)
        {
            let r = span_rect(s.start, s.end).shrink2(vec2(1.5, 1.5));
            clip.rect_stroke(r, 3, Stroke::new(2.0, p.t1), StrokeKind::Inside);
        }

        let lane_painter = painter.with_clip_rect(lane.union(track).expand2(vec2(0.0, 6.0)).intersect(painter.clip_rect()));
        for &t in &self.cuts {
            let x = x_of(t);
            if visible(x) {
                lane_painter.vline(x, lane.bottom() - 4.0..=lane.bottom(), Stroke::new(1.0, p.t4));
            }
        }
        for (i, resp) in &markers {
            let x = x_of(self.edit.markers[*i]);
            let chosen = self.selected == Some(Selected::Marker(*i));
            let color = if chosen { p.acc } else if resp.hovered() { p.t1 } else { p.t2 };
            clip.vline(x, track.y_range(), Stroke::new(1.0, color.gamma_multiply(0.7)));
            let tri = vec![pos2(x - 5.0, lane.top() + 2.0), pos2(x + 5.0, lane.top() + 2.0), pos2(x, lane.bottom() - 2.0)];
            lane_painter.add(Shape::convex_polygon(tri, color, Stroke::NONE));
        }
        for (r, resp) in [(hs, &start), (he, &end)] {
            let Some(resp) = resp else { continue };
            let h = widgets::anim(ui, resp.id.with("hov"), resp.hovered() || resp.dragged(), 0.08);
            let knob = r.shrink2(vec2(2.0 - h, 0.0));
            painter.rect_filled(knob, 3, if h > 0.0 { p.acch } else { p.acc });
            let c = knob.center();
            painter.vline(c.x, c.y - 6.0..=c.y + 6.0, Stroke::new(1.5, Color32::WHITE.gamma_multiply(0.85)));
        }
        let x = x_of(self.pos);
        if visible(x) {
            painter.vline(x, track.top() - 4.0..=track.bottom() + 4.0, Stroke::new(2.0, p.t1));
            painter.circle_filled(pos2(x, track.top() - 4.0), 4.0, p.t1);
        }

        if let Some(s) = &scroll {
            painter.rect_filled(bar, CornerRadius::same(3), p.field);
            let lit = s.hovered() || matches!(self.drag, Some(Drag::Scroll { .. }));
            painter.rect_filled(thumb, CornerRadius::same(3), if lit { p.t3 } else { p.line3 });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let dur = 100 * SECOND;
        let mut v = View::full(dur);
        v.zoom(50 * SECOND, 0.5, 4.0, dur, SECOND);
        assert_eq!(v, View { start: 375 * SECOND / 10, span: 25 * SECOND });
        v.zoom(40 * SECOND, 0.1, 1000.0, dur, SECOND);
        assert_eq!(v, View { start: 399 * SECOND / 10, span: SECOND });
        v.zoom(0, 0.0, 0.001, dur, SECOND);
        assert_eq!(v, View::full(dur));
    }

    #[test]
    fn the_view_follows_and_reveals_the_playhead_inside_the_clip() {
        let dur = 100 * SECOND;
        let mut v = View { start: 0, span: 10 * SECOND };
        v.follow(5 * SECOND, dur);
        assert_eq!(v.start, 0);
        v.follow(11 * SECOND, dur);
        assert_eq!(v.start, 10 * SECOND);
        v.reveal(99 * SECOND, dur);
        assert_eq!(v.start, 90 * SECOND);
        v.reveal(SECOND, dur);
        assert_eq!(v.start, 0);
    }
}
