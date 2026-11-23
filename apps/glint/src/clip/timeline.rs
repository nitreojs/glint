use eframe::egui::{
    self, Color32, CornerRadius, CursorIcon, Id, Order, Painter, Rect, Role, Sense, Shape, Stroke, StrokeKind, Ui,
    WidgetInfo, pos2, vec2,
};
use glint_core::clip::edit::{Edit, FULL_VOLUME, MAX_VOLUME, Segment, Speed, TimeMap};
use glint_core::clip::export::{Job, Video};
use glint_core::clip::info::Info;
use glint_core::clip::math::{self, SECOND};
use glint_ui::widgets::{Look, anim, ease, medium, mix, mono, motion, sans};

use super::preview::speaker;
use super::wave::{WAVES, Waveform};
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
const PIECE_DIM: Color32 = Color32::from_black_alpha(64);
const RENAME_W: f32 = 140.0;
const MENU_W: f32 = 180.0;
const ITEM_H: f32 = 30.0;
const HEADER: &str = "sound · muted tracks won't be in the file";
const MUTED: &str = "· muted, not exported";
const ADD_TEXT: &str = "+ add empty track";
const VOLUME_TIP: &str = "volume · drag, scroll or click · double-click for 100%";

/// How far a drag (in percent, a pixel each) has to pull to leave 100 %.
const DETENT: f32 = 6.0;
/// Wheel points per percent: one notch (40 points) is 5 %.
const WHEEL_PX: f32 = 8.0;
/// A volume gesture this long at rest becomes one undo step.
const SETTLE: f64 = 0.6;
const POP_W: f32 = 240.0;
const POP_H: f32 = 40.0;
const SNAP_PX: f32 = 8.0;
const MARKER: Color32 = Color32::from_rgb(0xF5, 0xC0, 0x4A);

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
    Stretch(Stretch),
    Scroll { grab: f32 },
}

#[derive(Clone, Copy, PartialEq)]
enum Edge {
    Left,
    Right,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) struct Stretch {
    index: usize,
    piece: Segment,
    kept: math::Range,
    edge: Edge,
    from: f32,
    units_per_point: f64,
    view_start: i64,
    fit_view: bool,
    output_len: i64,
    anchor_offset: i64,
}

impl Stretch {
    fn offset(piece: Segment, speed: Speed, source: i64) -> i64 {
        let source = source - piece.start;
        if source == piece.end - piece.start {
            return speed.output_len(source);
        }

        speed.output_delta(source)
    }

    fn anchor(&self) -> i64 {
        match self.edge {
            Edge::Left => self.kept.end,
            Edge::Right => self.kept.start,
        }
    }

    fn moving_edge(&self) -> i64 {
        match self.edge {
            Edge::Left => self.kept.start,
            Edge::Right => self.kept.end,
        }
    }

    fn duration(piece: Segment, kept: math::Range, speed: Speed) -> i64 {
        let start = Self::offset(piece, speed, kept.start);
        let end = Self::offset(piece, speed, kept.end);

        (end - start).max(1)
    }

    fn rate_at(&self, x: f32) -> Speed {
        let delta = (f64::from(x - self.from) * self.units_per_point).round() as i64;
        if delta == 0 {
            return self.piece.speed;
        }

        let output = match self.edge {
            Edge::Left => self.output_len.saturating_sub(delta),
            Edge::Right => self.output_len.saturating_add(delta),
        };

        Speed::from_lengths(self.kept.len(), output.max(1))
    }
}

#[derive(Clone, Copy)]
pub(super) struct ResetMotion {
    index: usize,
    piece: Segment,
    view: View,
    at: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct View {
    pub start: i64,
    pub span: i64,
}

impl View {
    fn between(self, to: View, t: f32) -> View {
        let lerp = |a: i64, b: i64| (a as f64 + (b as f64 - a as f64) * f64::from(t)).round() as i64;

        View { start: lerp(self.start, to.start), span: lerp(self.span, to.span) }
    }

