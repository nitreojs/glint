//! Drawing the gallery: the toolbar, the virtualized grid (only the rows in view are touched),
//! tiles that fade in as their thumbnails come and lift under the pointer, the grown preview, and
//! the motion when the list changes (tiles slide to their new places, gone ones fade away).

use std::collections::{HashMap, HashSet};

use eframe::egui::epaint::{RectShape, Shadow};
use eframe::egui::scroll_area::ScrollSource;
use eframe::egui::{
    self, Align, Align2, Color32, CursorIcon, Id, Layout, Order, Painter, Rect, Response, Role, Sense, Stroke,
    StrokeKind, TextureId, Ui, UiBuilder, WidgetInfo, pos2, vec2,
};
use glint_core::format::{human_duration, human_size};
use glint_core::gallery::grid::{self, Grid, Line, Spec};
use glint_core::gallery::thumb::Meta;
use glint_core::gallery::{Filter, Kind};

use super::{Gallery, Menu, Preview, TILE_RANGE};
use crate::widgets::{self, Look, anim, ease, medium, mix, mono, motion, sans};

const PAD: f32 = 24.0;
const GAP: f32 = 10.0;
const HEADING: f32 = 40.0;
const DAY_GAP: f32 = 30.0;
const ASPECT: f32 = 0.625;
const RADIUS: u8 = 6;
/// How long tiles take to slide, fade in, lift and grow.
const SLIDE: f32 = 0.32;
const FADE_IN: f32 = 0.28;
const LIFT: f32 = 0.12;
pub(super) const GROW: f32 = 0.24;
/// Tiles that would slide further than this fade in at their new place instead.
const MAX_SLIDE: f32 = 900.0;
/// Glass under badges on a thumbnail.
const GLASS: Color32 = Color32::from_rgba_premultiplied(12, 12, 12, 178);
const HEADING_BIT: u64 = 1 << 63;

/// What moves when the list changes, by key (an item's id, or a heading's).
#[derive(Default)]
pub(super) struct Flip {
    /// Where everything was drawn last frame, in content coordinates.
    last: HashMap<u64, Rect>,
    /// The headings drawn last frame, to fade out the ones that go.
    labels: HashMap<u64, String>,
    moves: HashMap<u64, (Rect, f64)>,
    appear: HashMap<u64, f64>,
    ghosts: Vec<Ghost>,
    /// The list changed: this frame's layout is compared with the last.
    armed: bool,
}

struct Ghost {
    key: u64,
    rect: Rect,
    start: f64,
    label: Option<String>,
}

impl Flip {
    fn start(&mut self, present: &HashSet<u64>, now: f64) {
        self.armed = true;
        for (key, rect) in &self.last {
            if !present.contains(key) {
                let label = self.labels.get(key).cloned();
                self.ghosts.push(Ghost { key: *key, rect: *rect, start: now, label });
            }
        }
    }

    /// Where `key` is drawn for its place `target`, and how far it has faded in.
    fn place(&mut self, key: u64, target: Rect, now: f64) -> (Rect, f32) {
        if self.armed {
            match self.last.get(&key) {
                Some(from) if *from != target && (from.center() - target.center()).length() < MAX_SLIDE => {
                    self.moves.insert(key, (*from, now));
                }
                Some(_) => {}
                None => {
                    self.appear.insert(key, now);
                }
            }
        }
        let progress = |start: f64, secs: f32| if motion() { ((now - start) as f32 / secs).clamp(0.0, 1.0) } else { 1.0 };
        let mut rect = target;
        if let Some(&(from, start)) = self.moves.get(&key) {
            let t = progress(start, SLIDE);
            rect = Rect::from_min_max(from.min.lerp(target.min, ease(t)), from.max.lerp(target.max, ease(t)));
            if t >= 1.0 {
                self.moves.remove(&key);
            }
        }
        let mut shown = 1.0;
        if let Some(&start) = self.appear.get(&key) {
            shown = ease(progress(start, FADE_IN));
            if shown >= 1.0 {
                self.appear.remove(&key);
            }
        }
        (rect, shown)
    }

