use eframe::egui::{
    self, Color32, CornerRadius, CursorIcon, Id, Mesh, Order, Painter, Rect, Role, Sense, Shape, Stroke, StrokeKind, Ui,
    WidgetInfo, pos2, vec2,
};
use glint_core::clip::edit::snap_to;
use glint_core::clip::export::{self, Job, Video};
use glint_core::clip::info::Info;
use glint_core::clip::math::{self, SECOND};
use glint_ui::widgets::{Look, anim, ease, medium, mix, motion, sans};

use super::preview::speaker;
use super::wave::BUCKET;
use super::clicklane::Axis;
use super::{ClipApp, Play, Selected, scan};

const TOP: f32 = 18.0;
const VIDEO_H: f32 = 52.0;
const GAP: f32 = 6.0;
const HEAD_H: f32 = 18.0;
const LANE_MAX: f32 = 34.0;
const LANE_MIN: f32 = 22.0;
const SCROLL_H: f32 = 10.0;
const ADD_H: f32 = 18.0;
const CLICKS_H: f32 = 24.0;
const KEYS_H: f32 = 24.0;
const HANDLE_W: f32 = 6.0;
const HANDLE_HIT: f32 = 14.0;
const RENAME_W: f32 = 140.0;
const MENU_W: f32 = 180.0;
const ITEM_H: f32 = 30.0;
const HEADER: &str = "sound · muted tracks won't be in the file";
const MUTED: &str = "· muted, not exported";
const ADD_TEXT: &str = "+ add empty track";
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

/// The timeline's height with `lanes` sound lanes `lane` tall, and the overlay lanes when present.
pub(super) fn height(lanes: usize, lane: f32, clicks: bool, keys: bool) -> f32 {
    let sound = if lanes > 0 { GAP + HEAD_H + GAP + lanes as f32 * (lane + GAP) - GAP } else { 0.0 };
    let overlay = if clicks { GAP + CLICKS_H } else { 0.0 } + if keys { GAP + KEYS_H } else { 0.0 };
    TOP + VIDEO_H + overlay + sound + GAP + ADD_H + SCROLL_H
}

/// Sound lanes shrink from 34 to 22 px to fit in `room`.
pub(super) fn lane_height(tracks: usize, room: f32, clicks: bool, keys: bool) -> f32 {
    if tracks == 0 {
        return LANE_MAX;
    }
    ((room - height(tracks, 0.0, clicks, keys)) / tracks as f32).clamp(LANE_MIN, LANE_MAX)
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

    pub(super) fn x(&self, t: i64, left: f32, width: f32) -> f32 {
        left + width * ((t - self.start) as f64 / self.span as f64) as f32
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
    clicks: Option<Rect>,
    keys: Option<Rect>,
    head: Rect,
    lanes: Vec<Rect>,
    span: Rect,
    bands: Vec<Rect>,
    add: Rect,
    bar: Rect,
}

struct Chip {
    rect: Rect,
    dots: Rect,
    resp: egui::Response,
    more: egui::Response,
}

#[derive(Clone, Copy)]
enum LaneItem {
    Rename,
    Up,
    Down,
    Delete,
}

impl LaneItem {
    fn label(self) -> &'static str {
        match self {
            LaneItem::Rename => "rename",
            LaneItem::Up => "move up",
            LaneItem::Down => "move down",
            LaneItem::Delete => "delete track",
        }
    }
}

fn rows(area: Rect, lanes: usize, lane: f32, clicks: bool, keys: bool) -> Rows {
    let video = Rect::from_min_size(pos2(area.left(), area.top() + TOP), vec2(area.width(), VIDEO_H));
    let clicks = clicks.then(|| Rect::from_min_size(pos2(area.left(), video.bottom() + GAP), vec2(area.width(), CLICKS_H)));
    let below = clicks.map_or(video, |c| video.union(c));
    let keys = keys.then(|| Rect::from_min_size(pos2(area.left(), below.bottom() + GAP), vec2(area.width(), KEYS_H)));
    let picture = keys.map_or(below, |k| below.union(k));
    let head = Rect::from_min_size(pos2(area.left(), picture.bottom() + GAP), vec2(area.width(), HEAD_H));
    let lanes: Vec<Rect> = (0..lanes)
        .map(|k| Rect::from_min_size(pos2(area.left(), head.bottom() + GAP + k as f32 * (lane + GAP)), vec2(area.width(), lane)))
        .collect();
    let bottom = lanes.last().map_or(picture.bottom(), |l| l.bottom());
    let span = Rect::from_min_max(video.min, pos2(area.right(), bottom));
    let mut bands = vec![picture];
    if let (Some(first), Some(last)) = (lanes.first(), lanes.last()) {
        bands.push(Rect::from_min_max(first.min, last.max));
    }
    let add = Rect::from_min_size(pos2(area.left(), bottom + GAP), vec2(area.width(), ADD_H));
    let bar = Rect::from_min_size(pos2(area.left() + 6.0, add.bottom() + 4.0), vec2(area.width() - 12.0, 4.0));
    Rows { video, clicks, keys, head, lanes, span, bands, add, bar }
}