    // Ease the painted positions, not the time span. At high speeds, easing the
    // span keeps most of the visible movement bunched up at the end.
    fn refit_between(self, to: View, t: f32) -> View {
        let from_scale = (1.0 - f64::from(t)) / self.span as f64;
        let to_scale = f64::from(t) / to.span as f64;
        let span = (from_scale + to_scale).recip();

        View {
            start: ((self.start as f64 * from_scale + to.start as f64 * to_scale) * span).round() as i64,
            span: span.round() as i64,
        }
    }

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

const HATCH_STEP: f32 = 7.0;
const HATCH_PAD: f32 = 1.0;

fn hatch_phase(now: f64, animate: bool, pixels_per_point: f32) -> f32 {
    if !animate {
        return 0.0;
    }
    let scale = f64::from(pixels_per_point);
    let pitch = (f64::from(HATCH_STEP) * scale).round().max(1.0);

    ((now * 5.0 * scale).round().rem_euclid(pitch) / scale) as f32
}

fn hatch(painter: &Painter, r: Rect, color: Color32, phase: f32) {
    let visible = r.intersect(painter.clip_rect());
    if !visible.is_positive() {
        return;
    }

    let painter = painter.with_clip_rect(visible);
    let top = (visible.top() - HATCH_PAD).max(r.top());
    let bottom = (visible.bottom() + HATCH_PAD).min(r.bottom());
    let rise = bottom - top;
    let first = visible.left() - rise - HATCH_PAD;
    let last = visible.right() + HATCH_PAD;
    let scale = painter.ctx().pixels_per_point();
    let pitch = (HATCH_STEP * scale).round().max(1.0) / scale;
    let anchor = f64::from(r.left()) + f64::from(r.top()) - f64::from(bottom) + f64::from(phase);
    let mut x = first + (anchor - f64::from(first)).rem_euclid(f64::from(pitch)) as f32;

    while x <= last {
        painter.line_segment([pos2(x, bottom), pos2(x + rise, top)], Stroke::new(1.0, color));
        x += pitch;
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
    /// The volume pill opened `shown` of the way to `full`; it takes input once mostly open.
    vol: Rect,
    full: Rect,
    shown: f32,
    vol_resp: Option<egui::Response>,
    /// The lane's head, chip to dots: the wheel sets the volume here.
    zone: Rect,
}

/// A volume change still in motion (a drag, a run of wheel notches): undo takes it back in one step.
pub(super) struct Nudge {
    lane: usize,
    before: Edit,
    at: f64,
    /// The part of the pull that hasn't made a whole percent yet.
    rest: f32,
}

/// `vol` pulled by `delta` percent, carrying what doesn't make a whole percent in the returned rest.
/// `sticky` (a drag) holds 100 % until the pull passes [`DETENT`] and catches a drag passing through it.
fn step_volume(vol: u16, rest: f32, delta: f32, sticky: bool) -> (u16, f32) {
    let (pull, full) = (rest + delta, FULL_VOLUME as f32);
    if sticky && vol == FULL_VOLUME && pull.abs() < DETENT {
        return (vol, pull);
    }
    let target = (vol as f32 + pull).clamp(0.0, MAX_VOLUME as f32);
    if sticky && (vol as f32 - full) * (target - full) < 0.0 {
        return (FULL_VOLUME, 0.0);
    }
    let v = target.round();
    (v as u16, target - v)
}

#[derive(Clone, Copy)]
enum LaneItem {
    Rename,
    Up,
    Down,
    Delete,
    Remember,
    Group,
}

impl LaneItem {
    fn label(self, grouped: bool) -> &'static str {
        match self {
            LaneItem::Rename => "rename",
            LaneItem::Up => "move up",
            LaneItem::Down => "move down",
            LaneItem::Delete => "delete track",
            LaneItem::Remember => "remember track volume",
            LaneItem::Group => if grouped { "ungroup tracks" } else { "group tracks" },
        }
    }

    fn paint_icon(self, painter: &Painter, center: egui::Pos2, color: Color32, grouped: bool) {
        let line = Stroke::new(1.35, color);
        let at = |x, y| center + vec2(x, y);
        let rect = |x, y, w, h| Rect::from_min_size(at(x, y), vec2(w, h));
        let segment = |a, b| painter.line_segment([a, b], line);
        match self {
            Self::Rename => {
                painter.add(Shape::closed_line(vec![at(-5.0, 5.0), at(-4.0, 1.0), at(2.5, -5.5), at(5.5, -2.5), at(-1.0, 4.0)], line));
                segment(at(1.0, -4.0), at(4.0, -1.0));
            }
            Self::Up | Self::Down => {
                let dir = if matches!(self, Self::Up) { -1.0 } else { 1.0 };
                segment(at(0.0, -5.0 * dir), at(0.0, 5.0 * dir));
                painter.add(Shape::line(vec![at(-4.0, dir), at(0.0, 5.0 * dir), at(4.0, dir)], line));
            }
            Self::Remember => {
                painter.rect_stroke(rect(-5.5, -5.5, 11.0, 11.0), 1, line, StrokeKind::Inside);
                painter.rect_stroke(rect(-2.5, 0.0, 5.0, 5.5), 0, line, StrokeKind::Inside);
                segment(at(0.0, -5.0), at(0.0, -2.5));
            }
            Self::Group => {
                let gap = if grouped { 2.0 } else { 0.0 };
                painter.rect_stroke(rect(-6.0, -4.0 - gap, 8.0, 5.0), 1, line, StrokeKind::Inside);
                painter.rect_stroke(rect(-2.0, -1.0 + gap, 8.0, 5.0), 1, line, StrokeKind::Inside);
            }
            Self::Delete => {
                segment(at(-6.0, -3.0), at(6.0, -3.0));
                painter.add(Shape::line(vec![at(-4.0, -3.0), at(-3.0, 5.0), at(3.0, 5.0), at(4.0, -3.0)], line));
                painter.add(Shape::line(vec![at(-2.0, -3.0), at(-2.0, -5.5), at(2.0, -5.5), at(2.0, -3.0)], line));
            }
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

fn kept_bounds(piece: Segment, range: math::Range) -> math::Range {
    math::Range { start: piece.start.max(range.start), end: piece.end.min(range.end) }
}

fn shift_press(ui: &Ui) -> Option<egui::Pos2> {
    ui.input(|input| {
        input.events.iter().rev().find_map(|event| match event {
            egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers }
                if modifiers.shift => Some(*pos),
            _ => None,
        })
    })
}

impl ClipApp {
    pub(super) fn toggle_group(&mut self) {
        self.grouped = !self.grouped;
        self.select_timeline(None);
    }

    pub(super) fn track_context_menu(&mut self) {
        let Some(hwnd) = self.hwnd else { return };
        if !std::mem::take(&mut self.track_context) || self.edit.lanes.is_empty() {
            return;
        }

        use glint_shell::menu::{self, Glyph, Item, Menu};
        let (label, icon) = if self.grouped { ("ungroup tracks", Glyph::Trim) } else { ("group tracks", Glyph::Copy) };
        let menu = Menu { accent: self.accent, header: None, items: vec![Item::command(1, label).glyph(icon)] };
        if menu::track(hwnd, &menu, false, || None) == Some(1) {
            self.toggle_group();
        }
    }

    pub(super) fn select_timeline(&mut self, selected: Option<Selected>) {
        if self.selected != selected {
            self.settle_volume(true);
            self.volume_pop = None;
            self.selected = selected;
        }
    }

    fn audio_selected_at(&self, lane: &glint_core::clip::edit::Lane) -> Option<i64> {
        match self.selected {
            Some(Selected::Audio { source, at }) if !self.grouped && lane.source == Some(source) => Some(at),
            _ => None,
        }
    }

    fn selected_audio_piece(&self, lane: &glint_core::clip::edit::Lane) -> Option<math::Range> {
        lane.piece(self.audio_selected_at(lane)?, self.duration())
    }

    fn lane_controls(&self, lane: &glint_core::clip::edit::Lane) -> (u16, bool) {
        self.selected_audio_piece(lane)
            .map_or((lane.volume, lane.muted), |piece| lane.piece_mix(piece.start))
    }

    fn set_control_volume(&mut self, k: usize, volume: u16) -> bool {
        let lane = &self.edit.lanes[k];
        if let Some(piece) = self.selected_audio_piece(lane) {
            self.edit.set_range_mix(k, piece, volume, lane.piece_mix(piece.start).1)
        } else {
            self.edit.set_volume(k, volume)
        }
    }

    fn toggle_lane_control(&mut self, k: usize) {
        let lane = &self.edit.lanes[k];
        if let Some(at) = self.audio_selected_at(lane).filter(|_| !lane.cuts.is_empty()) {
            self.silence_audio_piece(k, at, true);
        } else {
            self.set_muted(k, !lane.muted);
        }
    }

    pub(super) fn silence_audio_piece(&mut self, k: usize, at: i64, toggle: bool) {
        let lane = &self.edit.lanes[k];
        let Some(piece) = lane.piece(at, self.duration()) else { return };
        let (volume, muted) = lane.piece_mix(piece.start);
        if self.change(|edit| edit.set_range_mix(k, piece, volume, !muted || !toggle)) {
            self.sync_gains();
        }
    }

    fn parked_piece(&self) -> Option<usize> {
        if self.play != Play::Paused
            || self.edit.segments.len() < 2
            || !(self.edit.range.start..self.edit.range.end).contains(&self.pos)
        {
            return None;
        }

        let index = self.edit.segment_at(self.pos);

        (!self.edit.segments[index].removed).then_some(index)
    }

    fn stretch_edge(&self, x: f32, x_of: impl Fn(i64) -> f32) -> Option<(usize, math::Range, Edge)> {
        let edit = &self.edit;
        let within_range = x.clamp(x_of(edit.range.start), x_of(edit.range.end));
        let index = edit.segments.partition_point(|piece| {
            piece.end < edit.range.end && x_of(piece.end) <= within_range
        });
        let piece = *edit.segments.get(index)?;
        let kept = kept_bounds(piece, edit.range);
        if piece.removed || kept.start >= kept.end {
            return None;
        }

        let left = (x - x_of(kept.start)).abs();
        let right = (x - x_of(kept.end)).abs();
        if left.min(right) > HANDLE_HIT / 2.0 {
            return None;
        }

        let (edge, cut) = if left <= right {
            (Edge::Left, kept.start)
        } else {
            (Edge::Right, kept.end)
        };

        if edit.range.start < cut && cut < edit.range.end
            && let Some(parked) = self.parked_piece()
        {
            let target = edit.segments[parked];
            let other_edge = match edge {
                Edge::Left if target.end == cut => Some(Edge::Right),
                Edge::Right if target.start == cut => Some(Edge::Left),
                _ => None,
            };
            if let Some(edge) = other_edge
                && self.pos > target.start
            {
                return Some((parked, kept_bounds(target, edit.range), edge));
            }
        }

        Some((index, kept, edge))
    }

    fn snapped(&self, axis: &Axis<'_>, x: f32, t: i64, extra: &[i64], keys: &[i64]) -> i64 {
        if !axis.snapping {
            return t;
        }

        let min = axis.t(x - SNAP_PX);
        let max = axis.t(x + SNAP_PX);
        let first = keys.partition_point(|&key| key < min);
        let last = keys.partition_point(|&key| key <= max);
        let targets = self.edit.markers.iter().copied()
            .chain(self.cuts.iter().copied())
            .chain(self.edit.splits())
            .chain([0, axis.source_dur])
            .chain(extra.iter().copied())
            .chain(keys[first..last].iter().copied());

        targets.filter(|&s| (min..=max).contains(&s))
            .map(|s| (s, (axis.x(s) - x).abs()))
            .filter(|&(_, distance)| distance <= SNAP_PX)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map_or(t, |(s, _)| s)
    }

    fn update_stretch(&mut self, stretch: &Stretch, x: f32) -> bool {
        let Some(piece) = self.edit.segments.get(stretch.index) else { return false };
        if piece.removed || piece.start != stretch.piece.start || piece.end != stretch.piece.end {
            return false;
        }

        let speed = stretch.rate_at(x);
        if !self.edit.set_speed(stretch.index, speed) {
            return false;
        }

        self.pause();
        self.view.start = stretch.view_start
            + (Stretch::offset(stretch.piece, speed, stretch.anchor()) - stretch.anchor_offset);

        true
    }

    pub(super) fn timeline(&mut self, ui: &mut Ui, look: &Look, area: Rect, lane_h: f32, follow: Option<i64>) {
        let p = look.p;
        let stretching = matches!(self.drag, Some(Drag::Stretch(_)));
        let Some(info) = self.info().cloned() else {
            ui.painter().rect_filled(Rect::from_min_size(area.min + vec2(0.0, TOP), vec2(area.width(), VIDEO_H)), 6, p.ctl);
            return;
        };
        let rows = rows(area, self.edit.lanes.len(), lane_h, self.edit.clicks.is_some(), self.edit.keys.is_some());
        let source_dur = info.duration.max(1);
        let interrupted = ui.input(|i| i.pointer.any_pressed() || i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. } | egui::Event::MouseWheel { .. })));
        let transition = self.reset_motion.and_then(|m| {
            let progress = ((self.now - m.at) / 0.18).clamp(0.0, 1.0) as f32;
            if !motion() || progress >= 1.0 || interrupted || self.play != Play::Paused
                || self.edit.segments.get(m.index) != Some(&Segment { speed: Speed::NORMAL, ..m.piece }) {
                return None;
            }
            let t = ease(progress);
            let len = m.piece.end - m.piece.start;
            let from = m.piece.speed.output_len(len) as f64;
            let speed = Speed::from_lengths(len, (from + (len as f64 - from) * f64::from(t)).round() as i64);
            ui.ctx().request_repaint();

            Some((m, speed, t))
        });
        if transition.is_none() {
            self.reset_motion = None;
        }
        let visual = transition.map(|(m, speed, _)| (m.index, speed));
        let mut map = TimeMap::from_segments(&self.edit.segments, visual);
        let mut dur = map.duration().max(1);
        let frame = math::frame_len(info.fps);
        let inner = area.shrink2(vec2(6.0, 0.0));
        let min_span = (frame * 12).max(SECOND / 5);
        if let Some((from, to, at)) = self.refit {
            let t = ((self.now - at) / 0.18).clamp(0.0, 1.0) as f32;
            if t >= 1.0 || interrupted || !motion() || self.play != Play::Paused {
                self.view = to;
                self.refit = None;
            } else {
                self.view = from.refit_between(to, ease(t));
                ui.ctx().request_repaint();
            }
        }
        if !stretching && transition.is_none() && self.refit.is_none() {
            self.view.clamp(dur, min_span);
            if let Some(source) = follow {
                self.view.follow(map.to_edit(source), dur);
            }
        }
        let light = self.light == Some(true);

        let menus = self.sheet || self.menu || self.confirm_delete || self.confirm_cleanup.is_some()
            || self.lane_menu.is_some() || self.click_menu.is_some() || self.key_menu.is_some();
        let hover = ui.input(|i| i.pointer.hover_pos()).filter(|at| area.contains(*at));
        let (ctrl, shift) = ui.input(|i| (i.modifiers.ctrl, i.modifiers.shift));
        let body = ui.interact(Rect::from_min_max(area.min, rows.span.max), Id::unique("clip-track"), Sense::click_and_drag());
        let lanes = self.edit.lanes.clone();
        let renaming = self.renaming.as_ref().map(|(k, _)| *k);
        let pill_w = ui.painter().layout_no_wrap("200%".into(), mono(11.0), p.t2).size().x + 14.0;
        let still = self.drag.is_none() && self.lane_drag.is_none() && !menus;
        let chips: Vec<Chip> = (lanes.iter().enumerate())
            .map(|(k, lane)| {
                let name = lane.name(&info.tracks);
                let (volume, muted) = self.lane_controls(lane);
                let name_w = match renaming {
                    Some(r) if r == k => RENAME_W,
                    _ => ui.painter().layout_no_wrap(name.to_string(), medium(12.0), p.t1).size().x,
                };
                let piece = self.selected_audio_piece(lane).is_some();
                let extra = if muted {
                    ui.painter().layout_no_wrap(if piece { "· piece muted".into() } else { MUTED.into() }, sans(12.0), p.t3).size().x + 6.0
                } else { 0.0 };
                let l = rows.lanes[k];
                let h = (l.height() - 6.0).min(26.0);
                let rect = Rect::from_min_size(pos2(l.left() + 8.0, l.center().y - h / 2.0), vec2(h + 6.0 + name_w + extra + 8.0, h));
                let tip = if piece {
                    "click to mute or unmute the selected piece"
                } else if muted {
                    "muted: not in the file · click to unmute · drag to reorder"
                } else {
                    "click to mute: muted sound isn't saved · drag to reorder · right-click for more"
                };
                let sense = if renaming == Some(k) { Sense::hover() } else { Sense::click_and_drag() };
                let cursor = if self.lane_drag.is_some() { CursorIcon::Grabbing } else { CursorIcon::PointingHand };
                let resp = ui.interact(rect, Id::unique(("clip-lane", k)), sense).on_hover_cursor(cursor).on_hover_text(tip);
                resp.widget_info(|| WidgetInfo::selected(Role::CheckBox, true, !muted, format!("sound: {name}")));
                let zone = Rect::from_min_size(rect.min, vec2(rect.width() + 4.0 + pill_w + 4.0 + h, h));
                let popped = matches!(self.volume_pop, Some((j, _)) if j == k);
                let moving = self.nudge.as_ref().is_some_and(|n| n.lane == k);
                let wanted = piece || volume != FULL_VOLUME || popped || moving || (still && ui.rect_contains_pointer(zone));
                let shown = ease(anim(ui, Id::unique(("clip-lane-vol", k)), wanted, 0.18));
                let full = Rect::from_min_size(pos2(rect.right() + 4.0, rect.top()), vec2(pill_w, h));
                let vol = Rect::from_min_size(full.min, vec2(pill_w * shown, h));
                let vol_resp = (shown > 0.5 && renaming != Some(k)).then(|| {
                    let r = ui
                        .interact(vol, Id::unique(("clip-lane-volume", k)), Sense::click_and_drag())
                        .on_hover_cursor(CursorIcon::ResizeHorizontal)
                        .on_hover_text(if piece { "piece volume · drag, scroll or click · double-click for 100%" } else { VOLUME_TIP });
                    r.widget_info(|| WidgetInfo::slider(true, volume as f64, format!("{name}: volume")));
                    r
                });
                let dots = Rect::from_min_size(pos2(vol.right() + 4.0 * shown, rect.top()), vec2(h, h));
                let more = ui
                    .interact(dots, Id::unique(("clip-lane-more", k)), Sense::click())
                    .on_hover_cursor(CursorIcon::PointingHand)
                    .on_hover_text("track options");
                more.widget_info(|| WidgetInfo::labeled(Role::Button, true, format!("{name}: more")));
                Chip { rect, dots, resp, more, vol, full, shown, vol_resp, zone }
            })
            .collect();
        let over_head = hover.is_some_and(|at| chips.iter().any(|c| c.zone.contains(at)));
        let wheel_lane = hover.filter(|_| !ctrl && !menus && !stretching && self.volume_pop.is_none()).and_then(|at| {
            rows.lanes.iter().enumerate().position(|(k, row)| {
                if chips[k].zone.contains(at) {
                    return true;
                }
                row.contains(at) && self.selected_audio_piece(&lanes[k]).is_some_and(|piece| {
                    let x = |t| self.view.x(map.to_edit(t), inner.left(), inner.width());
                    (x(piece.start)..x(piece.end)).contains(&at.x)
                })
            })
        });
        if let Some(at) = hover
            && !menus
            && !stretching
            && !over_head
            && self.volume_pop.is_none()
        {
            let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta()));
            if zoom != 1.0 {
                let frac = ((at.x - inner.left()) / inner.width()).clamp(0.0, 1.0) as f64;
                let anchor = self.view.start + (frac * self.view.span as f64) as i64;
                self.view.zoom(anchor, frac, zoom as f64, dur, min_span);
            } else {
                let pan = scroll.x + if wheel_lane.is_none() { scroll.y } else { 0.0 };
                if self.view.span < dur && pan != 0.0 {
                    let shift = (pan as f64 / inner.width() as f64 * self.view.span as f64) as i64;
                    self.view.start -= shift;
                    self.view.clamp(dur, min_span);
                }
            }
        }

