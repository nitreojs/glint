use eframe::egui::{
    self, Color32, CornerRadius, CursorIcon, Id, Mesh, Painter, Rect, Role, Sense, Shape, Stroke, StrokeKind, Ui,
    WidgetInfo, pos2, vec2,
};

use super::edit::snap_to;
use super::export::{self, Job, Video};
use super::math::{self, SECOND};
use super::preview::speaker;
use super::wave::BUCKET;
use super::{ClipApp, Selected, scan};
use crate::settings::widgets::{Look, anim, ease, medium, mix, sans};

const TOP: f32 = 18.0;
const VIDEO_H: f32 = 52.0;
const GAP: f32 = 6.0;
const HEAD_H: f32 = 18.0;
const LANE_MAX: f32 = 34.0;
const LANE_MIN: f32 = 22.0;
const SCROLL_H: f32 = 10.0;
const SNAP_PX: f64 = 8.0;
const MARKER: Color32 = Color32::from_rgb(0xF5, 0xC0, 0x4A);
const WAVES: [Color32; 6] = [
    Color32::from_rgb(0x6A, 0xA7, 0xE8),
    Color32::from_rgb(0x9A, 0xD1, 0x7A),
    Color32::from_rgb(0xD6, 0x8A, 0xE0),
    Color32::from_rgb(0xE8, 0xA8, 0x6A),
    Color32::from_rgb(0x6A, 0xD1, 0xC4),
    Color32::from_rgb(0xE0, 0xD4, 0x6A),
];

/// The timeline's height with `tracks` sound lanes `lane` tall.
pub(super) fn height(tracks: usize, lane: f32) -> f32 {
    let sound = if tracks > 0 { GAP + HEAD_H + GAP + tracks as f32 * (lane + GAP) - GAP } else { 0.0 };
    TOP + VIDEO_H + sound + SCROLL_H
}