fn grips(rows: &Rows, x: f32) -> Vec<Rect> {
    rows.bands.iter().map(|b| Rect::from_center_size(pos2(x, b.center().y), vec2(HANDLE_HIT, b.height() + 4.0))).collect()
}

fn lane_slot(rows: &Rows, y: f32) -> usize {
    let (Some(first), n) = (rows.lanes.first(), rows.lanes.len()) else { return 0 };
    let pitch = first.height() + GAP;
    (((y - first.top() + GAP / 2.0) / pitch).floor().max(0.0) as usize).min(n - 1)
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
        let rows = rows(area, self.edit.lanes.len(), lane_h, self.edit.clicks.is_some(), self.edit.keys.is_some());
        let dur = info.duration.max(1);
        let frame = math::frame_len(info.fps);
        let inner = area.shrink2(vec2(6.0, 0.0));
        let min_span = (frame * 12).max(SECOND / 5);
        self.view.clamp(dur, min_span);
        let light = self.light == Some(true);

        let blocked = self.sheet || self.menu || self.confirm_delete || self.lane_menu.is_some() || self.click_menu.is_some() || self.key_menu.is_some();
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
        let x_of = move |t: i64| v.x(t, inner.left(), inner.width());
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
        let lanes = self.edit.lanes.clone();
        let renaming = self.renaming.as_ref().map(|(k, _)| *k);
        let chips: Vec<Chip> = (lanes.iter().enumerate())
            .map(|(k, lane)| {
                let name = lane.name(&info.tracks);
                let name_w = match renaming {
                    Some(r) if r == k => RENAME_W,
                    _ => ui.painter().layout_no_wrap(name.to_string(), medium(12.0), p.t1).size().x,
                };
                let extra = if lane.muted { ui.painter().layout_no_wrap(MUTED.into(), sans(12.0), p.t3).size().x + 6.0 } else { 0.0 };
                let l = rows.lanes[k];
                let h = (l.height() - 6.0).min(26.0);
                let rect = Rect::from_min_size(pos2(l.left() + 8.0, l.center().y - h / 2.0), vec2(h + 6.0 + name_w + extra + 8.0, h));
                let tip = if lane.muted {
                    "muted: not in the file · click to unmute · drag to reorder"
                } else {
                    "click to mute: muted sound isn't saved · drag to reorder · right-click for more"
                };
                let sense = if renaming == Some(k) { Sense::hover() } else { Sense::click_and_drag() };
                let cursor = if self.lane_drag.is_some() { CursorIcon::Grabbing } else { CursorIcon::PointingHand };
                let resp = ui.interact(rect, Id::unique(("clip-lane", k)), sense).on_hover_cursor(cursor).on_hover_text(tip);
                resp.widget_info(|| WidgetInfo::selected(Role::CheckBox, true, !lane.muted, format!("sound: {name}")));
                let dots = Rect::from_min_size(pos2(rect.right() + 4.0, rect.top()), vec2(h, h));
                let more = ui
                    .interact(dots, Id::unique(("clip-lane-more", k)), Sense::click())
                    .on_hover_cursor(CursorIcon::PointingHand)
                    .on_hover_text("rename, move or delete this track");
                more.widget_info(|| WidgetInfo::labeled(Role::Button, true, format!("{name}: more")));
                Chip { rect, dots, resp, more }
            })
            .collect();
        let all_muted = lanes.iter().all(|l| l.muted);
        let link_text = if all_muted { "unmute all" } else { "mute all" };
        let link = (!lanes.is_empty()).then(|| {
            let g = ui.painter().layout_no_wrap(link_text.into(), sans(12.0), p.t2);
            let r = Rect::from_min_size(pos2(rows.head.right() - g.size().x - 2.0, rows.head.center().y - 9.0), vec2(g.size().x + 2.0, 18.0));
            let resp = ui.interact(r, Id::unique("clip-mute-all"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
            resp.widget_info(|| WidgetInfo::labeled(Role::Link, true, link_text));
            (r, resp)
        });
        let add_w = ui.painter().layout_no_wrap(ADD_TEXT.into(), sans(12.0), p.t2).size().x;
        let add_r = Rect::from_min_size(pos2(rows.add.left() + 8.0, rows.add.center().y - 9.0), vec2(add_w + 2.0, 18.0));
        let add = ui
            .interact(add_r, Id::unique("clip-add-track"), Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("a silent track, for editors and sites that expect a set number of tracks");
        add.widget_info(|| WidgetInfo::labeled(Role::Button, true, ADD_TEXT));
        let axis = Axis { left: inner.left(), width: inner.width(), view: v, dur, fps: info.fps, snapping };
        let click_lane = rows.clicks.and_then(|lane| self.clicks_input(ui, look, lane, &axis));
        let key_lane = rows.keys.and_then(|lane| self.keys_input(ui, look, lane, &axis));
        let start_tip = if pieces.is_some() {
            "drag to trim · snaps to keyframes so it saves instantly · ctrl + drag for an exact frame (the file then starts at the keyframe before it)"
        } else {
            "drag to trim · ctrl + drag: no snapping"
        };
        let grip = |t: i64, id: &str, tip: &str| {
            let x = x_of(t);
            visible(x).then(|| {
                (grips(&rows, x).into_iter().enumerate())
                    .map(|(i, r)| {
                        ui.interact(r, Id::unique((id, i)), Sense::drag())
                            .on_hover_cursor(CursorIcon::ResizeHorizontal)
                            .on_hover_text(tip)
                    })
                    .collect::<Vec<_>>()
            })
        };
        let start = grip(self.edit.range.start, "clip-in", start_tip);
        let end = grip(self.edit.range.end, "clip-out", "drag to trim · ctrl + drag: no snapping");
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
        for (k, chip) in chips.iter().enumerate() {
            if chip.resp.clicked() {
                self.set_muted(k, !lanes[k].muted);
            }
            if chip.resp.secondary_clicked() {
                let at = chip.resp.interact_pointer_pos().unwrap_or(chip.rect.left_bottom());
                self.lane_menu = Some((k, at, chip.rect));
            }
            if chip.more.clicked() {
                let open = matches!(self.lane_menu, Some((j, ..)) if j == k);
                self.lane_menu = (!open).then(|| (k, chip.dots.left_bottom() + vec2(0.0, 4.0), chip.dots));
            }
            if chip.resp.drag_started() && lanes.len() > 1 {
                self.lane_drag = Some(k);
            }
        }
        if link.as_ref().is_some_and(|(_, r)| r.clicked()) {
            self.change(|e| {
                e.lanes.iter_mut().for_each(|l| l.muted = !all_muted);
                true
            });
            self.sync_gains();
        }
        if add.clicked() {
            self.change(|e| {
                e.add_empty();
                true
            });
        }
        if let Some(from) = self.lane_drag
            && !ui.input(|i| i.pointer.any_down())
        {
            self.lane_drag = None;
            if let Some(at) = ui.input(|i| i.pointer.latest_pos()) {
                self.change(|e| e.move_lane(from, lane_slot(&rows, at.y)));
            }
        }

        let head = self.pos;
        let pressed = |r: &Option<Vec<egui::Response>>| r.as_ref().is_some_and(|r| r.iter().any(|r| r.drag_started()));
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
            self.click_focus = false;
            self.click_sel.clear();
            self.key_focus = false;
            self.key_sel.clear();
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
        if let Some(r) = start.as_ref().and_then(|r| r.first()) {
            r.widget_info(|| timed(Role::Slider, "trim start", self.edit.range.start));
        }
        if let Some(r) = end.as_ref().and_then(|r| r.first()) {
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

        let (now, animate) = (self.now, motion());
        let head_t = match self.drag {
            _ if matches!(self.play, Play::Running { .. }) => self.playhead.play(now, self.pos),
            Some(Drag::Head | Drag::Start { .. } | Drag::End { .. }) => {
                self.playhead.set(self.pos);
                self.pos
            }
            _ => {
                self.playhead.glide(now, self.pos, animate, ease);
                self.playhead.at(now, ease)
            }
        };
        let ends = [
            (self.edit.range.start, matches!(self.drag, Some(Drag::Start { .. }))),
            (self.edit.range.end, matches!(self.drag, Some(Drag::End { .. }))),
        ];
        let trim_t = [0, 1].map(|k| {
            let (t, dragged) = ends[k];
            if dragged {
                self.trim[k].set(t);
            } else {
                self.trim[k].glide(now, t, animate, ease);
            }
            self.trim[k].at(now, ease)
        });
        if self.playhead.moving(now) || self.trim.iter().any(|g| g.moving(now)) {
            ui.ctx().request_repaint();
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

        if !lanes.is_empty() {
            painter.text(rows.head.left_center(), egui::Align2::LEFT_CENTER, HEADER, sans(12.0), p.t3);
        }
        let mut fades = Vec::with_capacity(rows.lanes.len());
        for (k, l) in rows.lanes.iter().enumerate() {
            let fade = 1.0 - 0.62 * ease(anim(ui, Id::unique(("clip-lane-fade", k)), lanes[k].muted, 0.3));
            fades.push(fade);
            let clip = lane_clip(*l);
            clip.rect_filled(*l, 6, p.ctl.gamma_multiply(fade));
            let (x_start, stop) = (inner.left().max(x_of(0)), inner.right().min(x_of(dur)));
            let Some(source) = lanes[k].source else {
                let mut x = x_start;
                while x < stop {
                    clip.hline(x..=(x + 4.0).min(stop), l.center().y, Stroke::new(1.0, p.t3.gamma_multiply(fade)));
                    x += 8.0;
                }
                continue;
            };
            let Some(peaks) = self.peaks.get(source).filter(|v| !v.is_empty()) else { continue };
            let color = WAVES[source % WAVES.len()].gamma_multiply(0.75 * fade);
            let mut mesh = Mesh::default();
            let (cy, room) = (l.center().y, l.height() * 0.8);
            let mut x = x_start;
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

        if let Some(lane) = &click_lane {
            self.clicks_paint(ui, look, lane, &axis, &painter);
        }
        if let Some(lane) = &key_lane {
            self.keys_paint(ui, look, lane, &axis, &painter);
        }
        let shade = Color32::from_black_alpha(if light { 70 } else { 158 });
        for band in &rows.bands {
            let clip = lane_clip(Rect::from_min_max(pos2(area.left(), band.top()), pos2(area.right(), band.bottom())));
            let span_rect = |a: i64, b: i64| Rect::from_min_max(pos2(x_of(a), band.top()), pos2(x_of(b), band.bottom()));
            for s in self.edit.segments.iter().filter(|s| s.removed) {
                let r = span_rect(s.start, s.end);
                clip.rect_filled(r, 0, shade);
                hatch(&clip, r, p.t3.gamma_multiply(0.6));
            }
            let (x0, x1) = (x_of(trim_t[0]), x_of(trim_t[1]));
            clip.rect_filled(Rect::from_min_max(pos2(area.left(), band.top()), pos2(x0, band.bottom())), 0, shade);
            clip.rect_filled(Rect::from_min_max(pos2(x1, band.top()), pos2(area.right(), band.bottom())), 0, shade);
            if let Some(pieces) = &pieces {
                for pc in pieces.iter().filter(|pc| pc.lead < pc.start) {
                    let r = span_rect(pc.lead, pc.start);
                    clip.rect_filled(r, 0, p.surf.gamma_multiply(0.35));
                    hatch(&clip, r, p.acc.gamma_multiply(0.5));
                }
            }
            if let Some(Selected::Segment(i)) = self.selected
                && let Some(s) = self.edit.segments.get(i)
            {
                let r = span_rect(s.start, s.end).shrink2(vec2(1.5, 1.5));
                clip.rect_stroke(r, 3, Stroke::new(2.0, p.t1), StrokeKind::Inside);
            }
            for t in self.edit.splits() {
                let x = x_of(t);
                if visible(x) {
                    clip.vline(x, band.y_range(), Stroke::new(1.0, p.t2));
                }
            }
        }

        for (k, chip) in chips.iter().enumerate() {
            let fade = fades[k];
            let muted = lanes[k].muted;
            let clip = lane_clip(rows.lanes[k]);
            clip.rect_filled(chip.rect, 5, p.ctl.gamma_multiply(0.85 * fade.max(0.6)));
            if self.lane_drag == Some(k) {
                clip.rect_stroke(chip.rect, 5, Stroke::new(1.5, p.acc), StrokeKind::Inside);
            }
            let button = Rect::from_min_size(chip.rect.min, vec2(chip.rect.height(), chip.rect.height()));
            let h = anim(ui, chip.resp.id.with("hov"), chip.resp.hovered(), 0.12);
            clip.rect_filled(button, 5, p.line2.gamma_multiply(h));
            speaker(&clip, button.center() - vec2(1.0, 0.0), p.t1.gamma_multiply(fade.max(0.7)), !muted);
            let at = pos2(button.right() + 6.0, chip.rect.center().y);
            let nw = if renaming == Some(k) {
                RENAME_W
            } else {
                let g = clip.layout_no_wrap(lanes[k].name(&info.tracks).to_string(), medium(12.0), p.t1.gamma_multiply(fade.max(0.55)));
                let nw = g.size().x;
                clip.galley(at - vec2(0.0, g.size().y / 2.0), g, p.t1);
                nw
            };
            if muted {
                clip.text(pos2(at.x + nw + 6.0, at.y), egui::Align2::LEFT_CENTER, MUTED, sans(12.0), p.t3);
            }
            let open = matches!(self.lane_menu, Some((j, ..)) if j == k);
            let hd = anim(ui, chip.more.id.with("hov"), chip.more.hovered() || open, 0.12);
            clip.rect_filled(chip.dots, 5, p.line2.gamma_multiply(hd));
            for dx in [-4.0, 0.0, 4.0] {
                clip.circle_filled(chip.dots.center() + vec2(dx, 0.0), 1.3, mix(p.t3, p.t1, hd));
            }
        }
        if let Some(from) = self.lane_drag
            && let Some(at) = ui.input(|i| i.pointer.latest_pos())
        {
            let to = lane_slot(&rows, at.y);
            if to != from {
                let y = if to < from { rows.lanes[to].top() - GAP / 2.0 } else { rows.lanes[to].bottom() + GAP / 2.0 };
                painter.hline(area.x_range(), y, Stroke::new(2.0, p.acc));
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
        if let Some(lane) = &click_lane {
            self.clicks_chip(ui, look, lane, &painter);
        }
        if let Some(lane) = &key_lane {
            self.keys_chip(ui, look, lane, &painter);
        }
        let h = anim(ui, add.id.with("hov"), add.hovered(), 0.12);
        painter.text(add_r.left_center(), egui::Align2::LEFT_CENTER, ADD_TEXT, sans(12.0), mix(p.t3, p.t1, h));

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
        for (k, resp) in [&start, &end].into_iter().enumerate() {
            let x = x_of(trim_t[k]);
            if !visible(x) {
                continue;
            }
            let lit = resp.as_ref().is_some_and(|r| r.iter().any(|r| r.hovered() || r.dragged()));
            let h = ease(anim(ui, Id::unique(("clip-trim-hov", k)), lit, 0.2));
            for band in &rows.bands {
                let bar = Rect::from_center_size(pos2(x, band.center().y), vec2(HANDLE_W + 2.0 * h, band.height()));
                painter.rect_filled(bar.expand(2.0 + 2.0 * h), 3.0 + 2.0 * h, p.acc.gamma_multiply(0.15 + 0.07 * h));
                painter.rect_filled(bar, 3, mix(p.acc, p.acch, h));
            }
        }
        let x = x_of(head_t);
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
        self.rename_field(ui, look, &chips, &info);
    }

    fn rename_field(&mut self, ui: &mut Ui, look: &Look, chips: &[Chip], info: &Info) {
        let Some((k, mut text)) = self.renaming.take() else { return };
        let Some(chip) = chips.get(k) else { return };
        let p = look.p;
        let h = chip.rect.height();
        let r = Rect::from_min_size(pos2(chip.rect.left() + h + 2.0, chip.rect.top() + 1.0), vec2(RENAME_W + 8.0, h - 2.0));
        ui.painter().rect(r, 4, p.field, Stroke::new(1.0, p.acc), StrokeKind::Inside);
        let edit = egui::TextEdit::singleline(&mut text)
            .id(Id::unique(("clip-lane-name", k)))
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(4, 0)))
            .font(medium(12.0))
            .text_color(p.t1)
            .align(egui::Align2::LEFT_CENTER)
            .desired_width(r.width())
            .min_size(r.size())
            .char_limit(80);
        let resp = ui.put(r, edit);
        if resp.lost_focus() {
            self.change(|e| e.rename(k, &text, &info.tracks));
        } else {
            if !resp.has_focus() {
                resp.request_focus();
            }
            self.renaming = Some((k, text));
        }
    }

    pub(super) fn lane_menu_popup(&mut self, ctx: &egui::Context, look: &Look) {
        let Some((k, at, anchor)) = self.lane_menu else { return };
        let n = self.edit.lanes.len();
        let Some(tracks) = self.info().map(|i| i.tracks.clone()).filter(|_| k < n) else {
            self.lane_menu = None;
            return;
        };
        let p = look.p;
        let items = [(LaneItem::Rename, true), (LaneItem::Up, k > 0), (LaneItem::Down, k + 1 < n), (LaneItem::Delete, true)];
        let size = vec2(MENU_W, items.len() as f32 * ITEM_H + 8.0);
        let screen = ctx.content_rect();
        let y = if at.y + size.y > screen.bottom() - 4.0 { anchor.top() - 4.0 - size.y } else { at.y };
        let rect = Rect::from_min_size(pos2(at.x.min(screen.right() - 4.0 - size.x), y), size);
        let red = super::overlay::danger(self.light == Some(true));
        let mut picked = None;
        egui::Area::new(Id::unique("clip-lane-menu")).order(Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
            ui.painter().rect(rect, 8, p.surf, Stroke::new(1.0, p.line2), StrokeKind::Inside);
            let mut y = rect.top() + 4.0;
            for (i, (item, on)) in items.into_iter().enumerate() {
                let r = Rect::from_min_size(pos2(rect.left() + 4.0, y), vec2(MENU_W - 8.0, ITEM_H));
                let resp = ui.interact(r, Id::unique(("clip-lane-menu-item", i)), if on { Sense::click() } else { Sense::hover() });
                let resp = if on { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp };
                resp.widget_info(|| WidgetInfo::labeled(Role::MenuItem, on, item.label()));
                let hov = anim(ui, resp.id.with("hov"), on && resp.hovered(), 0.1);
                let painter = ui.painter();
                painter.rect_filled(r, 5, p.ctlh.gamma_multiply(hov));
                let color = match (item, on) {
                    (_, false) => p.t3,
                    (LaneItem::Delete, _) => red,
                    _ => mix(p.t2, p.t1, hov),
                };
                painter.text(pos2(r.left() + 10.0, r.center().y), egui::Align2::LEFT_CENTER, item.label(), sans(13.0), color);
                if on && resp.clicked() {
                    picked = Some(item);
                }
                y += ITEM_H;
            }
        });
        let outside = ctx.input(|i| {
            i.pointer.any_click() && i.pointer.interact_pos().is_some_and(|at| !rect.contains(at) && !anchor.contains(at))
        });
        if outside {
            self.lane_menu = None;
        }
        let Some(item) = picked else { return };
        self.lane_menu = None;
        match item {
            LaneItem::Rename => {
                let name = self.edit.lanes[k].name(&tracks).to_string();
                self.renaming = Some((k, name));
            }
            LaneItem::Up => {
                self.change(|e| e.move_lane(k, k - 1));
            }
            LaneItem::Down => {
                self.change(|e| e.move_lane(k, k + 1));
            }
            LaneItem::Delete => {
                self.renaming = None;
                self.change(|e| e.delete_lane(k));
                self.sync_gains();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_lands_on_its_x_in_the_visible_span() {
        use glint_core::clip::glide::Glide;
        let v = View { start: 10 * SECOND, span: 20 * SECOND };
        assert_eq!(v.x(10 * SECOND, 100.0, 400.0), 100.0);
        assert_eq!(v.x(20 * SECOND, 100.0, 400.0), 300.0);
        assert_eq!(v.x(30 * SECOND, 100.0, 400.0), 500.0);
        assert_eq!(v.x(SECOND / 2 + 10 * SECOND, 100.0, 400.0), 110.0);
        let mut g = Glide::new(0);
        let xs: Vec<f32> = (0..=60).map(|f| v.x(g.play(f as f64 / 60.0, 15 * SECOND + f * SECOND / 60), 100.0, 400.0)).collect();
        assert_eq!(xs[0], 200.0);
        assert!(xs.windows(2).all(|w| (w[1] - w[0] - 20.0 / 60.0).abs() < 0.01), "one second of playback moves 20 px evenly");
    }

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
        let add = GAP + ADD_H;
        assert_eq!(height(0, 34.0, false, false), TOP + VIDEO_H + add + SCROLL_H);
        assert_eq!(height(3, 34.0, false, false), TOP + VIDEO_H + GAP + HEAD_H + GAP + 3.0 * 40.0 - GAP + add + SCROLL_H);
        assert_eq!(lane_height(3, 1000.0, false, false), LANE_MAX);
        assert_eq!(lane_height(8, 100.0, false, false), LANE_MIN);
        let fit = lane_height(3, 230.0, false, false);
        assert!((height(3, fit, false, false) - 230.0).abs() < 0.01 && fit < LANE_MAX && fit > LANE_MIN, "{fit}");
        let r = rows(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, height(2, 30.0, false, false))), 2, 30.0, false, false);
        assert_eq!(r.lanes.len(), 2);
        assert_eq!(r.span.bottom(), r.lanes[1].bottom());
        assert!(r.add.top() > r.span.bottom() && r.bar.top() > r.add.bottom());
        assert!(r.bar.bottom() <= height(2, 30.0, false, false));
    }

    #[test]
    fn trim_grips_cover_the_video_and_the_sound_lanes_but_never_the_header() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, height(3, 30.0, false, false)));
        let r = rows(area, 3, 30.0, false, false);
        for x in [6.0, 400.0, 794.0] {
            let g = grips(&r, x);
            assert_eq!(g.len(), 2);
            assert!(g.iter().all(|g| !g.intersects(r.head) && g.width() == HANDLE_HIT), "{g:?} vs {:?}", r.head);
            assert!(g[0].contains(pos2(x, r.video.top())) && g[0].contains(pos2(x, r.video.bottom())));
            assert!(g[1].contains(pos2(x, r.lanes[0].top())) && g[1].contains(pos2(x, r.lanes[2].bottom())));
        }
        let silent = rows(area, 0, 30.0, false, false);
        assert_eq!(grips(&silent, 400.0).len(), 1);
        let with = rows(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, height(3, 30.0, true, false))), 3, 30.0, true, false);
        let lane = with.clicks.unwrap();
        assert!(lane.top() > with.video.bottom() && lane.bottom() < with.head.top());
        let g = grips(&with, 400.0);
        assert!(g[0].contains(pos2(400.0, with.video.top())) && g[0].contains(pos2(400.0, lane.bottom())), "the clicks trim with the video");
        assert!(!g[0].intersects(with.head) && !g[1].intersects(with.head));
        assert_eq!(height(3, 30.0, true, false) - height(3, 30.0, false, false), GAP + CLICKS_H);
        assert!(with.add.bottom() <= height(3, 30.0, true, false));

        let both = rows(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, height(3, 30.0, true, true))), 3, 30.0, true, true);
        let clicks = both.clicks.unwrap();
        let keys = both.keys.unwrap();
        assert!(keys.top() > clicks.bottom() && keys.bottom() < both.head.top(), "the keys lane sits under the clicks lane");
        let g = grips(&both, 400.0);
        assert!(g[0].contains(pos2(400.0, both.video.top())) && g[0].contains(pos2(400.0, keys.bottom())), "the keys trim with the video");
        assert!(!g[0].intersects(both.head) && !g[1].intersects(both.head));
        assert_eq!(height(3, 30.0, true, true) - height(3, 30.0, true, false), GAP + KEYS_H);
        assert!(both.add.bottom() <= height(3, 30.0, true, true));
    }

    #[test]
    fn a_dragged_lane_lands_in_the_slot_under_the_pointer() {
        let r = rows(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, height(3, 30.0, false, false))), 3, 30.0, false, false);
        assert_eq!(lane_slot(&r, r.head.top()), 0);
        assert_eq!(lane_slot(&r, r.lanes[0].center().y), 0);
        assert_eq!(lane_slot(&r, r.lanes[1].top() - 1.0), 1);
        assert_eq!(lane_slot(&r, r.lanes[1].center().y), 1);
        assert_eq!(lane_slot(&r, r.lanes[2].bottom() + 50.0), 2);
        assert_eq!(lane_slot(&rows(r.span, 0, 30.0, false, false), 10.0), 0);
    }
}