        let (primary_down, pointer) = ui.input(|i| (i.pointer.primary_down(), i.pointer.interact_pos()));
        let stretch_changed = if let (Some(Drag::Stretch(stretch)), Some(at)) = (self.drag, pointer) {
            self.update_stretch(&stretch, at.x)
        } else {
            false
        };
        if stretch_changed {
            map = TimeMap::new(&self.edit);
            dur = map.duration().max(1);
        }

        let v = transition.map_or(self.view, |(m, _, t)| m.view.between(self.view, t));
        let axis = Axis {
            left: inner.left(),
            width: inner.width(),
            view: v,
            map: &map,
            source_dur,
            fps: info.fps,
            snapping: !ctrl,
        };
        let x_of = |t| axis.x(t);
        let t_raw = |x| axis.t(x);
        let t_of = |x| t_raw(x).clamp(0, source_dur);
        let mut reset_speed = None;
        if !menus && !stretching
            && let Some(at) = body.hover_pos().or_else(|| body.interact_pointer_pos())
            && rows.video.contains(at)
        {
            let index = self.edit.segment_at(t_of(at.x));
            let segment = self.edit.segments[index];

            let seam = self.edit.segments.windows(2).enumerate()
                .map(|(left, pair)| (left + 1, (x_of(pair[1].start) - at.x).abs()))
                .filter(|(_, distance)| *distance <= 5.0)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            let many = self.edit.segments.len() > 1;
            if let Some((right, _)) = seam {
                let left_piece = &self.edit.segments[right - 1];
                let right_piece = &self.edit.segments[right];
                let (can_merge, tip) = if left_piece.removed || right_piece.removed {
                    (false, "restore both pieces to merge them")
                } else if left_piece.speed != right_piece.speed {
                    (false, "set both pieces to the same speed to merge them")
                } else {
                    (true, "shift-drag to change speed · park playhead between cuts to pick a piece · right-click to merge")
                };
                body.clone().on_hover_text_at_pointer(tip);
                if can_merge && body.secondary_clicked() && self.change(|e| e.merge_kept(right)) {
                    self.select_timeline(None);
                }
            } else if !segment.removed && segment.speed != Speed::NORMAL
                && (self.edit.range.start..self.edit.range.end).contains(&t_of(at.x)) {
                let tip = if many {
                    "double-click to reset to 1× · shift-drag an edge to change speed · right-click to remove"
                } else {
                    "double-click to reset to 1× · shift-drag an edge to change speed"
                };
                body.clone().on_hover_text_at_pointer(tip);
                if !shift && self.drag.is_none() && body.double_clicked() {
                    reset_speed = Some(index);
                } else if body.secondary_clicked() {
                    if many {
                        self.toggle_piece(index);
                    } else {
                        self.track_context = true;
                    }
                }
            } else {
                let tip = if segment.removed {
                    "right-click to restore this piece"
                } else if many {
                    "shift-drag an edge to change speed · right-click to remove"
                } else {
                    "shift-drag an edge to change speed"
                };
                body.clone().on_hover_text_at_pointer(tip);
                if body.secondary_clicked() {
                    if many {
                        self.toggle_piece(index);
                    } else {
                        self.track_context = true;
                    }
                }
            }
        }
        let visible = move |x: f32| x >= inner.left() - 6.0 && x <= inner.right() + 6.0;
        let zoomed = v.span < dur;
        let pieces = match self.job() {
            Some(Job::Video(j)) if j.video == Video::Copy => Some(j.pieces),
            _ => None,
        };

