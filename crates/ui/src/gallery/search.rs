//! The toolbar and what narrows the list. Left: the kind switch and how many are shown. Right: the
//! search box, the filter button and the gear. What the list is narrowed to shows as chips inside
//! the box (× takes one off); typing offers what the last words could be (a span, a length, an
//! app, the app that made it) and a pick turns into a chip. The filter button opens the same
//! choices at once, each with how many it would show, and a calendar for any span of days.

use eframe::egui::epaint::{RectShape, Shadow};
use eframe::egui::{
    self, Align2, Color32, CursorIcon, Frame, Id, Margin, Mesh, Order, Painter, Pos2, Rect, RichText, Role, Sense, Shape,
    Stroke, StrokeKind, Ui, UiBuilder, WidgetInfo, emath::TSTransform, pos2, vec2,
};
use glint_core::format::human_size;
use glint_core::gallery::filter::{self, Facet, Filters, Length, MONTHS, Narrow, Suggestion, Token, Vocabulary, When, kind_word};
use glint_core::gallery::{Date, Filter, Kind, Maker};

use super::Gallery;
use crate::widgets::{self, Look, anim, anim_ctx, ease, glide, medium, mix, mono, sans, snap};

/// The search box's width with nothing in it, and the most it grows to with chips.
const FIELD_W: f32 = 260.0;
const FIELD_MAX: f32 = 560.0;
/// What the typed text keeps of the box however many chips there are.
const TYPING_ROOM: f32 = 72.0;
const CHIP_H: f32 = 22.0;
const CHIP_GAP: f32 = 4.0;
/// How long chips take to grow in or shrink away, and to slide over.
const CHIP_IN: f32 = 0.18;
const CHIP_OUT: f64 = 0.16;
const CHIP_MOVE: f32 = 0.2;
const OFFER_H: f32 = 30.0;
const POPOVER_W: f32 = 348.0;
const PAD: f32 = 16.0;
const PILL_H: f32 = 26.0;
const APP_ROW: f32 = 30.0;
/// Rows of apps shown before the list scrolls.
const APP_ROWS: usize = 6;
const DAY_H: f32 = 30.0;

/// The search box's text field.
pub(super) fn field_id() -> Id {
    Id::unique("gallery-search")
}

/// Something the search box offers, with how many it would show.
#[derive(Clone, Debug)]
pub(super) struct Offer {
    pub s: Suggestion,
    pub count: usize,
}

/// A chip on its way out.
struct Leaving {
    token: Token,
    rect: Rect,
    at: f64,
}

#[derive(Default)]
struct Calendar {
    open: bool,
    /// The first of the month shown; none: today's.
    month: Option<Date>,
    /// The first day picked, waiting for the last.
    start: Option<Date>,
}

pub(super) struct Search {
    pub query: String,
    pub filters: Filters,
    pub offers: Vec<Offer>,
    /// The offer enter picks.
    pub pick: usize,
    /// Esc put the offers away until something else is typed.
    pub hushed: bool,
    /// The offers were up last frame (the keys go to them).
    pub offers_shown: bool,
    pub popover: bool,
    calendar: Calendar,
    /// Facets whose chips were just made: they grow in.
    fresh: Vec<Facet>,
    leaving: Vec<Leaving>,
    /// Where each chip was drawn last, by facet.
    chips: [Option<Rect>; 5],
    field: Rect,
    offers_at: Rect,
    button: Rect,
    popover_at: Rect,
}

impl Default for Search {
    fn default() -> Search {
        Search {
            query: String::new(),
            filters: Filters::default(),
            offers: Vec::new(),
            pick: 0,
            hushed: false,
            offers_shown: false,
            popover: false,
            calendar: Calendar::default(),
            fresh: Vec::new(),
            leaving: Vec::new(),
            chips: [None; 5],
            field: Rect::NOTHING,
            offers_at: Rect::NOTHING,
            button: Rect::NOTHING,
            popover_at: Rect::NOTHING,
        }
    }
}

impl Search {
    /// The facet of the chip drawn last.
    pub fn last_chip(&self) -> Option<Facet> {
        self.filters.chips().last().map(Token::facet)
    }
}

/// What a glyph in front of a choice shows.
#[derive(Clone, Copy)]
enum Glyph<'a> {
    When,
    Length,
    Kind(Filter),
    /// An app, by its key.
    App(&'a str),
    Maker(Maker),
}

/// The glyph `token` goes with.
fn glyph_of(token: &Token) -> Glyph<'_> {
    match token {
        Token::Kind(k) => Glyph::Kind(*k),
        Token::When(_) => Glyph::When,
        Token::Length(_) => Glyph::Length,
        Token::App(name) => Glyph::App(name),
        Token::Maker(m) => Glyph::Maker(*m),
    }
}

fn filter_of(kind: Kind) -> Filter {
    match kind {
        Kind::Shot => Filter::Shots,
        Kind::Recording => Filter::Recordings,
        Kind::Gif => Filter::Gifs,
    }
}

/// The glint mark (a disc with a highlight) in `color`, `r` points across half.
pub(super) fn mark(painter: &Painter, c: Pos2, r: f32, color: [u8; 3], alpha: f32) {
    let [cr, cg, cb] = color;
    painter.circle_filled(c, r, Color32::from_rgb(cr, cg, cb).gamma_multiply(alpha));
    painter.circle_filled(c + vec2(r * 0.5, -r * 0.5), r * 0.22, Color32::WHITE.gamma_multiply(alpha));
}

impl Gallery {
    // ---- state ----------------------------------------------------------------------------------