    fn busy(&self) -> bool {
        !self.moves.is_empty() || !self.appear.is_empty() || !self.ghosts.is_empty()
    }
}

/// The texture region that fills `rect` from a `size` texture, cut from the middle (wide) or the
/// top (tall: a page reads from the top).
fn cover(size: [usize; 2], rect: Rect) -> Rect {
    let ta = size[0] as f32 / size[1].max(1) as f32;
    let ra = rect.width() / rect.height().max(1.0);
    if ta > ra {
        let w = ra / ta;
        Rect::from_min_max(pos2((1.0 - w) / 2.0, 0.0), pos2((1.0 + w) / 2.0, 1.0))
    } else {
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, ta / ra))
    }
}

fn lerp_rect(a: Rect, b: Rect, t: f32) -> Rect {
    Rect::from_min_max(a.min.lerp(b.min, t), a.max.lerp(b.max, t))
}

fn image(painter: &Painter, rect: Rect, radius: f32, tex: TextureId, uv: Rect, alpha: f32) {
    let tint = Color32::WHITE.gamma_multiply(alpha);
    painter.add(RectShape::filled(rect, radius, tint).with_texture(tex, uv));
}

/// A pill of glass with `text` at a corner of `rect`.
fn badge(painter: &Painter, rect: Rect, text: &str, right: bool, alpha: f32) {
    let g = painter.layout_no_wrap(text.to_string(), mono(11.0), Color32::WHITE.gamma_multiply(alpha));
    let size = g.size() + vec2(12.0, 6.0);
    let at = if right { pos2(rect.right() - 6.0 - size.x, rect.bottom() - 6.0 - size.y) } else { rect.left_bottom() + vec2(6.0, -6.0 - size.y) };
    let r = Rect::from_min_size(at, size);
    painter.rect_filled(r, 4, GLASS.gamma_multiply(alpha));
    painter.galley(r.center() - g.size() / 2.0, g, Color32::WHITE);
}

/// Where a preview of `meta`'s picture grows to from `tile`, inside `panel`.
pub(super) fn preview_rect(tile: Rect, meta: Meta, panel: Rect, ppp: f32) -> Rect {
    let aspect = if meta.width > 0 && meta.height > 0 { meta.width as f32 / meta.height as f32 } else { tile.width() / tile.height() };
    // small pictures grow up to twice their size, no more
    let limit = if meta.width > 0 { vec2(meta.width as f32, meta.height as f32) * 2.0 / ppp } else { vec2(f32::INFINITY, f32::INFINITY) };
    let bounds = panel.shrink(16.0);
    grid::grown(tile, aspect, bounds, 2.6, 520.0f32.min(bounds.width() * 0.8), limit)
}

/// What the filter calls a kind.
fn chip(kind: Kind) -> (Filter, &'static str) {
    match kind {
        Kind::Shot => (Filter::Shots, "screenshots"),
        Kind::Recording => (Filter::Recordings, "recordings"),
        Kind::Gif => (Filter::Gifs, "gifs"),
    }
}

/// A square button with a gear in it.
fn gear(ui: &mut Ui, look: &Look, rect: Rect) -> Response {
    let p = &look.p;
    let resp = ui.interact(rect, Id::unique("gallery-settings"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "settings"));
    let h = anim(ui, resp.id.with("hov"), resp.hovered(), 0.08);
    let d = anim(ui, resp.id.with("prs"), resp.is_pointer_button_down_on(), 0.08);
    let painter = ui.painter();
    painter.rect_filled(rect, 6, p.hov.gamma_multiply(h * (1.0 - d)));
    painter.rect_filled(rect, 6, p.prs.gamma_multiply(d));
    let color = mix(p.t2, p.t1, h);
    let c = rect.center();
    // turns a little toward the pointer
    let turn = std::f32::consts::FRAC_PI_8 * ease(h);
    for k in 0..8 {
        let a = turn + k as f32 * std::f32::consts::FRAC_PI_4;
        let dir = vec2(a.cos(), a.sin());
        painter.line_segment([c + dir * 5.2, c + dir * 7.4], Stroke::new(2.4, color));
    }
    painter.circle_stroke(c, 5.0, Stroke::new(1.6, color));
    painter.circle_stroke(c, 1.8, Stroke::new(1.4, color));
    if look.keyboard && resp.has_focus() {
        painter.rect_stroke(rect.expand(2.0), 8.0, Stroke::new(2.0, p.focus), StrokeKind::Middle);
    }
    resp.on_hover_text("settings")
}