        let markers: Vec<(usize, i64, egui::Response)> = (self.edit.markers.iter().copied().enumerate())
            .filter(|(_, m)| visible(x_of(*m)))
            .map(|(i, m)| {
                let r = Rect::from_center_size(pos2(x_of(m), area.top() + 7.0), vec2(14.0, 14.0));
                let resp = ui.interact(r, Id::unique(("clip-marker", i)), Sense::click());
                (i, m, resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("click jumps here · right-click removes"))
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
        let click_lane = rows.clicks.and_then(|lane| self.clicks_input(ui, look, lane, &axis));
        let key_lane = rows.keys.and_then(|lane| self.keys_input(ui, look, lane, &axis));
        let trim_tip = "drag to trim · shift-drag in the thumbnail strip to change speed · ctrl + drag: no snapping";
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
        let start = grip(self.edit.range.start, "clip-in", trim_tip);
        let end = grip(self.edit.range.end, "clip-out", trim_tip);
        let left = rows.bar.left() + rows.bar.width() * (v.start as f32 / dur as f32);
        let thumb = Rect::from_min_max(
            pos2(left, rows.bar.top()),
            pos2((left + rows.bar.width() * (v.span as f32 / dur as f32)).max(left + 12.0), rows.bar.bottom()),
        );
        let scroll = zoomed.then(|| ui.interact(rows.bar.expand2(vec2(0.0, 4.0)), Id::unique("clip-scroll"), Sense::drag()));

        let mut removed = None;
        for (i, t, r) in &markers {
            if r.clicked() {
                self.selected = Some(Selected::Marker(*i));
                self.jump(*t);
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
                self.toggle_lane_control(k);
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
            if let Some((j, anchor)) = &mut self.volume_pop
                && *j == k
            {
                *anchor = chip.full;
            }
            if let Some(r) = &chip.vol_resp {
                if r.double_clicked() {
                    self.volume_pop = None;
                    self.settle_volume(true);
                    let before = self.edit.clone();
                    if self.set_control_volume(k, FULL_VOLUME) {
                        self.undo.record(before, &self.edit);
                        self.sync_gains();
                    }
                } else if r.clicked() {
                    let open = matches!(self.volume_pop, Some((j, _)) if j == k);
                    self.volume_pop = (!open).then_some((k, chip.full));
                } else if r.dragged() {
                    self.nudge_volume(k, r.drag_delta().x, true);
                }
            }
            if wheel_lane == Some(k) && ui.input(|i| i.zoom_delta()) == 1.0 {
                let wheel = ui.input(|i| i.smooth_scroll_delta.y);
                if wheel != 0.0 {
                    self.nudge_volume(k, wheel / WHEEL_PX, false);
                }
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
        let audio_lane = |at: egui::Pos2| rows.lanes.iter().enumerate().position(|(k, row)| {
            row.contains(at) && !chips[k].zone.contains(at) && lanes[k].source.is_some()
        });
        if !menus && body.secondary_clicked()
            && let Some(at) = body.interact_pointer_pos()
            && rows.lanes.iter().any(|r| r.contains(at))
        {
            if let Some(k) = audio_lane(at).filter(|&k| !self.grouped && !self.edit.lanes[k].cuts.is_empty()) {
                let lane = &self.edit.lanes[k];
                let cut = lane.cuts.iter().copied()
                    .map(|cut| (cut, (x_of(cut) - at.x).abs()))
                    .filter(|&(_, distance)| distance <= 5.0)
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                if let Some((cut, _)) = cut {
                    if lane.piece_mix(cut - 1) != lane.piece_mix(cut) {
                        self.toast(super::Note::Info("match piece volume and mute to merge".into()));
                    } else {
                        self.change(|edit| edit.merge_audio(k, cut));
                    }
                } else {
                    self.silence_audio_piece(k, t_of(at.x), true);
                }
            } else {
                self.track_context = true;
            }
        }
        let shift_edge = if !menus && self.drag.is_none() {
            shift_press(ui)
                .filter(|at| rows.video.contains(*at))
                .filter(|_| {
                    body.is_pointer_button_down_on()
                        || [&start, &end].into_iter().flatten().flatten().any(|r| r.is_pointer_button_down_on())
                })
                .and_then(|at| {
                    self.stretch_edge(at.x, x_of)
                        .map(|(index, kept, edge)| (at.x, index, kept, edge))
                })
        } else {
            None
        };
        let pressed = |r: &Option<Vec<egui::Response>>| {
            r.as_ref().is_some_and(|r| r.iter().any(|r| r.drag_started()))
        };
        if stretching {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        } else if let Some((from, index, kept, edge)) = shift_edge {
            let piece = self.edit.segments[index];
            let anchor = if edge == Edge::Left { kept.end } else { kept.start };
            let stretch = Stretch {
                index,
                piece,
                kept,
                edge,
                from,
                units_per_point: v.span as f64 / f64::from(inner.width()),
                view_start: v.start,
                fit_view: !zoomed,
                output_len: Stretch::duration(piece, kept, piece.speed),
                anchor_offset: Stretch::offset(piece, piece.speed, anchor),
            };
            self.pause();
            self.settle_volume(true);
            self.before_drag = Some(self.edit.clone());
            self.drag = Some(Drag::Stretch(stretch));
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        } else if pressed(&start) {
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
        } else if reset_speed.is_none() && (body.drag_started() || body.clicked()) {
            self.drag = Some(Drag::Head);
            self.click_focus = false;
            self.click_sel.clear();
            self.key_focus = false;
            self.key_sel.clear();
            if body.clicked()
                && let Some(at) = body.interact_pointer_pos()
            {
                let many = self.edit.segments.len() > 1 && rows.span.contains(at);
                let selected = audio_lane(at).filter(|_| !self.grouped).map_or_else(
                    || many.then(|| Selected::Segment(self.edit.segment_at(t_of(at.x)))),
                    |lane| Some(Selected::Audio { source: lanes[lane].source.unwrap(), at: t_of(at.x) }),
                );
                self.select_timeline(selected);
            }
        }

        if let (Some(drag), Some(at)) = (self.drag, pointer) {
            match drag {
                Drag::Start { head } | Drag::End { head } => {
                    let raw = math::snap(t_of(at.x), info.fps);
                    let t = if axis.snapping && (x_of(head) - at.x).abs() <= SNAP_PX {
                        head
                    } else {
                        let keys = if axis.snapping && pieces.is_some() && matches!(drag, Drag::Start { .. }) {
                            self.index.as_ref().map_or(&[][..], |i| i.video.keys.as_slice())
                        } else {
                            &[]
                        };

                        self.snapped(&axis, at.x, raw, &[], keys)
                    };
                    let preview = if matches!(drag, Drag::Start { .. }) {
                        self.edit.range.set_start(t, frame);
                        self.edit.range.start
                    } else {
                        self.edit.range.set_end(t, info.duration, frame);
                        self.edit.range.end - 1
                    };
                    if self.pos != preview || self.play != Play::Paused {
                        self.pos = preview;
                        self.seek(preview, false);
                    }
                }
                Drag::Head => {
                    let range = [self.edit.range.start, self.edit.range.end];
                    let raw = math::snap(t_of(at.x), info.fps);
                    let t = self.snapped(&axis, at.x, raw, &range, &[]).min(source_dur - 1);
                    if t != self.pos {
                        self.pos = t;
                        self.seek(t, false);
                    }
                }
                Drag::Scroll { grab } => {
                    self.view.start = ((at.x - grab - rows.bar.left()) / rows.bar.width() * dur as f32) as i64;
                    self.view.clamp(dur, min_span);
                }
                Drag::Stretch(_) => {}
            }
        }
        let released = if matches!(self.drag, Some(Drag::Stretch(_))) {
            !primary_down
        } else {
            !ui.input(|i| i.pointer.any_down())
        };
        if self.drag.is_some() && released {
            if let Some(Drag::Start { head } | Drag::End { head }) = self.drag {
                self.pos = head;
                self.seek(head, false);
            }
            if let Some(before) = self.before_drag.take() {
                self.undo.record(before, &self.edit);
            }
            if let Some(Drag::Stretch(stretch)) = self.drag {
                let mut to = if stretch.fit_view { View::full(dur) } else { v };
                to.clamp(dur, min_span);
                if to != v {
                    if motion() {
                        self.refit = Some((v, to, self.now));
                    } else {
                        self.view = to;
                    }
                    ui.ctx().request_repaint();
                }
            }
            self.drag = None;
        }
        let focus = match self.drag {
            Some(Drag::Stretch(stretch)) => Some(stretch.index),
            None if !menus => {
                if shift
                    && let Some(at) = hover.filter(|at| rows.video.contains(*at))
                    && (body.contains_pointer()
                        || [&start, &end].into_iter().flatten().flatten().any(|r| r.contains_pointer()))
                {
                    self.stretch_edge(at.x, x_of).map(|(index, _, _)| {
                        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);

                        index
                    })
                } else {
                    self.parked_piece()
                }
            }
            _ => None,
        };

        let timed = |role: Role, label: &str, source: i64| {
            let time = map.to_edit(source);

            WidgetInfo {
                current_text_value: Some(math::fmt_time(time)),
                value: Some(time as f64 / SECOND as f64),
                ..WidgetInfo::labeled(role, true, label)
            }
        };
        let timeline_pos = match self.drag {
            Some(Drag::Start { head } | Drag::End { head }) => head,
            _ => self.pos,
        };
        body.widget_info(|| timed(Role::Slider, "playhead", timeline_pos));
        if let Some(r) = start.as_ref().and_then(|r| r.first()) {
            r.widget_info(|| timed(Role::Slider, "trim start", self.edit.range.start));
        }
        if let Some(r) = end.as_ref().and_then(|r| r.first()) {
            r.widget_info(|| timed(Role::Slider, "trim end", self.edit.range.end));
        }
        for (i, t, r) in &markers {
            if Some(*i) != removed {
                r.widget_info(|| timed(Role::Button, "marker", *t));
            }
        }
        if let Some(s) = &scroll {
            s.widget_info(|| WidgetInfo::slider(true, self.view.start as f64 / SECOND as f64, "timeline scroll"));
        }

        let (now, animate) = (self.now, motion());
        let head_t = match self.drag {
            _ if matches!(self.play, Play::Running { .. }) => self.playhead.play(now, self.pos),
            Some(Drag::Head | Drag::Start { .. } | Drag::End { .. }) => {
                self.playhead.set(timeline_pos);
                timeline_pos
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
        if let Some((_, first)) = self.thumbs.first().or_else(|| self.overview.first()) {
            let clip = lane_clip(rows.video.shrink(1.0));
            let [tw, th] = first.size();
            let h = rows.video.height() - 2.0;
            let w = (h * tw as f32 / th.max(1) as f32).max(8.0);
            let every = scan::interval(source_dur);
            let overview_interval = scan::overview_interval(source_dur);
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            let tint = Color32::WHITE.gamma_multiply(if light { 0.95 } else { 0.85 });
            let (x_start, x_end) = (x_of(0), x_of(source_dur));
            let mut x = x_start.max(inner.left() - w);
            x -= (x - x_start).rem_euclid(w);
            while x < inner.right().min(x_end) {
                let t = t_of(x + w / 2.0);
                let i = self.thumbs.partition_point(|(ts, _)| *ts <= t).saturating_sub(1);
                let tex = self.thumbs.get(i)
                    .filter(|(ts, _)| t.abs_diff(*ts) <= (2 * every) as u64)
                    .or_else(|| {
                        let step = if self.recorded_overview { i64::MAX } else { overview_interval? };
                        let next = self.overview.partition_point(|(ts, _)| *ts < t);
                        [next.checked_sub(1).and_then(|i| self.overview.get(i)), self.overview.get(next)]
                            .into_iter()
                            .flatten()
                            .min_by_key(|(ts, _)| t.abs_diff(*ts))
                            .filter(|(ts, _)| t.abs_diff(*ts) <= step as u64)
                    });
                if let Some((_, tex)) = tex {
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
        if pieces.is_some()
            && let Some(keys) = self.index.as_ref().map(|i| &i.video.keys)
            && keys.len() > 1
        {
            let first = keys.partition_point(|&key| key < t_raw(inner.left() - 6.0));
            let end = keys.partition_point(|&key| key <= t_raw(inner.right() + 6.0));
            let visible_keys = &keys[first..end];
            if inner.width() > 4.0 * visible_keys.len() as f32 {
                for &key in visible_keys {
                    painter.vline(
                        x_of(key),
                        rows.video.bottom() - 4.0..=rows.video.bottom(),
                        Stroke::new(1.0, Color32::WHITE.gamma_multiply(0.55)),
                    );
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
            let (x_start, stop) = (inner.left().max(x_of(0)), inner.right().min(x_of(source_dur)));
            let Some(source) = lanes[k].source else {
                let mut x = x_start;
                while x < stop {
                    clip.hline(x..=(x + 4.0).min(stop), l.center().y, Stroke::new(1.0, p.t3.gamma_multiply(fade)));
                    x += 8.0;
                }
                continue;
            };
            if self.audio_selected_at(&lanes[k]).is_some() {
                let piece = self.selected_audio_piece(&lanes[k]).unwrap_or(math::Range { start: 0, end: source_dur });
                let span = Rect::from_min_max(pos2(x_of(piece.start), l.top()), pos2(x_of(piece.end), l.bottom()));
                clip.rect_filled(span, 0, p.acc.gamma_multiply(0.14));
            }
            if let Some(peaks) = self.peaks.get(source).filter(|p| !p.is_empty()) {
                let wave = Waveform::recorded(ui.ctx(), look, &self.path, source, lanes[k].volume, lanes[k].muted);
                let rect = l.with_min_x(x_start).with_max_x(stop);
                wave.paint(&clip, rect, peaks, t_of, &lanes[k].gains);
            }
            for gain in lanes[k].gains.iter() {
                let span = Rect::from_min_max(pos2(x_of(gain.range.start), l.top()), pos2(x_of(gain.range.end), l.bottom()));
                if span.intersects(inner) {
                    clip.rect_filled(span, 0, p.acc.gamma_multiply(0.2));
                    clip.with_clip_rect(span.intersect(*l)).text(
                        pos2(span.left() + 4.0, span.center().y), egui::Align2::LEFT_CENTER,
                        if gain.muted || gain.volume == 0 { "silent".into() } else { format!("{}%", gain.volume) }, mono(11.0), p.t1,
                    );
                }
            }
            for &at in lanes[k].cuts.iter() {
                let x = x_of(at);
                clip.vline(x, l.y_range(), Stroke::new(1.0, p.t2));
            }
        }

        if let Some(lane) = &click_lane {
            self.clicks_paint(ui, look, lane, &axis, &painter);
        }
        if let Some(lane) = &key_lane {
            self.keys_paint(ui, look, lane, &axis, &painter);
        }
        let shade = Color32::from_black_alpha(if light { 70 } else { 158 });
        let phase = hatch_phase(now, animate, ui.ctx().pixels_per_point());
        let stripes = Color32::from_gray(if light { 110 } else { 96 });
        let track_clip = painter.clip_rect();
        let focus_span = focus.map(|_| (x_of(self.edit.range.start), x_of(self.edit.range.end)));
        let mut hatch_visible = false;
        for (index, s) in self.edit.segments.iter().enumerate() {
            let (left, right) = (x_of(s.start), x_of(s.end));
            if right <= track_clip.left() || left >= track_clip.right() {
                continue;
            }
            if focus != Some(index)
                && !s.removed
                && let Some((start, end)) = focus_span
            {
                let (left, right) = (left.max(start), right.min(end));
                if right > left {
                    for band in std::iter::once(rows.video).chain(rows.lanes.iter().copied()) {
                        let rect = Rect::from_min_max(pos2(left, band.top()), pos2(right, band.bottom()));
                        lane_clip(band).rect_filled(rect, 0, PIECE_DIM);
                    }
                }
            }
            let opacity = ease(anim(ui, Id::unique(("clip-removed", s.start)), s.removed, 0.24));
            if opacity <= 0.0 {
                continue;
            }
            let fill = shade.gamma_multiply(opacity);
            let stroke = stripes.gamma_multiply(opacity);
            for band in &rows.bands {
                let r = Rect::from_min_max(pos2(left, band.top()), pos2(right, band.bottom()));
                let clip = lane_clip(*band);
                let shaded = r.intersect(clip.clip_rect());
                if shaded.is_positive() {
                    clip.rect_filled(shaded, 0, fill);
                    hatch(&clip, r, stroke, phase);
                    hatch_visible = true;
                }
            }
        }
        if animate && hatch_visible {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(40));
        }

        for band in &rows.bands {
            let clip = lane_clip(Rect::from_min_max(pos2(area.left(), band.top()), pos2(area.right(), band.bottom())));
            let span_rect = |a: i64, b: i64| Rect::from_min_max(pos2(x_of(a), band.top()), pos2(x_of(b), band.bottom()));
            let (x0, x1) = (x_of(trim_t[0]), x_of(trim_t[1]));
            clip.rect_filled(Rect::from_min_max(pos2(area.left(), band.top()), pos2(x0, band.bottom())), 0, shade);
            clip.rect_filled(Rect::from_min_max(pos2(x1, band.top()), pos2(area.right(), band.bottom())), 0, shade);
            if let Some(pieces) = &pieces {
                for pc in pieces.iter().filter(|pc| pc.lead < pc.start) {
                    let r = span_rect(pc.lead, pc.start);
                    clip.rect_filled(r, 0, p.surf.gamma_multiply(0.35));
                    hatch(&clip, r, p.acc.gamma_multiply(0.5), 0.0);
                }
            }
            for t in self.edit.splits() {
                let x = x_of(t);
                if visible(x) {
                    clip.vline(x, band.y_range(), Stroke::new(6.0, Color32::BLACK));
                    clip.vline(x, band.y_range(), Stroke::new(4.0, p.acc));
                }
            }
        }

        let video = lane_clip(rows.video.shrink(1.0));
        for (i, segment) in self.edit.segments.iter().enumerate() {
            let returning = transition.filter(|(m, _, _)| m.index == i);
            if segment.removed || (segment.speed == Speed::NORMAL && returning.is_none()) {
                continue;
            }
            let mut video = video.clone();
            if let Some((_, _, t)) = returning {
                video.multiply_opacity(1.0 - t);
            }
            let left = x_of(segment.start.max(self.edit.range.start)).max(video.clip_rect().left());
            let right = x_of(segment.end.min(self.edit.range.end)).min(video.clip_rect().right());
            if right <= left {
                continue;
            }

            let top = rows.video.top() + 4.0;
            let speed = returning.map_or(segment.speed, |(_, speed, _)| speed);
            let text = video.layout_no_wrap(speed.label(), mono(11.0), p.t1);
            let size = text.size() + vec2(10.0, 4.0);
            if size.x + 8.0 <= right - left {
                let badge = Rect::from_min_size(pos2(left + 4.0, top), size);
                video.rect_filled(badge, 4, p.surf);
                video.galley(badge.min + vec2(5.0, 2.0), text, p.t1);
            } else {
                video.hline(left..=right, top + 1.0, Stroke::new(2.0, p.acc));
            }
        }

        for (k, chip) in chips.iter().enumerate() {
            let fade = fades[k];
            let (volume, muted) = self.lane_controls(&lanes[k]);
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
                let label = if self.selected_audio_piece(&lanes[k]).is_some() { "· piece muted" } else { MUTED };
                clip.text(pos2(at.x + nw + 6.0, at.y), egui::Align2::LEFT_CENTER, label, sans(12.0), p.t3);
            }
            if chip.shown > 0.01 {
                let vol = volume;
                let popped = matches!(self.volume_pop, Some((j, _)) if j == k);
                let lit = chip.vol_resp.as_ref().map_or(0.0, |r| anim(ui, r.id.with("hov"), r.hovered() || r.dragged() || popped, 0.12));
                let pill = lane_clip(chip.vol);
                let a = chip.shown * fade.max(0.55);
                pill.rect_filled(chip.vol, 5, p.ctl.gamma_multiply(0.85 * fade.max(0.6)));
                pill.rect_filled(chip.vol, 5, p.line2.gamma_multiply(lit));
                let gauge = chip.full.height() >= 20.0;
                let y = chip.full.center().y - if gauge { 2.0 } else { 0.0 };
                let ink = if vol == FULL_VOLUME { mix(p.t3, p.t2, lit) } else { mix(p.t2, p.t1, lit) };
                pill.text(pos2(chip.full.center().x, y), egui::Align2::CENTER_CENTER, format!("{vol}%"), mono(11.0), ink.gamma_multiply(a));
                if gauge {
                    let level = ui.ctx().animate_value_with_time(
                        Id::unique(("clip-lane-vol-level", k)),
                        vol as f32 / MAX_VOLUME as f32,
                        if animate { 0.15 } else { 0.0 },
                    );
                    let line = Rect::from_min_max(pos2(chip.full.left() + 7.0, chip.full.bottom() - 6.0), pos2(chip.full.right() - 7.0, chip.full.bottom() - 4.0));
                    let fill = lanes[k].source.map_or(p.t2, |s| WAVES[s % WAVES.len()]);
                    pill.rect_filled(line, 1, p.line2.gamma_multiply(a));
                    pill.rect_filled(Rect::from_min_max(line.min, pos2(line.left() + line.width() * level, line.bottom())), 1, fill.gamma_multiply(a));
                }
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

        for (i, t, resp) in &markers {
            if Some(*i) == removed {
                continue;
            }

            let x = x_of(*t);
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
        if let (Some(Drag::Stretch(stretch)), Some(at)) = (self.drag, pointer)
            && let Some(piece) = self.edit.segments.get(stretch.index)
        {
            let edge = x_of(stretch.moving_edge());
            let speed = piece.speed;
            let duration = math::fmt_time(speed.output_len(stretch.kept.len()));
            let label = painter.layout_no_wrap(format!("{} · {duration}", speed.label()), mono(11.0), p.t1);
            let size = label.size() + vec2(12.0, 6.0);
            let left = (at.x - size.x / 2.0).clamp(rows.video.left(), (rows.video.right() - size.x).max(rows.video.left()));
            let badge = Rect::from_min_size(pos2(left, rows.video.bottom() - size.y - 4.0), size);
            if visible(edge) {
                painter.vline(edge, rows.video.y_range(), Stroke::new(2.0, p.acch));
            }
            painter.rect_filled(badge, 4, p.surf);
            painter.galley(badge.min + vec2(6.0, 3.0), label, p.t1);
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
        if let Some(i) = reset_speed {
            let piece = self.edit.segments[i];
            self.change(|e| e.set_speed(i, Speed::NORMAL));
            self.reset_motion = motion().then_some(ResetMotion { index: i, piece, view: v, at: self.now });
            self.view.clamp(TimeMap::new(&self.edit).duration(), min_span);
            self.selected = Some(Selected::Segment(i));
            self.reveal(self.pos);
            ui.ctx().request_repaint();
        }
        self.settle_volume(false);
    }

    /// Moves lane `k`'s volume by `delta` percent, as part of one gesture.
    fn nudge_volume(&mut self, k: usize, delta: f32, sticky: bool) {
        let Some(vol) = self.edit.lanes.get(k).map(|l| self.lane_controls(l).0) else { return };
        let rest = self.nudge.as_ref().filter(|n| n.lane == k).map_or(0.0, |n| n.rest);
        let (v, rest) = step_volume(vol, rest, delta, sticky);
        self.slide_volume(k, v, rest);
    }

    /// Sets lane `k`'s volume as part of one gesture, which [`ClipApp::settle_volume`] makes one undo step.
    fn slide_volume(&mut self, k: usize, v: u16, rest: f32) {
        if self.nudge.as_ref().is_some_and(|n| n.lane != k) {
            self.settle_volume(true);
        }
        let before = self.nudge.take().map_or_else(|| self.edit.clone(), |n| n.before);
        if self.set_control_volume(k, v) {
            self.sync_gains();
        }
        self.nudge = Some(Nudge { lane: k, before, at: self.now, rest });
    }

    /// Ends a volume gesture as one undo step: once it has rested for [`SETTLE`], or `now`.
    pub(super) fn settle_volume(&mut self, now: bool) {
        if self.nudge.as_ref().is_some_and(|n| now || self.now - n.at >= SETTLE)
            && let Some(n) = self.nudge.take()
        {
            self.undo.record(n.before, &self.edit);
        }
    }

    /// The open volume slider: 0 to 200 %, catching at 100 % unless ctrl is held.
    pub(super) fn volume_popup(&mut self, ctx: &egui::Context, look: &Look) {
        let Some((k, anchor)) = self.volume_pop else { return };
        let Some(lane) = self.edit.lanes.get(k) else {
            self.volume_pop = None;
            return;
        };
        let (volume, _) = self.lane_controls(lane);
        let source = lane.source;
        let p = look.p;
        let screen = ctx.content_rect();
        let below = anchor.bottom() + 6.0;
        let y = if below + POP_H > screen.bottom() - 4.0 { anchor.top() - 6.0 - POP_H } else { below };
        let x = (anchor.center().x - POP_W / 2.0).clamp(screen.left() + 4.0, screen.right() - 4.0 - POP_W);
        let rect = Rect::from_min_size(pos2(x, y), vec2(POP_W, POP_H));
        let track = Rect::from_min_max(pos2(rect.left() + 16.0, rect.center().y - 10.0), pos2(rect.right() - 60.0, rect.center().y + 10.0));
        let full_x = track.left() + track.width() * FULL_VOLUME as f32 / MAX_VOLUME as f32;
        let (mut set, mut wheel) = (None, 0.0);
        egui::Area::new(Id::unique("clip-volume-pop")).order(Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
            ui.painter().rect(rect, 8, p.surf, Stroke::new(1.0, p.line2), StrokeKind::Inside);
            let resp = ui
                .interact(track, Id::unique("clip-volume-slider"), Sense::click_and_drag())
                .on_hover_cursor(CursorIcon::PointingHand);
            resp.widget_info(|| WidgetInfo::slider(true, volume as f64, "volume"));
            let ctrl = ui.input(|i| i.modifiers.ctrl);
            if resp.double_clicked() {
                set = Some(FULL_VOLUME);
            } else if (resp.dragged() || resp.clicked())
                && let Some(at) = resp.interact_pointer_pos()
            {
                let v = (((at.x - track.left()) / track.width()).clamp(0.0, 1.0) * MAX_VOLUME as f32).round() as u16;
                set = Some(if !ctrl && v.abs_diff(FULL_VOLUME) <= 4 { FULL_VOLUME } else { v });
            }
            if !ctrl && ui.rect_contains_pointer(rect) {
                wheel = ui.input(|i| i.smooth_scroll_delta.y);
            }
            let lit = anim(ui, resp.id.with("hov"), resp.hovered() || resp.dragged(), 0.1);
            let painter = ui.painter();
            let cy = track.center().y;
            let kx = track.left() + track.width() * volume as f32 / MAX_VOLUME as f32;
            let line = Rect::from_min_max(pos2(track.left(), cy - 1.5), pos2(track.right(), cy + 1.5));
            painter.rect_filled(line, 2, p.line2);
            let fill = source.map_or(p.t2, |s| WAVES[s % WAVES.len()]);
            painter.rect_filled(Rect::from_min_max(line.min, pos2(kx, line.bottom())), 2, fill);
            painter.vline(full_x, cy - 5.0..=cy + 5.0, Stroke::new(1.0, p.t3));
            painter.circle_filled(pos2(kx, cy), 5.5 + lit, p.t1);
            let label = format!("{volume}%");
            painter.text(pos2(rect.right() - 14.0, cy), egui::Align2::RIGHT_CENTER, label, mono(12.0), p.t1);
        });
        if let Some(v) = set {
            self.slide_volume(k, v, 0.0);
        }
        if wheel != 0.0 {
            self.nudge_volume(k, wheel / WHEEL_PX, false);
        }
        let outside = ctx.input(|i| {
            i.pointer.any_click() && i.pointer.interact_pos().is_some_and(|at| !rect.contains(at) && !anchor.contains(at))
        });
        if outside {
            self.volume_pop = None;
        }
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
        let items = [
            (LaneItem::Rename, true),
            (LaneItem::Up, k > 0),
            (LaneItem::Down, k + 1 < n),
            (LaneItem::Remember, self.edit.lanes[k].source.is_some()),
            (LaneItem::Group, true),
            (LaneItem::Delete, true),
        ];
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
                let label = item.label(self.grouped);
                resp.widget_info(|| WidgetInfo::labeled(Role::MenuItem, on, label));
                let hov = anim(ui, resp.id.with("hov"), on && resp.hovered(), 0.1);
                let painter = ui.painter();
                painter.rect_filled(r, 5, p.ctlh.gamma_multiply(hov));
                let color = match (item, on) {
                    (_, false) => p.t3,
                    (LaneItem::Delete, _) => red,
                    _ => mix(p.t2, p.t1, hov),
                };
                item.paint_icon(painter, pos2(r.left() + 14.0, r.center().y), color, self.grouped);
                painter.text(pos2(r.left() + 29.0, r.center().y), egui::Align2::LEFT_CENTER, label, sans(13.0), color);
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
            LaneItem::Group => self.toggle_group(),
            LaneItem::Remember => {
                let lane = &self.edit.lanes[k];
                let name = lane.default_name(&tracks).trim().to_lowercase();
                let volume = lane.volume;
                let save = super::Config::load().and_then(|mut cfg| {
                    cfg.editor_track_volumes.retain(|key, _| key.trim().to_lowercase() != name);
                    if volume != FULL_VOLUME {
                        cfg.editor_track_volumes.insert(name.clone(), volume);
                    }
                    cfg.save()
                });
                match save {
                    Ok(()) => {
                        self.saved.track_volumes.insert(name, volume);
                        self.toast(super::Note::Info(format!("remembered track volume · {volume}%")));
                    }
                    Err(err) => self.toast(super::Note::Failed(format!("couldn't remember volume: {err}"))),
                }
            }
            LaneItem::Delete => {
                self.renaming = None;
                if self.audio_selected_at(&self.edit.lanes[k]).is_some() {
                    self.select_timeline(None);
                }
                self.change(|e| e.delete_lane(k));
                self.sync_gains();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Config, Opened, Saved};
    use super::*;

    fn painted_hatch(r: Rect, clip: Rect, now: f64, animate: bool) -> Vec<(Rect, [egui::Pos2; 2])> {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(clip.expand(4.0)),
            ..Default::default()
        };

        let mut frame = ctx.run_ui(input, |ui| {
            hatch(&ui.painter().with_clip_rect(clip), r, MARKER, hatch_phase(now, animate, ctx.pixels_per_point()));
        });
        frame.textures_delta.clear();

        frame.shapes.into_iter().filter_map(|c| match c.shape {
            Shape::LineSegment { points, stroke } if stroke.color == MARKER => Some((c.clip_rect, points)),
            _ => None,
        }).collect()
    }

    #[test]
    fn moving_stripes_preserve_pixel_coverage_at_fractional_display_scales() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let pitch = (HATCH_STEP * scale).round();
            for tick in 0..100 {
                let pixels = hatch_phase(f64::from(tick) * 0.04, true, scale) * scale;
                assert!((pixels - pixels.round()).abs() < 0.00001);
                assert!((0.0..pitch).contains(&pixels));
            }
        }
    }

    #[test]
    fn hatch_moves_and_wraps_inside_the_visible_clip() {
        let r = Rect::from_min_max(pos2(-1_000_000_000.0, 0.0), pos2(120.0, 100_000_000.0));
        let clip = Rect::from_min_max(pos2(100.0, 10.0), pos2(130.0, 30.0));
        let visible = r.intersect(clip);
        let at = |now, animate| painted_hatch(r, clip, now, animate);

        let initial = at(0.0, true);
        let shifted = at(0.2, true);
        let wrapped = at(1.6, true);
        let reduced_motion = at(0.2, false);
        let offscreen = painted_hatch(r, clip.translate(vec2(20.0, 0.0)), 0.2, true);

        assert_eq!(initial.len(), 6, "the visible clip bounds the stripe count");
        assert_eq!(shifted.len(), initial.len());
        assert_eq!(shifted, wrapped, "one stripe period wraps to the same geometry");
        assert_eq!(reduced_motion, initial);
        assert!(offscreen.is_empty());
        for ((initial_clip, [bottom, top]), (shifted_clip, [moved_bottom, moved_top])) in initial.iter().zip(&shifted) {
            assert_eq!(*initial_clip, visible);
            assert_eq!(*shifted_clip, visible);
            assert_eq!((bottom.y, top.y), (31.0, 9.0));
            assert_eq!(top.x - bottom.x, bottom.y - top.y);
            assert_eq!(*moved_bottom, *bottom + vec2(1.0, 0.0));
            assert_eq!(*moved_top, *top + vec2(1.0, 0.0));
        }
    }

    #[test]
    fn a_new_split_is_visible_on_both_sides_of_the_playhead() {
        let ctx = egui::Context::default();
        glint_ui::theme::fonts(&ctx);
        let accent = [0xE5, 0x40, 0x2F];
        let look = Look { p: glint_ui::widgets::palette(false, accent), keyboard: false };
        let file = Opened {
            path: Default::default(),
            info: Ok(Info {
                duration: 10 * SECOND,
                size: (1920, 1080),
                fps: 60.0,
                codec: None,
                tracks: Vec::new(),
                clicks: None,
                keys: None,
                snap: None,
            }),
            index: None,
        };
        let cfg = Config::default();
        let mut app = ClipApp::new(Some(file), cfg.effort, cfg.after_trim, String::new(), Saved::default(), accent);
        app.pos = 5 * SECOND;
        app.playhead.set(app.pos);
        let area = Rect::from_min_size(pos2(16.0, 16.0), vec2(320.0, height(0, LANE_MAX, false, false)));
        let video = rows(area, 0, LANE_MAX, false, false).video;
        let x = area.center().x;
        let input = egui::RawInput {
            screen_rect: Some(area.expand(12.0)),
            events: vec![egui::Event::Key {
                key: egui::Key::S,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };

        let mut frame = ctx.run_ui(input, |ui| {
            app.keys(ui.ctx());
            app.timeline(ui, &look, area, LANE_MAX, None);
        });
        frame.textures_delta.clear();
        assert_eq!(app.edit.splits().next(), Some(app.pos));

        let (head, head_width) = frame.shapes.iter().enumerate().find_map(|(i, c)| match &c.shape {
            Shape::LineSegment { points: [top, bottom], stroke }
                if top.x == x && bottom.x == x && top.y < video.top() && bottom.y >= video.bottom()
                    && stroke.color == look.p.t1 => Some((i, stroke.width)),
            _ => None,
        }).expect("the playhead over the new split");
        let seam = frame.shapes[..head].iter().any(|c| matches!(&c.shape,
            Shape::LineSegment { points: [top, bottom], stroke }
                if top.x == x && bottom.x == x && top.y == video.top() && bottom.y == video.bottom()
                    && stroke.color == look.p.acc && stroke.width > head_width
        ));
        assert!(seam, "the vertical split stays visible behind the playhead");
        assert!(!frame.shapes.iter().any(|c| matches!(&c.shape,
            Shape::LineSegment { points: [left, right], .. }
                if left.y == right.y && video.y_range().contains(left.y) && left.x < x && right.x > x
        )), "no crossbar across the split");
    }

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
    fn volume_pulls_carry_fractions_and_catch_at_full() {
        let near = |(v, rest): (u16, f32), want: (u16, f32)| v == want.0 && (rest - want.1).abs() < 1e-4;
        assert_eq!(step_volume(100, 0.0, 5.0, false), (105, 0.0), "a wheel notch");
        assert!(near(step_volume(50, 0.0, 0.3, false), (50, 0.3)), "a slow scroll adds up");
        assert!(near(step_volume(50, 0.3, 0.3, false), (51, -0.4)));
        assert_eq!(step_volume(3, 0.0, -10.0, false), (0, 0.0));
        assert_eq!(step_volume(195, 0.0, 30.0, false), (MAX_VOLUME, 0.0));

        assert_eq!(step_volume(100, 0.0, 4.0, true), (100, 4.0), "a drag holds at 100 %");
        assert_eq!(step_volume(100, 4.0, 3.0, true), (107, 0.0));
        assert_eq!(step_volume(100, -4.0, -3.0, true), (93, 0.0));
        assert_eq!(step_volume(95, 0.0, 9.0, true), (100, 0.0), "and catches one passing through");
        assert_eq!(step_volume(95, 0.0, 9.0, false), (104, 0.0));
        assert_eq!(step_volume(95, 0.0, 5.0, true), (100, 0.0));
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