    /// What the search box offers for the words being typed, each with how many it would show.
    pub(super) fn offers(&self) -> Vec<Offer> {
        let query = &self.search.query;
        if query.trim().is_empty() {
            return Vec::new();
        }
        let vocab = Vocabulary { index: &self.index, today: self.today };
        let (items, pending) = (&self.items, &self.pending);
        filter::suggest(query, &vocab, &self.search.filters)
            .into_iter()
            .map(|s| {
                // what's left once the words it stands for go, with it set
                let (tokens, words) = filter::read(&query[..s.from], &vocab);
                let set = self.search.filters.with(&tokens).with(std::slice::from_ref(&s.token));
                let narrow = Narrow::new(&set, &self.index, self.today);
                let count = filter::count(&self.index, &narrow, |i| !pending.holds(items[i].id) && items[i].holds(&words));
                Offer { s, count }
            })
            .collect()
    }

    /// Sets what `token` narrows; a new chip grows in.
    fn set_token(&mut self, token: Token) {
        let facet = token.facet();
        if facet != Facet::Kind && !self.search.filters.chips().iter().any(|t| t.facet() == facet) {
            self.search.fresh.push(facet);
        }
        self.search.filters.set(token);
        self.refilter();
    }

    /// Takes `facet`'s chip off; it shrinks away where it was.
    fn remove_chip(&mut self, facet: Facet) {
        if let (Some(token), Some(rect)) =
            (self.search.filters.chips().into_iter().find(|t| t.facet() == facet), self.search.chips[facet as usize])
        {
            self.search.leaving.push(Leaving { token, rect, at: self.now });
        }
        self.search.filters.clear(facet);
        self.refilter();
    }

    pub(super) fn remove_last_chip(&mut self) {
        if let Some(f) = self.search.last_chip() {
            self.remove_chip(f);
        }
    }

    /// Nothing narrows the list any more.
    pub(super) fn clear_filters(&mut self) {
        for t in self.search.filters.chips() {
            if let Some(rect) = self.search.chips[t.facet() as usize] {
                self.search.leaving.push(Leaving { token: t, rect, at: self.now });
            }
        }
        self.search.filters = Filters::default();
        self.search.query.clear();
        self.search.calendar.start = None;
        self.refilter();
    }

    /// The offer `k` becomes a chip in place of the words it stands for.
    pub(super) fn pick_offer(&mut self, k: usize) {
        let Some(o) = self.search.offers.get(k).cloned() else { return };
        let q = &mut self.search.query;
        q.truncate(o.s.from);
        q.truncate(q.trim_end().len());
        if !q.is_empty() {
            q.push(' ');
        }
        self.search.pick = 0;
        self.set_token(o.s.token);
        self.ctx.memory_mut(|m| m.request_focus(field_id()));
    }

    /// Whether the list is narrowed by anything.
    pub(super) fn narrowed(&self) -> bool {
        self.search.filters != Filters::default() || !self.search.query.trim().is_empty()
    }

    // ---- toolbar --------------------------------------------------------------------------------

    pub(super) fn toolbar(&mut self, ui: &mut Ui, look: &Look, rect: Rect) {
        let p = look.p;
        let inner = Rect::from_min_max(pos2(rect.left() + 16.0, rect.top() + 4.0), pos2(rect.right() - 16.0, rect.bottom() - 16.0));
        let cy = inner.center().y;
        let gear_rect = Rect::from_center_size(pos2(inner.right() - 16.0, cy), vec2(32.0, 32.0));
        if super::view::gear(ui, look, gear_rect).clicked() {
            self.open_settings();
        }
        let button = Rect::from_center_size(pos2(gear_rect.left() - 24.0, cy), vec2(32.0, 32.0));
        self.filter_button(ui, look, button);
        let mut left = inner.left();
        let kinds = self.index.kinds.len();
        if kinds > 1 {
            let before = self.search.filters.kind;
            // "all", then each kind there is: at most four, so no allocation a frame
            let mut options = [(Filter::All, "all"); 4];
            for (slot, &k) in options[1..].iter_mut().zip(&self.index.kinds) {
                *slot = (filter_of(k), kind_word(filter_of(k)));
            }
            let options = &options[..(kinds + 1).min(4)];
            let width = (96.0 * options.len() as f32).min(inner.width() * 0.36);
            let at = Rect::from_min_size(pos2(left, cy - 16.0), vec2(width, 32.0));
            let mut row = ui.new_child(UiBuilder::new().max_rect(at));
            widgets::segmented(&mut row, look, &mut self.search.filters.kind, options, Some(width));
            if self.search.filters.kind != before {
                self.refilter();
            }
            left += width + 14.0;
        }
        let right = button.left() - 8.0;
        self.search_field(ui, look, left, right, cy);
        let field_left = self.search.field.left();
        if self.loaded && !self.shown.is_empty() {
            let bytes: u64 = self.shown.iter().map(|&i| self.items[i].size).sum();
            let text = format!("{} · {}", self.shown.len(), human_size(bytes));
            let g = ui.painter().layout_no_wrap(text, sans(12.0), p.t3);
            if left + g.size().x + 12.0 < field_left {
                ui.painter().galley(pos2(left, cy - g.size().y / 2.0), g, p.t3);
            }
        }
    }

    /// The width a chip for `t` takes.
    fn chip_width(&self, painter: &Painter, t: &Token) -> f32 {
        let g = painter.layout_no_wrap(t.label(self.today), sans(12.0), Color32::WHITE);
        8.0 + 14.0 + 6.0 + g.size().x.min(150.0) + 4.0 + 16.0
    }