/// Sound lanes shrink from 34 to 22 px to fit in `room`.
pub(super) fn lane_height(tracks: usize, room: f32) -> f32 {
    if tracks == 0 {
        return LANE_MAX;
    }
    ((room - height(tracks, 0.0)) / tracks as f32).clamp(LANE_MIN, LANE_MAX)
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

struct Rows {
    video: Rect,
    head: Rect,
    lanes: Vec<Rect>,
    span: Rect,
    bar: Rect,
}

fn rows(area: Rect, tracks: usize, lane: f32) -> Rows {
    let video = Rect::from_min_size(pos2(area.left(), area.top() + TOP), vec2(area.width(), VIDEO_H));
    let head = Rect::from_min_size(pos2(area.left(), video.bottom() + GAP), vec2(area.width(), HEAD_H));
    let lanes: Vec<Rect> = (0..tracks)
        .map(|k| Rect::from_min_size(pos2(area.left(), head.bottom() + GAP + k as f32 * (lane + GAP)), vec2(area.width(), lane)))
        .collect();
    let bottom = lanes.last().map_or(video.bottom(), |l| l.bottom());
    let span = Rect::from_min_max(video.min, pos2(area.right(), bottom));
    let bar = Rect::from_min_size(pos2(area.left() + 6.0, bottom + 4.0), vec2(area.width() - 12.0, 4.0));
    Rows { video, head, lanes, span, bar }
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

    pub(super) fn timeline(&mut self, ui: &mut Ui, look: &Look, area: Rect, lane_h: f32) {
        let p = look.p;
        let Some(info) = self.info().cloned() else {
            ui.painter().rect_filled(Rect::from_min_size(area.min + vec2(0.0, TOP), vec2(area.width(), VIDEO_H)), 6, p.ctl);
            return;
        };
        let rows = rows(area, info.tracks.len(), lane_h);
        let dur = info.duration.max(1);
        let frame = math::frame_len(info.fps);
        let inner = area.shrink2(vec2(6.0, 0.0));
        let min_span = (frame * 12).max(SECOND / 5);
        self.view.clamp(dur, min_span);
        let light = self.light == Some(true);

        let blocked = self.sheet || self.menu || self.confirm_delete;
        let hover = ui.input(|i| i.pointer.hover_pos()).filter(|at| area.contains(*at));
        if let Some(at) = hover
            && !blocked
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
        let t_raw = move |x: f32| v.start + (((x - inner.left()) / inner.width()) as f64 * v.span as f64) as i64;
        let t_of = move |x: f32| t_raw(x).clamp(0, dur);
        let visible = move |x: f32| x >= inner.left() - 6.0 && x <= inner.right() + 6.0;
        let within = (SNAP_PX / inner.width() as f64 * v.span as f64) as i64;
        let snapping = !ui.input(|i| i.modifiers.ctrl);
        let zoomed = v.span < dur;
        let pieces = match self.job() {
            Some(Job::Video(j)) if j.video == Video::Copy => Some(j.pieces),
            _ => None,
        };
        let keys: Vec<i64> = if pieces.is_some() { self.index.as_ref().map_or(Vec::new(), |i| i.video.keys.clone()) } else { Vec::new() };

        let body = ui.interact(Rect::from_min_max(area.min, rows.span.max), Id::unique("clip-track"), Sense::click_and_drag());
        let markers: Vec<(usize, egui::Response)> = (self.edit.markers.iter().copied().enumerate())
            .filter(|(_, m)| visible(x_of(*m)))
            .map(|(i, m)| {
                let r = Rect::from_center_size(pos2(x_of(m), area.top() + 7.0), vec2(14.0, 14.0));
                let resp = ui.interact(r, Id::unique(("clip-marker", i)), Sense::click());
                (i, resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("click jumps here · right-click removes"))
            })
            .collect();
        let chips: Vec<(Rect, egui::Response)> = (info.tracks.iter().enumerate())
            .map(|(k, t)| {
                let name = ui.painter().layout_no_wrap(t.name.clone(), medium(12.0), p.t1);
                let extra = if self.edit.muted[k] {
                    ui.painter().layout_no_wrap("· muted, not exported".into(), sans(12.0), p.t3).size().x + 6.0
                } else {
                    0.0
                };
                let l = rows.lanes[k];
                let h = (l.height() - 6.0).min(26.0);
                let chip = Rect::from_min_size(pos2(l.left() + 8.0, l.center().y - h / 2.0), vec2(h + 6.0 + name.size().x + extra + 8.0, h));
                let tip = if self.edit.muted[k] { "muted: not in the file · click to unmute" } else { "click to mute: muted sound isn't saved" };
                let resp = ui
                    .interact(chip, Id::unique(("clip-lane-mute", k)), Sense::click())
                    .on_hover_cursor(CursorIcon::PointingHand)
                    .on_hover_text(tip);
                resp.widget_info(|| WidgetInfo::selected(Role::CheckBox, true, !self.edit.muted[k], format!("sound: {}", t.name)));
                (chip, resp)
            })
            .collect();
        let all_muted = self.edit.muted.iter().all(|&m| m);
        let link_text = if all_muted { "unmute all" } else { "mute all" };
        let link = (!info.tracks.is_empty()).then(|| {
            let g = ui.painter().layout_no_wrap(link_text.into(), sans(12.0), p.t2);
            let r = Rect::from_min_size(pos2(rows.head.right() - g.size().x - 2.0, rows.head.center().y - 9.0), vec2(g.size().x + 2.0, 18.0));
            let resp = ui.interact(r, Id::unique("clip-mute-all"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
            resp.widget_info(|| WidgetInfo::labeled(Role::Link, true, link_text));
            (r, resp)
        });
        let handle = |t: i64| Rect::from_center_size(pos2(x_of(t), rows.span.center().y), vec2(14.0, rows.span.height() + 8.0));
        let (hs, he) = (handle(self.edit.range.start), handle(self.edit.range.end));
        let start_tip = if pieces.is_some() {
            "drag to trim · snaps to keyframes so it saves instantly · ctrl + drag for an exact frame (the file then starts at the keyframe before it)"
        } else {
            "drag to trim · ctrl + drag: no snapping"
        };
        let grip = |r: Rect, id: &str, tip: &str| {
            visible(r.center().x).then(|| {
                ui.interact(r, Id::unique(id), Sense::drag()).on_hover_cursor(CursorIcon::ResizeHorizontal).on_hover_text(tip)
            })
        };
        let (start, end) = (grip(hs, "clip-in", start_tip), grip(he, "clip-out", "drag to trim · ctrl + drag: no snapping"));
        let left = rows.bar.left() + rows.bar.width() * (v.start as f32 / dur as f32);
        let thumb = Rect::from_min_max(
            pos2(left, rows.bar.top()),
            pos2((left + rows.bar.width() * (v.span as f32 / dur as f32)).max(left + 12.0), rows.bar.bottom()),
        );
        let scroll = zoomed.then(|| ui.interact(rows.bar.expand2(vec2(0.0, 4.0)), Id::unique("clip-scroll"), Sense::drag()));

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
        for (k, (_, r)) in chips.iter().enumerate() {
            if r.clicked() {
                self.set_muted(k, !self.edit.muted[k]);
            }
        }
        if link.as_ref().is_some_and(|(_, r)| r.clicked()) {
            self.change(|e| {
                e.muted.iter_mut().for_each(|m| *m = !all_muted);
                true
            });
            self.sync_gains();
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
                let many = self.edit.segments.len() > 1 && rows.span.contains(at);
                self.selected = many.then(|| Selected::Segment(self.edit.segment_at(t_of(at.x))));
            }
        }

        if let (Some(drag), Some(at)) = (self.drag, ui.input(|i| i.pointer.interact_pos())) {
            let raw = math::snap(t_of(at.x), info.fps);
            match drag {
                Drag::Start { head } => {
                    let t = match export::nearest_key(&keys, raw) {
                        Some(k) if snapping => k,
                        _ => self.snapped(raw, &[head], within, snapping),
                    };
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
                    self.view.start = ((at.x - grab - rows.bar.left()) / rows.bar.width() * dur as f32) as i64;
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
        let timed = |role: Role, label: &str, t: i64| WidgetInfo {
            current_text_value: Some(math::fmt_time(t)),
            value: Some(t as f64 / SECOND as f64),
            ..WidgetInfo::labeled(role, true, label)
        };
        body.widget_info(|| timed(Role::Slider, "playhead", self.pos));
        if let Some(r) = &start {
            r.widget_info(|| timed(Role::Slider, "trim start", self.edit.range.start));
        }
        if let Some(r) = &end {
            r.widget_info(|| timed(Role::Slider, "trim end", self.edit.range.end));
        }
        for (i, r) in &markers {
            if let Some(&t) = self.edit.markers.get(*i) {
                r.widget_info(|| timed(Role::Button, "marker", t));
            }
        }
        if let Some(s) = &scroll {
            s.widget_info(|| WidgetInfo::slider(true, self.view.start as f64 / SECOND as f64, "timeline scroll"));
        }

        let painter = ui.painter().with_clip_rect(area.expand2(vec2(0.0, 4.0)).intersect(ui.clip_rect()));
        let lane_clip = |r: Rect| painter.with_clip_rect(r.intersect(painter.clip_rect()));

        painter.rect_filled(rows.video, 6, p.ctl);
        if let Some((_, first)) = self.thumbs.first() {
            let clip = lane_clip(rows.video.shrink(1.0));
            let [tw, th] = first.size();
            let h = rows.video.height() - 2.0;
            let w = (h * tw as f32 / th.max(1) as f32).max(8.0);
            let every = scan::interval(dur);
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            let tint = Color32::WHITE.gamma_multiply(if light { 0.95 } else { 0.85 });
            let (x_start, x_end) = (x_of(0), x_of(dur));
            let mut x = x_start.max(inner.left() - w);
            x -= (x - x_start).rem_euclid(w);
            while x < inner.right().min(x_end) {
                let t = t_of(x + w / 2.0);
                let i = self.thumbs.partition_point(|(ts, _)| *ts <= t).saturating_sub(1);
                let (ts, tex) = &self.thumbs[i];
                if t - ts <= 2 * every {
                    let r = Rect::from_min_size(pos2(x, rows.video.top() + 1.0), vec2(w, h));
                    clip.image(tex.id(), r, uv, tint);
                    clip.vline(r.right(), r.y_range(), Stroke::new(1.0, Color32::from_black_alpha(90)));
                }
                x += w;
            }
        }
        for &t in &self.cuts {
            let x = x_of(t);
            if visible(x) {
                painter.vline(x, rows.video.top()..=rows.video.top() + 4.0, Stroke::new(1.0, p.t3));
            }
        }
        if keys.len() > 1 && inner.width() / (keys.len() as f32 * v.span as f32 / dur as f32) > 4.0 {
            for &k in &keys {
                let x = x_of(k);
                if visible(x) {
                    painter.vline(x, rows.video.bottom() - 4.0..=rows.video.bottom(), Stroke::new(1.0, Color32::WHITE.gamma_multiply(0.55)));
                }
            }
        }

        if !info.tracks.is_empty() {
            painter.text(rows.head.left_center(), egui::Align2::LEFT_CENTER, "sound · muted tracks won't be in the file", sans(12.0), p.t3);
        }
        let mut fades = Vec::with_capacity(rows.lanes.len());
        for (k, l) in rows.lanes.iter().enumerate() {
            let muted = self.edit.muted[k];
            let fade = 1.0 - 0.62 * ease(anim(ui, Id::unique(("clip-lane-fade", k)), muted, 0.3));
            fades.push(fade);
            let clip = lane_clip(*l);
            clip.rect_filled(*l, 6, p.ctl.gamma_multiply(fade));
            let peaks = &self.peaks[k];
            if peaks.is_empty() {
                continue;
            }
            let color = WAVES[k % WAVES.len()].gamma_multiply(0.75 * fade);
            let mut mesh = Mesh::default();
            let (cy, room) = (l.center().y, l.height() * 0.8);
            let mut x = inner.left().max(x_of(0));
            let stop = inner.right().min(x_of(dur));
            while x < stop {
                let (b0, b1) = ((t_raw(x).max(0) / BUCKET) as usize, (t_raw(x + 2.0).max(0) / BUCKET) as usize);
                if b0 >= peaks.len() {
                    break;
                }
                let peak = peaks[b0..b1.clamp(b0 + 1, peaks.len())].iter().copied().max().unwrap_or(0);
                let h = (peak as f32 / 255.0 * room).max(1.0);
                mesh.add_colored_rect(Rect::from_min_size(pos2(x, cy - h / 2.0), vec2(1.5, h)), color);
                x += 2.0;
            }
            clip.add(Shape::mesh(mesh));
        }

        let shade = Color32::from_black_alpha(if light { 70 } else { 158 });
        let span_rect = |a: i64, b: i64| Rect::from_min_max(pos2(x_of(a), rows.span.top()), pos2(x_of(b), rows.span.bottom()));
        let full_clip = lane_clip(Rect::from_min_max(pos2(area.left(), rows.span.top()), pos2(area.right(), rows.span.bottom())));
        for s in self.edit.segments.iter().filter(|s| s.removed) {
            let r = span_rect(s.start, s.end);
            full_clip.rect_filled(r, 0, shade);
            hatch(&full_clip, r, p.t3.gamma_multiply(0.6));
        }
        let (x0, x1) = (x_of(self.edit.range.start), x_of(self.edit.range.end));
        full_clip.rect_filled(Rect::from_min_max(pos2(area.left(), rows.span.top()), pos2(x0, rows.span.bottom())), 0, shade);
        full_clip.rect_filled(Rect::from_min_max(pos2(x1, rows.span.top()), pos2(area.right(), rows.span.bottom())), 0, shade);
        if let Some(pieces) = &pieces {
            for pc in pieces.iter().filter(|pc| pc.lead < pc.start) {
                let r = span_rect(pc.lead, pc.start);
                full_clip.rect_filled(r, 0, p.surf.gamma_multiply(0.35));
                hatch(&full_clip, r, p.acc.gamma_multiply(0.5));
            }
        }
        if let Some(Selected::Segment(i)) = self.selected
            && let Some(s) = self.edit.segments.get(i)
        {
            let r = span_rect(s.start, s.end).shrink2(vec2(1.5, 1.5));
            full_clip.rect_stroke(r, 3, Stroke::new(2.0, p.t1), StrokeKind::Inside);
        }
        for t in self.edit.splits() {
            let x = x_of(t);
            if visible(x) {
                full_clip.vline(x, rows.span.y_range(), Stroke::new(1.0, p.t2));
            }
        }

        for (k, (chip, resp)) in chips.iter().enumerate() {
            let fade = fades[k];
            let muted = self.edit.muted[k];
            let clip = lane_clip(rows.lanes[k]);
            clip.rect_filled(*chip, 5, p.ctl.gamma_multiply(0.85 * fade.max(0.6)));
            let button = Rect::from_min_size(chip.min, vec2(chip.height(), chip.height()));
            let h = anim(ui, resp.id.with("hov"), resp.hovered(), 0.12);
            clip.rect_filled(button, 5, p.line2.gamma_multiply(h));
            speaker(&clip, button.center() - vec2(1.0, 0.0), p.t1.gamma_multiply(fade.max(0.7)), !muted);
            let name = &info.tracks[k].name;
            let g = clip.layout_no_wrap(name.clone(), medium(12.0), p.t1.gamma_multiply(fade.max(0.55)));
            let at = pos2(button.right() + 6.0, chip.center().y - g.size().y / 2.0);
            let nw = g.size().x;
            clip.galley(at, g, p.t1);
            if muted {
                clip.text(pos2(at.x + nw + 6.0, chip.center().y), egui::Align2::LEFT_CENTER, "· muted, not exported", sans(12.0), p.t3);
            }
        }
        if let Some((r, resp)) = &link {
            let h = anim(ui, resp.id.with("hov"), resp.hovered(), 0.12);
            let color = mix(p.t2, p.t1, h);
            painter.text(pos2(r.right(), r.center().y), egui::Align2::RIGHT_CENTER, link_text, sans(12.0), color);
            let y = r.bottom() - 2.0;
            let mut x = r.left() + 1.0;
            while x < r.right() {
                painter.hline(x..=(x + 1.0).min(r.right()), y, Stroke::new(1.0, p.t3));
                x += 3.0;
            }
        }

        for (i, resp) in &markers {
            let x = x_of(self.edit.markers[*i]);
            let chosen = self.selected == Some(Selected::Marker(*i));
            let c = pos2(x, area.top() + 7.0);
            painter.circle_filled(c, if resp.hovered() { 6.0 } else { 5.0 }, MARKER);
            if chosen {
                painter.circle_stroke(c, 7.5, Stroke::new(1.5, p.t1));
            }
            painter.vline(x, rows.video.y_range(), Stroke::new(1.0, MARKER.gamma_multiply(0.45)));
        }
        for (r, resp) in [(hs, &start), (he, &end)] {
            let Some(resp) = resp else { continue };
            let h = ease(anim(ui, resp.id.with("hov"), resp.hovered() || resp.dragged(), 0.2));
            let bar = Rect::from_center_size(r.center(), vec2(10.0, r.height() + 2.0 * h));
            painter.rect_filled(bar.expand(3.0 + 3.0 * h), 4.0 + 3.0 * h, p.acc.gamma_multiply(0.15 + 0.07 * h));
            painter.rect_filled(bar, 4, mix(p.acc, p.acch, h));
            let c = bar.center();
            painter.vline(c.x, c.y - 7.0..=c.y + 7.0, Stroke::new(1.5, p.on_acc.gamma_multiply(0.8)));
        }
        let x = x_of(self.pos);
        if visible(x) {
            let top = area.top() + 8.0;
            painter.vline(x, top..=rows.span.bottom() + 2.0, Stroke::new(2.0, p.t1));
            let tri = vec![pos2(x - 6.0, top - 1.0), pos2(x + 6.0, top - 1.0), pos2(x, top + 7.0)];
            painter.add(Shape::convex_polygon(tri, p.t1, Stroke::NONE));
        }

        if let Some(s) = &scroll {
            painter.rect_filled(rows.bar, CornerRadius::same(2), p.field);
            let lit = s.hovered() || matches!(self.drag, Some(Drag::Scroll { .. }));
            painter.rect_filled(thumb, CornerRadius::same(2), if lit { p.t3 } else { p.line3 });
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

    #[test]
    fn lanes_shrink_to_fit_and_every_track_gets_one() {
        assert_eq!(height(0, 34.0), TOP + VIDEO_H + SCROLL_H);
        assert_eq!(height(3, 34.0), TOP + VIDEO_H + GAP + HEAD_H + GAP + 3.0 * 40.0 - GAP + SCROLL_H);
        assert_eq!(lane_height(3, 1000.0), LANE_MAX);
        assert_eq!(lane_height(8, 100.0), LANE_MIN);
        let fit = lane_height(3, 200.0);
        assert!((height(3, fit) - 200.0).abs() < 0.01 && fit < LANE_MAX && fit > LANE_MIN, "{fit}");
        let r = rows(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, height(2, 30.0))), 2, 30.0);
        assert_eq!(r.lanes.len(), 2);
        assert_eq!(r.span.bottom(), r.lanes[1].bottom());
        assert!(r.bar.bottom() <= height(2, 30.0));
    }
}