impl Gallery {
    pub(super) fn toolbar(&mut self, ui: &mut Ui, look: &Look, rect: Rect) {
        let p = look.p;
        let inner = Rect::from_min_max(pos2(rect.left() + 16.0, rect.top() + 4.0), pos2(rect.right() - 16.0, rect.bottom() - 16.0));
        let mut row = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::left_to_right(Align::Center)));
        let kinds = self.setup.kinds;
        if kinds.len() > 1 {
            let before = self.filter;
            // "all", then each kind: at most four, so no allocation a frame
            let mut options = [(Filter::All, "all"); 4];
            for (slot, &k) in options[1..].iter_mut().zip(kinds) {
                *slot = chip(k);
            }
            let options = &options[..(kinds.len() + 1).min(4)];
            let width = (100.0 * options.len() as f32).min(inner.width() * 0.6);
            widgets::segmented(&mut row, look, &mut self.filter, options, Some(width));
            if self.filter != before {
                self.refilter();
            }
        }
        row.with_layout(Layout::right_to_left(Align::Center), |row| {
            let right = row.max_rect().right();
            let gear_rect = Rect::from_center_size(pos2(right - 16.0, inner.center().y), vec2(32.0, 32.0));
            if gear(row, look, gear_rect).clicked() {
                self.open_settings();
            }
            let search_w = 240.0_f32.min((row.available_width() - 48.0).max(120.0));
            let mut field = row.new_child(UiBuilder::new().max_rect(Rect::from_min_size(
                pos2(gear_rect.left() - 8.0 - search_w, inner.center().y - 16.0),
                vec2(search_w, 32.0),
            )));
            let before = self.query.clone();
            let resp = widgets::text_field(&mut field, look, &mut self.query, "search", false);
            self.search_id = Some(resp.id);
            if self.query != before {
                self.refilter();
            }
            row.add_space(32.0 + 8.0 + search_w + 14.0);
            if self.loaded && !self.shown.is_empty() {
                let bytes: u64 = self.shown.iter().map(|&i| self.items[i].size).sum();
                let text = format!("{} · {}", self.shown.len(), human_size(bytes));
                row.label(egui::RichText::new(text).font(sans(12.0)).color(p.t3));
            }
        });
    }

    fn spec(&self, width: f32) -> Spec {
        Spec { width, min_tile: self.tile_width(), aspect: ASPECT, gap: GAP, heading: HEADING, day_gap: DAY_GAP }
    }

    pub(super) fn grid_view(&mut self, ui: &mut Ui, look: &Look) {
        let p = look.p;
        let ctx = ui.ctx().clone();
        let panel = ui.max_rect();
        if self.loaded && self.shown.is_empty() && self.pending.is_empty() {
            let text = if self.items.is_empty() { self.setup.empty } else { "nothing matches" };
            ui.painter().text(panel.center(), Align2::CENTER_CENTER, text, sans(13.0), p.t3);
            self.drawn.clear();
            self.flip.last.clear();
            return;
        }
        // ctrl+wheel: bigger or smaller tiles
        let zoom = ctx.input(|i| i.zoom_delta());
        if zoom != 1.0 && ui.rect_contains_pointer(panel) {
            let w = (self.tile_width() * zoom).clamp(TILE_RANGE.0, TILE_RANGE.1);
            self.saved.tile = Some(w);
        }
        let spec = self.spec(panel.width() - 2.0 * PAD);
        if self.dirty || self.spec != Some(spec) {
            self.grid = Grid::new(spec, &self.days);
            self.spec = Some(spec);
            if self.dirty {
                let mut present: HashSet<u64> = self.shown.iter().map(|&i| self.items[i].id).collect();
                present.extend(self.days.iter().map(heading_key));
                self.flip.start(&present, self.now);
            }
            self.dirty = false;
        }
        let scroll_to = self.scroll_to.take().and_then(|id| self.index(id));
        let mut hit = None;
        let mut pointer = None;
        let mut clicked = None;
        let mut menu_at = None;
        let mut rows_in_view = 0..0;
        let (mut before, mut after) = (0..0, 0..0);
        let height = self.grid.height + 2.0 * PAD;
        let area = egui::ScrollArea::vertical()
            .id_salt("gallery-grid")
            .auto_shrink(false)
            .scroll_source(ScrollSource::SCROLL_BAR | ScrollSource::MOUSE_WHEEL);
        area.show_viewport(ui, |ui, viewport| {
                ui.set_min_size(vec2(panel.width(), height));
                let origin = ui.min_rect().min + vec2(PAD, PAD);
                let top = viewport.top() - PAD;
                let bottom = viewport.bottom() - PAD;
                if let Some(i) = scroll_to {
                    let r = self.grid.tile(i).translate(origin.to_vec2());
                    ui.scroll_to_rect(r.expand(PAD / 2.0), None);
                }
                let rows = self.grid.visible(top, bottom);
                rows_in_view = self.grid.items_in(rows.clone());
                let span = bottom - top;
                before = self.grid.items_in(self.grid.visible(top - span, top));
                after = self.grid.items_in(self.grid.visible(bottom, bottom + span));
                let clip = ui.clip_rect();
                let resp = ui.interact(clip, Id::unique("gallery-tiles"), Sense::click());
                pointer = ctx.input(|i| i.pointer.hover_pos()).filter(|p| clip.contains(*p));
                let painter = ui.painter().clone();
                self.drawn.clear();
                let mut last = HashMap::new();
                // gone tiles fade where they were, under the rest
                self.draw_ghosts(&painter, look, origin);
                for r in rows {
                    let row = self.grid.rows[r];
                    match row.line {
                        Line::Heading(d) => self.heading(&painter, look, origin, row.top, d, &mut last),
                        Line::Tiles { first, end } => {
                            for i in first..end {
                                self.tile(&painter, look, origin, i, pointer, &mut last);
                            }
                        }
                    }
                }
                self.flip.armed = false;
                self.flip.last = last;
                let over = pointer.and_then(|p| self.grid.at(p - origin.to_vec2())).and_then(|i| self.shown_item(i)).map(|i| i.id);
                hit = over;
                if over.is_some() {
                    resp.clone().on_hover_cursor(CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    clicked = over;
                }
                if resp.secondary_clicked() {
                    menu_at = over.zip(resp.interact_pointer_pos());
                }
            });
        if self.flip.busy() {
            ctx.request_repaint();
        }
        // the preview sits over the grid: it takes the pointer where it is
        let in_preview = self.preview.as_ref().filter(|pv| pointer.is_some_and(|p| pv.rect.contains(p))).map(|pv| pv.id);
        let over = in_preview.or(hit);
        if clicked.is_some() {
            let id = in_preview.or(clicked);
            if let Some(id) = id {
                self.focus = Some(id);
                if let Some(f) = ctx.memory(|m| m.focused()) {
                    ctx.memory_mut(|m| m.surrender_focus(f));
                }
                self.open_item(id);
            }
        }
        if let Some((id, at)) = menu_at {
            let id = in_preview.unwrap_or(id);
            self.focus = Some(id);
            self.menu = Some(Menu { id, at });
        }
        // a preview the keys asked for opens once its tile is in view
        if let Some(id) = self.follow {
            if self.index(id).is_none() {
                self.follow = None;
            } else if self.drawn.contains_key(&id) {
                self.follow = None;
                self.open_preview(&ctx, id, true);
            }
        } else {
            self.hover(&ctx, pointer, over);
        }
        let visible = rows_in_view.len();
        self.want(rows_in_view.clone().chain(after).chain(before.rev()));
        self.evict(visible);
        if !self.reported && self.loaded && visible > 0 && rows_in_view.clone().all(|i| self.shown_item(i).is_some_and(|it| self.tex.contains_key(&it.id) || self.failed.contains(&it.id))) {
            self.reported = true;
            glint_core::log::log(&format!("gallery: {visible} thumbnails in view after {} ms", self.started.elapsed().as_millis()));
        }
    }

    fn heading(&mut self, painter: &Painter, look: &Look, origin: egui::Pos2, top: f32, d: usize, last: &mut HashMap<u64, Rect>) {
        let p = look.p;
        let day = &self.days[d];
        let key = heading_key(day);
        let target = Rect::from_min_size(pos2(0.0, top), vec2(self.grid.tile.x, HEADING));
        let (rect, shown) = self.flip.place(key, target, self.now);
        last.insert(key, rect);
        let label = label(day);
        self.flip.labels.insert(key, label.clone());
        let at = origin + rect.min.to_vec2() + vec2(0.0, 12.0);
        let g = painter.layout_no_wrap(label, medium(13.0), p.t1.gamma_multiply(shown));
        let w = g.size().x;
        painter.galley(at, g, p.t1);
        let n = day.items.len();
        painter.text(at + vec2(w + 8.0, 2.0), Align2::LEFT_TOP, n.to_string(), mono(11.0), p.t4.gamma_multiply(shown));
    }

    fn tile(&mut self, painter: &Painter, look: &Look, origin: egui::Pos2, i: usize, pointer: Option<egui::Pos2>, last: &mut HashMap<u64, Rect>) {
        let p = look.p;
        let item = &self.items[self.shown[i]];
        let (id, kind) = (item.id, item.kind);
        let (rect, shown) = self.flip.place(id, self.grid.tile(i), self.now);
        last.insert(id, rect);
        let screen = rect.translate(origin.to_vec2());
        self.drawn.insert(id, screen);
        let previewing = self.preview.as_ref().is_some_and(|pv| pv.id == id) || self.fading.as_ref().is_some_and(|pv| pv.id == id);
        let hovered = pointer.is_some_and(|p| screen.contains(p)) && self.menu.is_none();
        let ctx = painter.ctx();
        let h = ease(widgets::anim_ctx(ctx, Id::unique(("tile-hover", id)), hovered, LIFT));
        let tex = self.tex.get_mut(&id);
        let arrived = tex.as_ref().map_or(0.0, |t| {
            let t = if motion() { ((self.now - t.at) as f32 / FADE_IN).clamp(0.0, 1.0) } else { 1.0 };
            ease(t)
        });
        if arrived < 1.0 {
            ctx.request_repaint();
        }
        // lifted toward the viewer while hovered; new tiles grow in, thumbnails settle into theirs
        let lifted = 1.0 + 0.02 * h - 0.04 * (1.0 - shown);
        let r = Rect::from_center_size(screen.center() - vec2(0.0, 2.0 * h), screen.size() * lifted);
        let radius = RADIUS as f32;
        if h > 0.0 {
            let shadow = Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha((70.0 * h * shown) as u8) };
            painter.add(shadow.as_shape(r, RADIUS));
        }
        painter.rect_filled(r, RADIUS, p.field.gamma_multiply(shown));
        match tex {
            Some(t) if !previewing => {
                t.used = self.frame;
                let inner = Rect::from_center_size(r.center(), r.size() * (0.96 + 0.04 * arrived));
                let uv = cover(t.tex.size(), inner);
                image(painter, inner, radius, t.tex.id(), uv, arrived * shown);
            }
            Some(t) => t.used = self.frame,
            None if self.failed.contains(&id) => {
                let ext = item.path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                painter.text(r.center(), Align2::CENTER_CENTER, ext, mono(11.0), p.t4.gamma_multiply(shown));
            }
            None => {}
        }
        if previewing {
            painter.rect_stroke(r, RADIUS, Stroke::new(1.0, p.line2), StrokeKind::Inside);
        }
        let meta = self.meta.get(&id).copied();
        let badges = arrived * shown * if previewing { 0.0 } else { 1.0 };
        if badges > 0.0 && kind.moves() && let Some(m) = meta {
            let length = human_duration(m.duration_ms as f64 / 1000.0);
            let text = if kind == Kind::Gif { format!("gif {length}") } else { length };
            badge(painter, r, &text, true, badges);
        }
        if h > 0.0 && !previewing && let Some(local) = self.locals[self.shown[i]] {
            let text = format!("{:02}:{:02} · {}", local.hour, local.minute, human_size(item.size));
            badge(painter, r, &text, false, h * shown);
        }
        if self.is_busy(id) {
            let t = (self.now * 0.8).fract() as f32;
            let bar = Rect::from_min_size(pos2(r.left() + (r.width() - 40.0) * t, r.bottom() - 3.0), vec2(40.0, 3.0));
            painter.rect_filled(bar.intersect(r), 1, p.acc);
            ctx.request_repaint();
        }
        let focused = self.focus == Some(id);
        let ring = widgets::anim_ctx(ctx, Id::unique(("tile-focus", id)), focused && look.keyboard, 0.1);
        if ring > 0.0 {
            painter.rect_stroke(r.expand(3.0), radius + 3.0, Stroke::new(2.0, p.focus.gamma_multiply(ring)), StrokeKind::Middle);
        }
    }

    fn draw_ghosts(&mut self, painter: &Painter, look: &Look, origin: egui::Pos2) {
        let now = self.now;
        let p = look.p;
        self.flip.ghosts.retain(|g| motion() && now - g.start < FADE_IN as f64);
        for g in &self.flip.ghosts {
            let t = ease(((now - g.start) as f32 / FADE_IN).clamp(0.0, 1.0));
            let fade = 1.0 - t;
            let r = g.rect.translate(origin.to_vec2());
            if g.key & HEADING_BIT != 0 {
                if let Some(label) = &g.label {
                    painter.text(r.min + vec2(0.0, 12.0), Align2::LEFT_TOP, label, medium(13.0), p.t1.gamma_multiply(fade));
                }
                continue;
            }
            let r = Rect::from_center_size(r.center(), r.size() * (1.0 - 0.12 * t));
            painter.rect_filled(r, RADIUS, p.field.gamma_multiply(fade));
            if let Some(tex) = self.tex.get(&g.key) {
                image(painter, r, RADIUS as f32, tex.tex.id(), cover(tex.tex.size(), r), fade);
            }
        }
    }

    /// The grown preview, and the one shrinking back into its tile.
    pub(super) fn draw_previews(&mut self, ctx: &egui::Context, look: &Look) {
        if self.preview.is_none() && self.fading.is_none() {
            return;
        }
        let ppp = ctx.pixels_per_point();
        let area = egui::Area::new(Id::unique("gallery-preview")).order(Order::Middle).fixed_pos(self.panel.min).interactable(false);
        area.show(ctx, |ui| {
            let painter = ui.painter().with_clip_rect(self.panel);
            if let Some(mut pv) = self.fading.take() {
                let done = self.draw_preview(ctx, &painter, look, &mut pv, ppp);
                if !done {
                    self.fading = Some(pv);
                }
            }
            if let Some(mut pv) = self.preview.take() {
                self.draw_preview(ctx, &painter, look, &mut pv, ppp);
                self.preview = Some(pv);
            }
        });
    }

    /// Returns true once a closing preview is back in its tile.
    fn draw_preview(&mut self, ctx: &egui::Context, painter: &Painter, look: &Look, pv: &mut Preview, ppp: f32) -> bool {
        let p = look.p;
        let raw = pv.progress(self.now);
        if pv.closing && raw <= 0.0 {
            return true;
        }
        if raw > 0.0 && raw < 1.0 {
            ctx.request_repaint();
        }
        let t = ease(raw);
        if let Some(tile) = self.drawn.get(&pv.id) {
            pv.tile = *tile;
        }
        let meta = self.meta.get(&pv.id).copied().unwrap_or_default();
        let target = preview_rect(pv.tile, meta, self.panel, ppp);
        let rect = lerp_rect(pv.tile, target, t);
        pv.rect = rect;
        if let Some(play) = &pv.play
            && let Some(frame) = play.poll()
        {
            match &mut pv.full {
                Some(tex) => self.textures.replace(tex, &frame, false),
                None => {
                    pv.full = self.textures.upload(&frame, false);
                    pv.full_at = self.now;
                }
            }
        }
        // the rest of the grid steps back
        painter.rect_filled(self.panel, super::panel_corners(ctx), Color32::from_black_alpha((60.0 * t) as u8));
        let radius = RADIUS as f32 + 2.0 * t;
        let shadow = Shadow { offset: [0, 12], blur: 36, spread: 0, color: Color32::from_black_alpha((110.0 * t) as u8) };
        painter.add(shadow.as_shape(rect, radius as u8));
        painter.rect_filled(rect, radius, p.field);
        let full_uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        let thumb = self.tex.get(&pv.id);
        if let Some(th) = thumb {
            let uv = lerp_rect(cover(th.tex.size(), pv.tile), full_uv, t);
            image(painter, rect, radius, th.tex.id(), uv, 1.0);
        }
        if let Some(full) = &pv.full {
            // crisp once it's decoded: it fades over the stretched thumbnail
            let fade = if motion() { ((self.now - pv.full_at) as f32 / 0.15).clamp(0.0, 1.0) } else { 1.0 };
            if fade < 1.0 {
                ctx.request_repaint();
            }
            image(painter, rect, radius, full.id(), full_uv, fade);
        }
        let kind = self.item(pv.id).map(|i| i.kind);
        if let Some(play) = pv.play.as_ref().filter(|_| t > 0.0) {
            let bar = Rect::from_min_size(pos2(rect.left(), rect.bottom() - 2.0), vec2(rect.width() * play.progress(), 2.0));
            painter.rect_filled(bar, 0, p.acc.gamma_multiply(t));
            ctx.request_repaint();
        }
        let info = ((t - 0.6) / 0.4).clamp(0.0, 1.0);
        if info > 0.0 && let Some(item) = self.item(pv.id) {
            let mut text = item.name();
            if meta.width > 0 {
                text += &format!(" · {}×{}", meta.width, meta.height);
            }
            if kind.is_some_and(Kind::moves) && meta.duration_ms > 0 {
                text += &format!(" · {}", human_duration(meta.duration_ms as f64 / 1000.0));
            }
            text += &format!(" · {}", human_size(item.size));
            let clip = painter.with_clip_rect(rect.intersect(self.panel));
            badge(&clip, rect.shrink(4.0), &text, false, info);
        }
        false
    }
}

fn heading_key(day: &glint_core::gallery::Day) -> u64 {
    let d = day.date.map_or(0, |d| (d.year as u64) << 16 | (d.month as u64) << 8 | d.day as u64);
    HEADING_BIT | d
}

fn label(day: &glint_core::gallery::Day) -> String {
    let today = glint_shell::time::local_time(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
    )
    .map(glint_core::gallery::Date::of);
    match (day.date, today) {
        (Some(d), Some(today)) => d.label(today),
        _ => "earlier".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_cut_wide_pictures_in_the_middle_and_tall_ones_from_the_top() {
        let tile = Rect::from_min_size(pos2(0.0, 0.0), vec2(160.0, 100.0));
        let wide = cover([320, 100], tile);
        assert_eq!(wide, Rect::from_min_max(pos2(0.25, 0.0), pos2(0.75, 1.0)));
        let tall = cover([100, 250], tile);
        assert_eq!(tall.min, pos2(0.0, 0.0));
        assert!((tall.max.y - 0.25).abs() < 1e-6 && tall.max.x == 1.0);
        assert_eq!(cover([160, 100], tile), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)));
    }
}