    fn search_field(&mut self, ui: &mut Ui, look: &Look, left: f32, right: f32, cy: f32) {
        let p = look.p;
        let ctx = ui.ctx().clone();
        let chips = self.search.filters.chips();
        let painter = ui.painter().clone();
        let widths: Vec<f32> = chips.iter().map(|t| self.chip_width(&painter, t)).collect();
        let room = (right - left - 140.0).max(160.0);
        let target = (FIELD_W + widths.iter().map(|w| w + CHIP_GAP).sum::<f32>()).min(FIELD_MAX).min(room);
        let w = glide(ui, Id::unique("gallery-search-w"), target, CHIP_MOVE);
        let field = Rect::from_min_max(pos2(right - w, cy - 16.0), pos2(right, cy + 16.0));
        self.search.field = field;
        let focused = ctx.memory(|m| m.has_focus(field_id()));
        let h = anim(ui, Id::unique("gallery-search-hov"), ui.rect_contains_pointer(field), 0.08);
        let f = anim(ui, Id::unique("gallery-search-foc"), focused, 0.08);
        let border = mix(mix(p.line2, p.line3, h), p.t2, f);
        painter.add(RectShape::new(field, 5, p.field, Stroke::new(1.0 + f, border), StrokeKind::Inside));
        // a magnifier
        let m = pos2(field.left() + 15.0, cy - 1.0);
        let lens = mix(p.t3, p.t2, f);
        painter.circle_stroke(m, 4.5, Stroke::new(1.4, lens));
        painter.line_segment([m + vec2(3.3, 3.3), m + vec2(6.5, 6.5)], Stroke::new(1.6, lens));

        // the text after the chips keeps some room: chips past it slide left, out under the lens
        let clear_w = 26.0;
        let typing = field.right() - clear_w - TYPING_ROOM;
        let row: f32 = widths.iter().map(|w| w + CHIP_GAP).sum();
        let strip = Rect::from_min_max(pos2(field.left() + 24.0, field.top() + 1.0), pos2(field.right() - 1.0, field.bottom() - 1.0));
        let clip = painter.with_clip_rect(strip);
        let fresh = std::mem::take(&mut self.search.fresh);
        let start = field.left() + 28.0;
        let mut x = start - (start + row - typing).max(0.0);
        let mut removed = None;
        let mut opened = false;
        self.search.chips = [None; 5];
        for (t, cw) in chips.iter().zip(&widths) {
            let facet = t.facet();
            let id = Id::unique(("gallery-chip", facet as u8));
            if fresh.contains(&facet) {
                snap(&ctx, id.with("in"), 0.0);
                snap(&ctx, id.with("x"), x);
            }
            let grow = ease(glide(ui, id.with("in"), 1.0, CHIP_IN));
            let cx = glide(ui, id.with("x"), x, CHIP_MOVE);
            let r = Rect::from_min_size(pos2(cx, cy - CHIP_H / 2.0), vec2(*cw, CHIP_H));
            let shown = Rect::from_center_size(r.center(), r.size() * (0.82 + 0.18 * grow));
            let body = ui.interact(r.intersect(strip), id, Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
            let close_r = Rect::from_min_max(pos2(r.right() - 20.0, r.top()), r.max);
            let close = ui.interact(close_r.intersect(strip), id.with("x"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
            let label = t.label(self.today);
            close.widget_info(|| WidgetInfo::labeled(Role::Button, true, format!("remove {label}")));
            body.widget_info(|| WidgetInfo::labeled(Role::Button, true, label.clone()));
            let hov = anim(ui, id.with("hov"), body.hovered() || close.hovered(), 0.08);
            let xh = anim(ui, id.with("xhov"), close.hovered(), 0.08);
            self.chip(&clip, look, shown, t, grow, hov, xh);
            self.search.chips[facet as usize] = Some(r);
            if close.clicked() {
                removed = Some(facet);
            } else if body.clicked() {
                opened = true;
            }
            x += (cw + CHIP_GAP) * grow;
        }
        // chips taken off shrink away where they were
        let now = self.now;
        self.search.leaving.retain(|l| widgets::motion() && now - l.at < CHIP_OUT);
        for l in &self.search.leaving {
            let t = ((now - l.at) / CHIP_OUT) as f32;
            let r = Rect::from_center_size(l.rect.center(), l.rect.size() * (1.0 - 0.18 * ease(t)));
            self.chip(&clip, look, r, &l.token, 1.0 - ease(t), 0.0, 0.0);
        }
        if !self.search.leaving.is_empty() {
            ctx.request_repaint();
        }
        // chips slid out on the left fade away by the lens
        if self.search.chips.iter().flatten().any(|r| r.left() < start) {
            let edge = Rect::from_min_size(strip.min, vec2(14.0, strip.height()));
            let mut mesh = Mesh::default();
            for (at, color) in [(edge.left_top(), p.field), (edge.right_top(), Color32::TRANSPARENT), (edge.right_bottom(), Color32::TRANSPARENT), (edge.left_bottom(), p.field)] {
                mesh.colored_vertex(at, color);
            }
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            painter.add(Shape::mesh(mesh));
        }
        // the text after the chips, and a clear-all at the end
        let edit_r = Rect::from_min_max(pos2(x.min(typing), field.top()), pos2(field.right() - clear_w, field.bottom()));
        let hint = if chips.is_empty() { "search" } else { "" };
        let before = self.search.query.len();
        let edit = egui::TextEdit::singleline(&mut self.search.query)
            .id(field_id())
            .frame(Frame::NONE.inner_margin(Margin::symmetric(2, 0)))
            .font(mono(12.0))
            .text_color(p.t1)
            .hint_text(RichText::new(hint).font(mono(12.0)).color(p.t3))
            .align(Align2::LEFT_CENTER)
            .desired_width(edit_r.width())
            .min_size(vec2(edit_r.width(), 32.0));
        let resp = ui.put(edit_r, edit);
        if resp.changed() || self.search.query.len() != before {
            self.search.hushed = false;
            self.search.popover = false;
            self.search.pick = 0;
            self.refilter();
        }
        let any = !self.search.query.is_empty() || !chips.is_empty();
        let shown = anim(ui, Id::unique("gallery-search-clear"), any, 0.12);
        if shown > 0.0 {
            let c = pos2(field.right() - clear_w / 2.0 - 2.0, cy);
            let r = Rect::from_center_size(c, vec2(22.0, 22.0));
            let resp = ui.interact(r, Id::unique("gallery-search-clear-btn"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
            resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "clear"));
            let hv = anim(ui, resp.id.with("hov"), resp.hovered(), 0.08);
            painter.circle_filled(c, 9.0, p.hov.gamma_multiply(hv * shown));
            cross(&painter, c, 3.4, mix(p.t3, p.t1, hv).gamma_multiply(shown));
            if resp.clicked() && any {
                self.clear_filters();
            }
        }
        if let Some(f) = removed {
            self.remove_chip(f);
        } else if opened {
            self.search.popover = true;
        }
    }

    /// One chip: a glyph, the label, an ×; `alpha` fades it, `hov` and `xh` light it and its ×.
    #[allow(clippy::too_many_arguments)]
    fn chip(&self, painter: &Painter, look: &Look, r: Rect, t: &Token, alpha: f32, hov: f32, xh: f32) {
        if alpha <= 0.0 {
            return;
        }
        let p = look.p;
        painter.rect(r, 5, mix(p.ctl, p.ctlh, hov).gamma_multiply(alpha), Stroke::new(1.0, p.line2.gamma_multiply(alpha)), StrokeKind::Inside);
        let gc = pos2(r.left() + 8.0 + 7.0, r.center().y);
        self.glyph(painter, look, gc, glyph_of(t), alpha);
        let label = t.label(self.today);
        let text_r = Rect::from_min_max(pos2(gc.x + 13.0, r.top()), pos2(r.right() - 20.0, r.bottom()));
        let g = painter.layout_no_wrap(label, sans(12.0), p.t1.gamma_multiply(alpha));
        painter.with_clip_rect(text_r.intersect(painter.clip_rect())).galley(pos2(text_r.left(), r.center().y - g.size().y / 2.0), g, p.t1);
        let xc = pos2(r.right() - 10.0, r.center().y);
        if xh > 0.0 {
            painter.circle_filled(xc, 7.0, p.hov.gamma_multiply(xh * alpha));
        }
        cross(painter, xc, 3.0, mix(p.t3, p.t1, xh.max(hov * 0.5)).gamma_multiply(alpha));
    }

    /// A 14-point glyph for a kind of choice at `c`.
    fn glyph(&self, painter: &Painter, look: &Look, c: Pos2, g: Glyph, alpha: f32) {
        let p = look.p;
        let ink = p.t2.gamma_multiply(alpha);
        let line = Stroke::new(1.3, ink);
        match g {
            Glyph::When => {
                // a calendar page
                let r = Rect::from_center_size(c + vec2(0.0, 0.5), vec2(11.0, 10.0));
                painter.rect_stroke(r, 2, line, StrokeKind::Middle);
                painter.hline(r.x_range(), r.top() + 3.0, line);
                for dx in [-2.5, 2.5] {
                    painter.vline(c.x + dx, r.top() - 1.5..=r.top() + 1.0, line);
                }
            }
            Glyph::Length => {
                // a clock
                painter.circle_stroke(c, 5.5, line);
                painter.line_segment([c, c + vec2(0.0, -3.2)], line);
                painter.line_segment([c, c + vec2(2.4, 1.2)], line);
            }
            Glyph::Kind(k) => match k {
                Filter::Shots | Filter::All => {
                    let r = Rect::from_center_size(c, vec2(12.0, 9.0));
                    painter.rect_stroke(r, 2, line, StrokeKind::Middle);
                    painter.circle_filled(c + vec2(2.5, -1.0), 1.2, ink);
                }
                Filter::Recordings => {
                    painter.circle_stroke(c, 5.5, line);
                    painter.circle_filled(c, 2.6, ink);
                }
                Filter::Gifs => {
                    painter.text(c, Align2::CENTER_CENTER, "gif", mono(8.5), ink);
                }
            },
            Glyph::App(name) => {
                let key = name.to_lowercase();
                let r = Rect::from_center_size(c, vec2(14.0, 14.0));
                match self.icons.get(&key) {
                    Some(tex) => super::view::image(painter, r, 3.0, tex.id(), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), alpha),
                    None => {
                        painter.rect_filled(r, 3, p.line2.gamma_multiply(alpha));
                        let letter: String = name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect()).unwrap_or_default();
                        widgets::centred_mark(painter, c, letter, medium(9.0), p.t1.gamma_multiply(alpha));
                    }
                }
            }
            Glyph::Maker(m) => mark(painter, c, 5.5, self.accents[m as usize], alpha),
        }
    }

    /// The square button with the filter's funnel; a dot counts what's set.
    fn filter_button(&mut self, ui: &mut Ui, look: &Look, rect: Rect) {
        let p = look.p;
        self.search.button = rect;
        let resp = ui.interact(rect, Id::unique("gallery-filter"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "filter"));
        let open = self.search.popover;
        let h = anim(ui, resp.id.with("hov"), resp.hovered() || open, 0.08);
        let d = anim(ui, resp.id.with("prs"), resp.is_pointer_button_down_on(), 0.08);
        let painter = ui.painter();
        painter.rect_filled(rect, 6, p.hov.gamma_multiply(h * (1.0 - d)));
        painter.rect_filled(rect, 6, p.prs.gamma_multiply(d));
        let color = mix(p.t2, p.t1, h);
        let c = rect.center();
        for (i, half) in [7.0, 4.6, 2.2].into_iter().enumerate() {
            let y = c.y - 4.5 + i as f32 * 4.5;
            painter.line_segment([pos2(c.x - half, y), pos2(c.x + half, y)], Stroke::new(1.6, color));
        }
        let n = self.search.filters.chips().len() + usize::from(self.search.filters.kind != Filter::All);
        let dot = ease(glide(ui, resp.id.with("dot"), if n > 0 { 1.0 } else { 0.0 }, 0.18));
        if dot > 0.0 {
            let at = rect.right_top() + vec2(-6.0, 6.0);
            painter.circle_filled(at, 7.0 * dot, p.acc);
            painter.circle_stroke(at, 7.0 * dot, Stroke::new(1.5, p.side));
            if n > 0 {
                painter.text(at, Align2::CENTER_CENTER, n.to_string(), mono(9.5), p.on_acc.gamma_multiply(dot));
            }
        }
        if look.keyboard && resp.has_focus() {
            painter.rect_stroke(rect.expand(2.0), 8.0, Stroke::new(2.0, p.focus), StrokeKind::Middle);
        }
        // open, the popover says what it is; the tip would sit on its corner
        let resp = if open { resp } else { resp.on_hover_text("filter") };
        if resp.clicked() {
            self.search.popover = !self.search.popover;
        }
    }

    // ---- offers ---------------------------------------------------------------------------------

    /// What the box offers while typing, under it.
    pub(super) fn draw_offers(&mut self, ctx: &egui::Context, look: &Look) {
        let p = look.p;
        let held = ctx.input(|i| i.pointer.hover_pos()).is_some_and(|at| self.search.offers_at.contains(at));
        let typing = ctx.memory(|m| m.has_focus(field_id()));
        let show = (typing || held) && !self.search.hushed && !self.search.offers.is_empty() && !self.search.popover;
        self.search.offers_shown = show;
        let t = ease(anim_ctx(ctx, Id::unique("gallery-offers-open"), show, 0.12));
        if t <= 0.0 || self.search.offers.is_empty() {
            self.search.offers_at = Rect::NOTHING;
            return;
        }
        let field = self.search.field;
        let n = self.search.offers.len();
        let w = field.width().max(300.0);
        let rect = Rect::from_min_size(pos2(field.right() - w, field.bottom() + 6.0 - 4.0 * (1.0 - t)), vec2(w, n as f32 * OFFER_H + 8.0));
        self.search.offers_at = rect;
        let mut picked = None;
        let mut hovered = None;
        let shown = egui::Area::new(Id::unique("gallery-offers")).order(Order::Foreground).fixed_pos(rect.min).interactable(show).show(ctx, |ui| {
            ui.set_opacity(t);
            let painter = ui.painter().clone();
            let shadow = Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(70) };
            painter.add(shadow.as_shape(rect, 8));
            painter.rect(rect, 8, p.surf, Stroke::new(1.0, p.line2), StrokeKind::Inside);
            let at = glide(ui, Id::unique("gallery-offers-pick"), self.search.pick.min(n - 1) as f32, 0.1);
            let hl = Rect::from_min_size(pos2(rect.left() + 4.0, rect.top() + 4.0 + at * OFFER_H), vec2(w - 8.0, OFFER_H));
            painter.rect_filled(hl, 5, p.ctlh);
            for (k, o) in self.search.offers.iter().enumerate() {
                let r = Rect::from_min_size(pos2(rect.left() + 4.0, rect.top() + 4.0 + k as f32 * OFFER_H), vec2(w - 8.0, OFFER_H));
                let label = o.s.token.label(self.today);
                let resp = ui.interact(r, Id::unique(("gallery-offer", k)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
                resp.widget_info(|| WidgetInfo::labeled(Role::ListBoxOption, true, label.clone()));
                if resp.hovered() && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
                    hovered = Some(k);
                }
                if resp.clicked() {
                    picked = Some(k);
                }
                self.glyph(&painter, look, pos2(r.left() + 16.0, r.center().y), glyph_of(&o.s.token), 1.0);
                let color = if o.count == 0 { p.t3 } else { p.t1 };
                painter.text(pos2(r.left() + 34.0, r.center().y), Align2::LEFT_CENTER, label, sans(13.0), color);
                painter.text(pos2(r.right() - 12.0, r.center().y), Align2::RIGHT_CENTER, o.count.to_string(), mono(11.0), p.t3);
            }
        });
        let pivot = rect.center_top();
        let s = 0.98 + 0.02 * t;
        let transform = TSTransform::from_translation(pivot.to_vec2()) * TSTransform::from_scaling(s) * TSTransform::from_translation(-pivot.to_vec2());
        ctx.set_transform_layer(shown.response.layer_id, transform);
        if let Some(k) = hovered {
            self.search.pick = k;
        }
        if let Some(k) = picked.filter(|_| show) {
            self.pick_offer(k);
        }
    }

    // ---- the filter popover ---------------------------------------------------------------------

    pub(super) fn draw_popover(&mut self, ctx: &egui::Context, look: &Look) {
        let p = look.p;
        let open = self.search.popover;
        let t = ease(anim_ctx(ctx, Id::unique("gallery-popover-open"), open, 0.16));
        if t <= 0.0 {
            self.search.popover_at = Rect::NOTHING;
            self.search.calendar.start = None;
            return;
        }
        let button = self.search.button;
        let top = button.bottom() + 8.0;
        let left = (button.right() - POPOVER_W).max(8.0);
        let mut action: Option<Act> = None;
        let mut height = 0.0;
        let shown = egui::Area::new(Id::unique("gallery-popover")).order(Order::Foreground).fixed_pos(pos2(left, top)).interactable(open).show(ctx, |ui| {
            ui.set_opacity(t);
            let painter = ui.painter().clone();
            let shadow_at = painter.add(Shape::Noop);
            let back_at = painter.add(Shape::Noop);
            let inner_w = POPOVER_W - 2.0 * PAD;
            let x0 = left + PAD;
            let mut y = top + 12.0;
            // when
            y = section(&painter, look, "when", x0, y);
            let mut pills: Vec<Pill> = When::PRESETS
                .iter()
                .zip(self.counts.when)
                .map(|(w, n)| Pill::new(Act::Set(Token::When(*w)), w.label(self.today), Some(n), self.search.filters.when == Some(*w), None))
                .collect();
            if let Some(w) = self.search.filters.when.filter(|w| !When::PRESETS.contains(w)) {
                pills.push(Pill::new(Act::Clear(Facet::When), w.label(self.today), None, true, None));
            }
            pills.push(Pill::new(Act::Calendar, "pick days".into(), None, self.search.calendar.open, Some(Glyph::When)));
            y = self.pills(ui, look, "when", &pills, x0, y, inner_w, &mut action);
            y = self.calendar(ui, look, x0, y, inner_w, &mut action);
            // length
            y = section(&painter, look, "length", x0, y + 6.0);
            let mut pills: Vec<Pill> = Length::PRESETS
                .iter()
                .zip(self.counts.length)
                .map(|(l, n)| Pill::new(Act::Set(Token::Length(*l)), l.label(), Some(n), self.search.filters.length == Some(*l), None))
                .collect();
            if let Some(l) = self.search.filters.length.filter(|l| !Length::PRESETS.contains(l)) {
                pills.push(Pill::new(Act::Clear(Facet::Length), l.label(), None, true, None));
            }
            y = self.pills(ui, look, "length", &pills, x0, y, inner_w, &mut action);
            // app: as many rows as the window leaves room for under it, and it scrolls
            if !self.index.apps.is_empty() {
                y = section(&painter, look, "app", x0, y + 6.0);
                let made_by = if self.index.makers.len() > 1 { 6.0 + 20.0 + PILL_H + 4.0 } else { 0.0 };
                let clear = if self.search.filters != Filters::default() { 28.0 } else { 0.0 };
                let room = ctx.content_rect().bottom() - 8.0 - y - 4.0 - made_by - clear - 12.0;
                let rows = ((room / APP_ROW).floor().max(0.0) as usize).clamp(2, APP_ROWS);
                y = self.app_list(ui, look, x0, y, inner_w, rows, &mut action);
            }
            // made by
            if self.index.makers.len() > 1 {
                y = section(&painter, look, "made by", x0, y + 6.0);
                let pills: Vec<Pill> = Maker::ALL
                    .iter()
                    .map(|m| {
                        let on = self.search.filters.maker == Some(*m);
                        Pill::new(Act::Set(Token::Maker(*m)), m.name().to_string(), Some(self.counts.makers[*m as usize]), on, Some(Glyph::Maker(*m)))
                    })
                    .collect();
                y = self.pills(ui, look, "made by", &pills, x0, y, inner_w, &mut action);
            }
            // clear all, once something's set
            let set = self.search.filters != Filters::default();
            let c = anim(ui, Id::unique("gallery-popover-clear"), set, 0.14);
            if c > 0.0 {
                let g = painter.layout_no_wrap("clear all".into(), sans(12.5), p.t2);
                let r = Rect::from_min_size(pos2(x0 + inner_w - g.size().x, y + 8.0), g.size());
                let resp = ui.interact(r.expand(4.0), Id::unique("gallery-popover-clear-btn"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
                resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "clear all"));
                let hv = anim(ui, resp.id.with("hov"), resp.hovered(), 0.08);
                let color = mix(p.t3, p.t1, hv).gamma_multiply(c);
                painter.hline(r.x_range(), r.bottom() + 1.0, Stroke::new(1.0, mix(p.line3, p.t1, hv).gamma_multiply(c)));
                let gh = g.size().y;
                painter.galley(r.min, g, color);
                if resp.clicked() && set {
                    action = Some(Act::ClearAll);
                }
                y += (gh + 12.0) * c;
            }
            y += 12.0;
            let rect = Rect::from_min_max(pos2(left, top), pos2(left + POPOVER_W, y));
            height = rect.height();
            let shadow = Shadow { offset: [0, 10], blur: 30, spread: 0, color: Color32::from_black_alpha(80) };
            painter.set(shadow_at, shadow.as_shape(rect, 10));
            painter.set(back_at, RectShape::new(rect, 10, p.surf, Stroke::new(1.0, p.line2), StrokeKind::Inside));
        });
        let rect = Rect::from_min_size(pos2(left, top), vec2(POPOVER_W, height));
        self.search.popover_at = rect;
        // grows from under the button
        let pivot = pos2(button.center().x, top);
        let s = 0.96 + 0.04 * t;
        let transform = TSTransform::from_translation(vec2(0.0, -6.0 * (1.0 - t)))
            * TSTransform::from_translation(pivot.to_vec2())
            * TSTransform::from_scaling(s)
            * TSTransform::from_translation(-pivot.to_vec2());
        ctx.set_transform_layer(shown.response.layer_id, transform);
        if !open {
            return;
        }
        let outside = ctx.input(|i| {
            (i.pointer.any_click() || i.pointer.secondary_pressed())
                && i.pointer.interact_pos().is_some_and(|at| !rect.contains(at) && !self.search.button.contains(at) && !self.search.field.contains(at))
        });
        if outside && action.is_none() {
            self.search.popover = false;
        }
        match action {
            Some(Act::Set(token)) => {
                if self.search.filters.has(&token) {
                    self.remove_chip(token.facet());
                } else {
                    self.set_token(token);
                }
            }
            Some(Act::Clear(facet)) => self.remove_chip(facet),
            Some(Act::Calendar) => {
                let c = &mut self.search.calendar;
                c.open = !c.open;
                c.start = None;
            }
            Some(Act::Month(month)) => self.search.calendar.month = Some(month),
            Some(Act::Day(d)) => match self.search.calendar.start.take() {
                Some(first) => self.set_token(Token::When(When::Days(first, d))),
                None => self.search.calendar.start = Some(d),
            },
            Some(Act::ClearAll) => self.clear_filters(),
            None => {}
        }
    }

    /// A row of pills that wraps; what's clicked goes into `action`. Returns where it ends.
    #[allow(clippy::too_many_arguments)]
    fn pills(&self, ui: &Ui, look: &Look, group: &str, pills: &[Pill], x0: f32, y0: f32, w: f32, action: &mut Option<Act>) -> f32 {
        let p = look.p;
        let painter = ui.painter();
        let (mut x, mut y) = (x0, y0);
        for (k, Pill { act, label, count, on, glyph }) in pills.iter().enumerate() {
            let g = painter.layout_no_wrap(label.clone(), sans(12.5), p.t1);
            let n = count.map(|n| painter.layout_no_wrap(n.to_string(), mono(11.0), p.t3));
            let lead = if glyph.is_some() { 20.0 } else { 0.0 };
            let pw = 10.0 + lead + g.size().x + n.as_ref().map_or(0.0, |n| 6.0 + n.size().x) + 10.0;
            if x > x0 && x + pw > x0 + w {
                x = x0;
                y += PILL_H + 6.0;
            }
            let r = Rect::from_min_size(pos2(x, y), vec2(pw, PILL_H));
            let resp = ui.interact(r, Id::unique(("gallery-pill", group, k)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
            resp.widget_info(|| WidgetInfo::selected(Role::Button, true, *on, label));
            let hv = anim(ui, resp.id.with("hov"), resp.hovered(), 0.08);
            let sel = anim(ui, resp.id.with("on"), *on, 0.14);
            let empty = count == &Some(0) && !on;
            let fill = mix(mix(p.field, p.ctlh, hv), mix(p.ctl, p.acc, 0.32), sel);
            let edge = mix(mix(p.line2, p.line3, hv), p.acc, sel);
            painter.rect(r, 6, fill, Stroke::new(1.0, edge), StrokeKind::Inside);
            if let Some(gl) = glyph {
                self.glyph(painter, look, pos2(r.left() + 10.0 + 7.0, r.center().y), *gl, 1.0);
            }
            let color = if empty { p.t4 } else { mix(p.t2, p.t1, hv.max(sel)) };
            let gx = r.left() + 10.0 + lead;
            let gw = g.size().x;
            painter.galley(pos2(gx, r.center().y - g.size().y / 2.0), g, color);
            if let Some(n) = n {
                painter.galley(pos2(gx + gw + 6.0, r.center().y - n.size().y / 2.0), n, if empty { p.t4 } else { p.t3 });
            }
            if resp.clicked() {
                *action = Some(act.clone());
            }
            x += pw + 6.0;
        }
        y + PILL_H + 4.0
    }

    /// The apps captured, most first, each with its icon and how many; scrolls past `rows`.
    #[allow(clippy::too_many_arguments)]
    fn app_list(&self, ui: &mut Ui, look: &Look, x0: f32, y0: f32, w: f32, rows: usize, action: &mut Option<Act>) -> f32 {
        let p = look.p;
        let h = self.index.apps.len().min(rows) as f32 * APP_ROW;
        let area = Rect::from_min_size(pos2(x0 - 4.0, y0), vec2(w + 8.0, h));
        let mut child = ui.new_child(UiBuilder::new().max_rect(area));
        egui::ScrollArea::vertical().id_salt("gallery-popover-apps").max_height(h).auto_shrink([false, true]).show(&mut child, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (k, app) in self.index.apps.iter().enumerate() {
                let (r, resp) = ui.allocate_exact_size(vec2(w + 8.0 - 8.0, APP_ROW), Sense::click());
                let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
                let on = self.search.filters.app.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(&app.name));
                resp.widget_info(|| WidgetInfo::selected(Role::Button, true, on, &app.name));
                let hv = anim(ui, resp.id.with("hov"), resp.hovered(), 0.08);
                let sel = anim(ui, resp.id.with("on"), on, 0.14);
                let painter = ui.painter();
                painter.rect_filled(r, 5, mix(p.ctlh.gamma_multiply(hv), mix(p.ctl, p.acc, 0.32), sel));
                self.glyph(painter, look, pos2(r.left() + 14.0, r.center().y), Glyph::App(&app.key), 1.0);
                let n = self.counts.apps.get(k).copied().unwrap_or(0);
                let color = if n == 0 && !on { p.t4 } else { p.t1 };
                let text_r = Rect::from_min_max(pos2(r.left() + 30.0, r.top()), pos2(r.right() - 44.0, r.bottom()));
                let g = painter.layout_no_wrap(app.name.clone(), sans(13.0), color);
                painter.with_clip_rect(text_r.intersect(ui.clip_rect())).galley(pos2(text_r.left(), r.center().y - g.size().y / 2.0), g, color);
                painter.text(pos2(r.right() - 10.0, r.center().y), Align2::RIGHT_CENTER, n.to_string(), mono(11.0), p.t3);
                if resp.clicked() {
                    *action = Some(Act::Set(Token::App(app.name.clone())));
                }
            }
        });
        y0 + h + 4.0
    }

    /// A month of days to pick a span on, opening under the "when" pills; days with captures have
    /// a dot. Returns where it ends.
    fn calendar(&self, ui: &Ui, look: &Look, x0: f32, y0: f32, w: f32, action: &mut Option<Act>) -> f32 {
        let p = look.p;
        let open = ease(glide(ui, Id::unique("gallery-calendar"), if self.search.calendar.open { 1.0 } else { 0.0 }, 0.2));
        if open <= 0.0 {
            return y0;
        }
        let full = 8.0 + 26.0 + 20.0 + 6.0 * DAY_H + 4.0;
        let area = Rect::from_min_size(pos2(x0, y0), vec2(w, full * open));
        let painter = ui.painter().with_clip_rect(area.intersect(ui.clip_rect()));
        let alpha = open;
        let month = self.search.calendar.month.unwrap_or(Date { day: 1, ..self.today });
        let mut y = y0 + 8.0;
        // the month and the arrows to the next and the one before
        let title = format!("{} {}", MONTHS[(month.month - 1) as usize], month.year);
        painter.text(pos2(x0 + w / 2.0, y + 13.0), Align2::CENTER_CENTER, title, medium(13.0), p.t1.gamma_multiply(alpha));
        for (dir, cx) in [(-1i64, x0 + 12.0), (1, x0 + w - 12.0)] {
            let r = Rect::from_center_size(pos2(cx, y + 13.0), vec2(24.0, 24.0));
            let resp = ui.interact(r, Id::unique(("gallery-calendar-step", dir)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
            resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, if dir < 0 { "month before" } else { "next month" }));
            let hv = anim(ui, resp.id.with("hov"), resp.hovered(), 0.08);
            painter.rect_filled(r, 5, p.hov.gamma_multiply(hv * alpha));
            let c = r.center();
            let s = dir as f32;
            let chevron = vec![c + vec2(-2.0 * s, -4.0), c + vec2(2.0 * s, 0.0), c + vec2(-2.0 * s, 4.0)];
            painter.add(Shape::line(chevron, Stroke::new(1.5, mix(p.t3, p.t1, hv).gamma_multiply(alpha))));
            if resp.clicked() && open >= 1.0 {
                let to = if dir < 0 { month.number() - 1 } else { month.number() + month.days_in_month() as i64 };
                *action = Some(Act::Month(Date { day: 1, ..Date::from_number(to) }));
            }
        }
        y += 26.0;
        let cell = w / 7.0;
        for (i, d) in ["m", "t", "w", "t", "f", "s", "s"].iter().enumerate() {
            painter.text(pos2(x0 + cell * (i as f32 + 0.5), y + 9.0), Align2::CENTER_CENTER, *d, sans(11.0), p.t4.gamma_multiply(alpha));
        }
        y += 20.0;
        // what's picked: the span set, or the first day picked and the hovered one
        let hover_day = |at: Pos2| -> Option<Date> {
            let (col, row) = (((at.x - x0) / cell).floor(), ((at.y - y) / DAY_H).floor());
            if !(0.0..7.0).contains(&col) || !(0.0..6.0).contains(&row) {
                return None;
            }
            let n = month.number() - month.weekday() + row as i64 * 7 + col as i64;
            let d = Date::from_number(n);
            (d.month == month.month).then_some(d)
        };
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let span = match (self.search.calendar.start, pointer.and_then(hover_day)) {
            (Some(a), Some(b)) => Some((a.number().min(b.number()), a.number().max(b.number()))),
            (Some(a), None) => Some((a.number(), a.number())),
            (None, _) => self.search.filters.when.map(|w| w.span(self.today)),
        };
        let today = self.today.number();
        let first = month.number();
        for day in 0..month.days_in_month() as i64 {
            let n = first + day;
            let slot = month.weekday() + day;
            let (col, row) = (slot % 7, slot / 7);
            let r = Rect::from_min_size(pos2(x0 + col as f32 * cell, y + row as f32 * DAY_H), vec2(cell, DAY_H)).shrink2(vec2(1.0, 1.5));
            let resp = ui.interact(r, Id::unique(("gallery-day", n)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
            let hv = anim(ui, resp.id.with("hov"), resp.hovered(), 0.06);
            let picked = span.is_some_and(|(a, b)| n >= a && n <= b);
            if picked {
                painter.rect_filled(r, 5, mix(p.ctl, p.acc, 0.35).gamma_multiply(alpha));
            } else {
                painter.rect_filled(r, 5, p.hov.gamma_multiply(hv * alpha));
            }
            if n == today {
                painter.rect_stroke(r, 5, Stroke::new(1.0, p.t3.gamma_multiply(alpha)), StrokeKind::Inside);
            }
            let color = if n > today { p.t4 } else { p.t1 };
            painter.text(r.center() - vec2(0.0, 2.0), Align2::CENTER_CENTER, (day + 1).to_string(), sans(12.0), color.gamma_multiply(alpha));
            if let Some(&count) = self.counts.days.get(&n) {
                let strength = (0.45 + count as f32 / 8.0).min(1.0);
                painter.circle_filled(pos2(r.center().x, r.bottom() - 4.5), 1.7, p.acct.gamma_multiply(strength * alpha));
            }
            if resp.clicked() && open >= 1.0 {
                *action = Some(Act::Day(Date::from_number(n)));
            }
        }
        y0 + full * open
    }
}

/// A choice in the popover: what a click does, what it says, how many it would show, whether
/// it's what's set, and its glyph.
struct Pill<'a> {
    act: Act,
    label: String,
    count: Option<usize>,
    on: bool,
    glyph: Option<Glyph<'a>>,
}

impl<'a> Pill<'a> {
    fn new(act: Act, label: String, count: Option<usize>, on: bool, glyph: Option<Glyph<'a>>) -> Pill<'a> {
        Pill { act, label, count, on, glyph }
    }
}

/// What a click in the popover does.
#[derive(Clone)]
enum Act {
    /// Sets the token, or unsets it when it's set already.
    Set(Token),
    Clear(Facet),
    Calendar,
    Month(Date),
    Day(Date),
    ClearAll,
}

/// A section's quiet name; returns where its content starts.
fn section(painter: &Painter, look: &Look, name: &str, x: f32, y: f32) -> f32 {
    painter.text(pos2(x, y), Align2::LEFT_TOP, name, sans(11.5), look.p.t3);
    y + 20.0
}

fn cross(painter: &Painter, c: Pos2, r: f32, color: Color32) {
    let s = Stroke::new(1.4, color);
    painter.line_segment([c + vec2(-r, -r), c + vec2(r, r)], s);
    painter.line_segment([c + vec2(-r, r), c + vec2(r, -r)], s);
}
