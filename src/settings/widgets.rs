//! Hand-drawn, animated widgets and the backdrop of the settings window.

use std::f32::consts::{PI, TAU};
use std::ops::RangeInclusive;

use eframe::egui::emath::TSTransform;
use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, CursorIcon, FontFamily, FontId, Layout, Mesh, Pos2, Rect, Response,
    RichText, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2,
};

pub const ACCENT: Color32 = Color32::from_rgb(76, 141, 255);
pub const ACCENT_HI: Color32 = Color32::from_rgb(132, 178, 255);
pub const TEXT: Color32 = Color32::from_rgb(236, 236, 244);
pub const MUTED: Color32 = Color32::from_rgb(150, 150, 166);
pub const FAINT: Color32 = Color32::from_rgb(100, 100, 116);
pub const WARN: Color32 = Color32::from_rgb(255, 182, 88);
/// Width of the control column inside a card.
pub const CONTROL_W: f32 = 316.0;
const LABEL_W: f32 = 108.0;
const HOVER_S: f32 = 0.14;

fn white(a: f32) -> Color32 {
    Color32::from_white_alpha((a.clamp(0.0, 1.0) * 255.0) as u8)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    a.lerp_to_gamma(b, t.clamp(0.0, 1.0))
}

fn mixf(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

pub fn ease(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

fn hover(ui: &Ui, resp: &Response, salt: &str, on: bool) -> f32 {
    ui.ctx().animate_bool_with_time(resp.id.with(salt), on, HOVER_S)
}

/// Dusky diagonal gradient, two soft glows and a barely visible dot grid.
pub fn backdrop(p: &egui::Painter, r: Rect) {
    let mut m = Mesh::default();
    for (pos, c) in [
        (r.left_top(), Color32::from_rgb(31, 28, 46)),
        (r.right_top(), Color32::from_rgb(22, 22, 35)),
        (r.left_bottom(), Color32::from_rgb(17, 16, 25)),
        (r.right_bottom(), Color32::from_rgb(11, 11, 15)),
    ] {
        m.colored_vertex(pos, c);
    }
    m.add_triangle(0, 1, 2);
    m.add_triangle(1, 3, 2);
    p.add(Shape::mesh(m));
    glow(p, pos2(r.right() - 70.0, r.top() + 20.0), 400.0, Color32::from_rgba_unmultiplied(72, 112, 255, 44));
    glow(p, pos2(r.left() + 30.0, r.bottom() - 30.0), 340.0, Color32::from_rgba_unmultiplied(196, 72, 168, 20));
    let step = 16.0;
    let mut y = r.top() + step / 2.0;
    while y < r.bottom() {
        let c = white(0.05 * (1.0 - (y - r.top()) / r.height()));
        let mut x = r.left() + step / 2.0;
        while x < r.right() {
            p.circle_filled(pos2(x, y), 0.8, c);
            x += step;
        }
        y += step;
    }
}

/// Radial gradient: `color` at the center fading to nothing at `radius`.
fn glow(p: &egui::Painter, center: Pos2, radius: f32, color: Color32) {
    let mut m = Mesh::default();
    m.colored_vertex(center, color);
    let n = 64u32;
    for ring in [0.45f32, 1.0] {
        let c = if ring < 1.0 { color.gamma_multiply(0.42) } else { Color32::TRANSPARENT };
        for i in 0..n {
            let a = i as f32 / n as f32 * TAU;
            m.colored_vertex(center + vec2(a.cos(), a.sin()) * radius * ring, c);
        }
    }
    let inner = |i: u32| 1 + i % n;
    let outer = |i: u32| 1 + n + i % n;
    for i in 0..n {
        m.add_triangle(0, inner(i), inner(i + 1));
        m.add_triangle(inner(i), outer(i), outer(i + 1));
        m.add_triangle(inner(i), outer(i + 1), inner(i + 1));
    }
    p.add(Shape::mesh(m));
}

/// A four-point sparkle made of two thin diamonds.
fn sparkle(p: &egui::Painter, c: Pos2, s: f32, color: Color32) {
    let k = 0.22;
    p.add(Shape::convex_polygon(
        vec![c + vec2(0.0, -s), c + vec2(s * k, 0.0), c + vec2(0.0, s), c + vec2(-s * k, 0.0)],
        color,
        Stroke::NONE,
    ));
    p.add(Shape::convex_polygon(
        vec![c + vec2(-s, 0.0), c + vec2(0.0, -s * k), c + vec2(s, 0.0), c + vec2(0.0, s * k)],
        color,
        Stroke::NONE,
    ));
}

/// Seconds between sparkle twinkles, and how long one lasts.
pub const TWINKLE_EVERY: f64 = 4.0;
pub const TWINKLE_LEN: f64 = 0.8;

/// The wordmark: per-letter gradient plus a sparkle that twinkles every few seconds.
pub fn title(ui: &mut Ui, text: &str, family: FontFamily, size: f32, time: f64) -> Response {
    let mut job = LayoutJob::default();
    let n = text.chars().count().max(2) - 1;
    for (i, ch) in text.chars().enumerate() {
        let color = mix(Color32::WHITE, Color32::from_rgb(160, 182, 255), i as f32 / n as f32);
        job.append(&ch.to_string(), 0.0, TextFormat { font_id: FontId::new(size, family.clone()), color, ..Default::default() });
    }
    let galley = ui.painter().layout_job(job);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(20.0, 0.0), Sense::hover());
    let gsize = galley.size();
    ui.painter().galley(rect.left_top(), galley, TEXT);
    let phase = time % TWINKLE_EVERY;
    let tw = if phase < TWINKLE_LEN { (phase / TWINKLE_LEN * PI as f64).sin() as f32 } else { 0.0 };
    let c = pos2(rect.left() + gsize.x + 9.0, rect.top() + size * 0.34);
    sparkle(ui.painter(), c, 5.5 + 3.5 * tw, mix(ACCENT_HI, Color32::WHITE, tw).gamma_multiply(0.6 + 0.4 * tw));
    resp
}

/// Small letter-spaced section title.
fn caption(ui: &mut Ui, text: &str, family: FontFamily) {
    let mut job = LayoutJob::default();
    job.append(
        text,
        0.0,
        TextFormat {
            font_id: FontId::new(11.5, family),
            color: FAINT,
            extra_letter_spacing: 1.4,
            ..Default::default()
        },
    );
    ui.label(job);
}

/// A translucent card that slides and fades in as `appear` goes 0 → 1.
pub fn card(ui: &mut Ui, title: &str, semibold: FontFamily, appear: f32, add: impl FnOnce(&mut Ui)) {
    let e = ease(appear);
    ui.with_visual_transform(TSTransform::from_translation(vec2(0.0, 14.0 * (1.0 - e))), |ui| {
        ui.multiply_opacity(e);
        egui::Frame::new()
            .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 7))
            .stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 15)))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(egui::Margin { left: 16, right: 16, top: 12, bottom: 10 })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                caption(ui, title, semibold);
                ui.add_space(4.0);
                add(ui);
            });
    });
    ui.add_space(8.0);
}

/// Label column + control column, with an optional hint under the control.
pub fn row(ui: &mut Ui, label: &str, hint: Option<(String, Color32)>, control: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(vec2(LABEL_W, 30.0), Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_width(LABEL_W);
            ui.label(RichText::new(label).color(TEXT));
        });
        ui.vertical(|ui| {
            control(ui);
            if let Some((h, c)) = hint {
                ui.label(RichText::new(h).small().color(c));
            }
        });
    });
    ui.add_space(4.0);
}

pub enum Kind {
    Primary,
    Secondary,
    Ghost,
}

pub fn button(ui: &mut Ui, text: &str, kind: Kind, enabled: bool) -> Response {
    let font = FontId::proportional(14.0);
    let tw = ui.painter().layout_no_wrap(text.to_string(), font.clone(), TEXT).size().x;
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(tw + 30.0, 32.0), sense);
    let hov = hover(ui, &resp, "hover", enabled && resp.hovered());
    let press = ui.ctx().animate_bool_with_time(resp.id.with("press"), enabled && resp.is_pointer_button_down_on(), 0.08);
    let on = ui.ctx().animate_bool_with_time(resp.id.with("on"), enabled, 0.22);
    let r = rect.shrink(1.2 * press);
    let p = ui.painter();
    let text_color = match kind {
        Kind::Primary => {
            let fill = mix(white(0.05), mix(ACCENT, ACCENT_HI, 0.35 * hov), on);
            if on * hov > 0.0 {
                p.rect_stroke(r.expand(2.0), 10, Stroke::new(3.0, ACCENT.gamma_multiply(0.22 * on * hov)), StrokeKind::Outside);
            }
            p.rect_filled(r, 8, fill);
            mix(FAINT, Color32::WHITE, on)
        }
        Kind::Secondary => {
            p.rect(r, 8, white(0.05 + 0.05 * hov), Stroke::new(1.0, white(0.07 + 0.07 * hov)), StrokeKind::Inside);
            mix(MUTED, TEXT, 0.6 + 0.4 * hov)
        }
        Kind::Ghost => {
            p.rect_filled(r, 8, white(0.05 * hov));
            mix(MUTED, TEXT, hov)
        }
    };
    p.text(r.center(), Align2::CENTER_CENTER, text, font, text_color);
    if enabled { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp }
}

/// A row of options with a highlight that slides to the selected one.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, salt: &str, value: &mut T, options: &[(T, String)]) -> Response {
    let h = 32.0;
    let (rect, mut resp) = ui.allocate_exact_size(vec2(CONTROL_W, h), Sense::hover());
    let id = resp.id.with(salt);
    let seg = CONTROL_W / options.len() as f32;
    let sel = options.iter().position(|(v, _)| v == value).unwrap_or(0);
    let x = ui.ctx().animate_value_with_time(id.with("thumb"), sel as f32, 0.22);
    let cell = |i: usize| Rect::from_min_size(pos2(rect.left() + i as f32 * seg, rect.top()), vec2(seg, h));

    let mut hovers = Vec::with_capacity(options.len());
    for (i, (v, _)) in options.iter().enumerate() {
        let r = ui.interact(cell(i), id.with(i), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        if r.clicked() && *value != *v {
            *value = *v;
            resp.mark_changed();
        }
        hovers.push(ui.ctx().animate_bool_with_time(id.with(("hover", i)), r.hovered(), HOVER_S));
    }

    let p = ui.painter();
    p.rect(rect, 9, white(0.04), Stroke::new(1.0, white(0.07)), StrokeKind::Inside);
    for (i, &hv) in hovers.iter().enumerate() {
        if hv > 0.0 {
            p.rect_filled(cell(i).shrink(3.0), 7, white(0.05 * hv));
        }
    }
    let thumb = Rect::from_min_size(pos2(rect.left() + x * seg, rect.top()), vec2(seg, h)).shrink(3.0);
    p.rect_filled(thumb, 7, ACCENT);
    p.rect_filled(Rect::from_min_size(thumb.left_top(), vec2(thumb.width(), 1.0)).shrink2(vec2(6.0, 0.0)), 0, white(0.25));
    for (i, (_, label)) in options.iter().enumerate() {
        let near = (1.0 - (x - i as f32).abs()).clamp(0.0, 1.0);
        let color = mix(mix(MUTED, TEXT, hovers[i]), Color32::WHITE, near);
        p.text(cell(i).center(), Align2::CENTER_CENTER, label, FontId::proportional(13.5), color);
    }
    resp
}

/// A switch with a sliding knob, followed by its label (both clickable).
pub fn toggle(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let font = FontId::proportional(14.0);
    let tw = ui.painter().layout_no_wrap(label.to_string(), font.clone(), TEXT).size().x;
    let sw = vec2(38.0, 22.0);
    let (rect, mut resp) = ui.allocate_exact_size(vec2(sw.x + 10.0 + tw, 26.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ease(ui.ctx().animate_bool_with_time(resp.id, *on, 0.2));
    let hov = hover(ui, &resp, "hover", resp.hovered());
    let track = Rect::from_min_size(pos2(rect.left(), rect.center().y - sw.y / 2.0), sw);
    let p = ui.painter();
    p.rect_filled(track, 11, mix(white(0.12 + 0.05 * hov), ACCENT, t));
    let knob = pos2(mixf(track.left() + 11.0, track.right() - 11.0, t), track.center().y);
    p.circle_filled(knob + vec2(0.0, 1.0), 8.0, Color32::from_black_alpha(60));
    p.circle_filled(knob, 8.0 + 0.6 * hov, Color32::WHITE);
    p.text(pos2(track.right() + 10.0, rect.center().y), Align2::LEFT_CENTER, label, font, mix(MUTED, TEXT, 0.75 + 0.25 * hov));
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

pub fn slider(ui: &mut Ui, value: &mut u32, range: RangeInclusive<u32>) -> Response {
    let (lo, hi) = (*range.start(), *range.end());
    let (rect, mut resp) = ui.allocate_exact_size(vec2(CONTROL_W, 26.0), Sense::click_and_drag());
    let track = Rect::from_min_max(pos2(rect.left() + 9.0, rect.center().y - 2.0), pos2(rect.right() - 9.0, rect.center().y + 2.0));
    if let Some(pos) = resp.interact_pointer_pos() {
        let t = ((pos.x - track.left()) / track.width()).clamp(0.0, 1.0);
        let v = lo + (t * (hi - lo) as f32).round() as u32;
        if v != *value {
            *value = v;
            resp.mark_changed();
        }
    }
    let t = ((*value).clamp(lo, hi) - lo) as f32 / (hi - lo) as f32;
    let shown = ui.ctx().animate_value_with_time(resp.id.with("value"), t, if resp.dragged() { 0.0 } else { 0.12 });
    let hov = hover(ui, &resp, "hover", resp.hovered() || resp.dragged());
    let p = ui.painter();
    p.rect_filled(track, 2, white(0.10));
    let kx = mixf(track.left(), track.right(), shown);
    let mut fill = Mesh::default();
    let filled = Rect::from_min_max(track.left_top(), pos2(kx, track.bottom()));
    for (pos, c) in [(filled.left_top(), ACCENT), (filled.right_top(), ACCENT_HI), (filled.left_bottom(), ACCENT), (filled.right_bottom(), ACCENT_HI)] {
        fill.colored_vertex(pos, c);
    }
    fill.add_triangle(0, 1, 2);
    fill.add_triangle(1, 3, 2);
    p.add(Shape::mesh(fill));
    let knob = pos2(kx, track.center().y);
    if hov > 0.0 {
        p.circle_filled(knob, 9.0 + 5.0 * hov, ACCENT.gamma_multiply(0.18 * hov));
    }
    p.circle_filled(knob + vec2(0.0, 1.0), 7.5, Color32::from_black_alpha(70));
    p.circle_filled(knob, 7.0 + 1.5 * hov, Color32::WHITE);
    let cursor = if resp.dragged() { CursorIcon::Grabbing } else { CursorIcon::Grab };
    resp.on_hover_cursor(cursor)
}

/// The hotkey as keycaps; while recording, a pulsing accent border and a prompt.
pub fn hotkey_field(ui: &mut Ui, hotkey: &str, listening: bool, time: f64) -> Response {
    let h = 38.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(CONTROL_W, h), Sense::click());
    let hov = hover(ui, &resp, "hover", resp.hovered());
    let lis = ui.ctx().animate_bool_with_time(resp.id.with("listening"), listening, 0.2);
    let pulse = 0.5 + 0.5 * (time * 3.4).sin() as f32;
    let p = ui.painter();
    if lis > 0.0 {
        p.rect_stroke(rect.expand(3.0), 13, Stroke::new(3.0, ACCENT.gamma_multiply(0.16 * lis * pulse)), StrokeKind::Outside);
    }
    let border = mix(white(0.08 + 0.08 * hov), ACCENT.gamma_multiply(0.55 + 0.45 * pulse), lis);
    p.rect(rect, 10, Color32::from_black_alpha(70), Stroke::new(1.0 + 0.5 * lis, border), StrokeKind::Inside);
    let small = FontId::proportional(12.0);
    if listening {
        p.text(rect.left_center() + vec2(14.0, 0.0), Align2::LEFT_CENTER, "press a shortcut…", FontId::proportional(14.0), TEXT);
        p.text(rect.right_center() - vec2(14.0, 0.0), Align2::RIGHT_CENTER, "esc to cancel", small, FAINT);
    } else {
        let mut x = rect.left() + 9.0;
        let cy = rect.center().y;
        for (i, part) in hotkey.split('+').enumerate() {
            if i > 0 {
                p.text(pos2(x + 6.0, cy), Align2::CENTER_CENTER, "+", FontId::proportional(13.0), FAINT);
                x += 12.0;
            }
            let g = p.layout_no_wrap(part.to_lowercase(), FontId::proportional(13.0), TEXT);
            let cap = Rect::from_min_size(pos2(x, cy - 12.0), vec2(g.size().x + 16.0, 24.0));
            p.rect_filled(cap.translate(vec2(0.0, 2.0)), 6, Color32::from_black_alpha(100));
            p.rect(cap, 6, white(0.10 + 0.04 * hov), Stroke::new(1.0, white(0.09)), StrokeKind::Inside);
            p.galley(cap.center() - g.size() / 2.0, g, TEXT);
            x = cap.right() + 2.0;
        }
        p.text(rect.right_center() - vec2(12.0, 0.0), Align2::RIGHT_CENTER, "click to change", small, FAINT.gamma_multiply(hov));
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Inline text link with an underline that grows in on hover.
pub fn link(ui: &mut Ui, text: &str) -> Response {
    let font = FontId::proportional(12.5);
    let g = ui.painter().layout_no_wrap(text.to_string(), font, MUTED);
    let (rect, resp) = ui.allocate_exact_size(g.size() + vec2(0.0, 3.0), Sense::click());
    let hov = hover(ui, &resp, "hover", resp.hovered());
    let p = ui.painter();
    p.text(rect.left_top(), Align2::LEFT_TOP, text, FontId::proportional(12.5), mix(MUTED, TEXT, hov));
    if hov > 0.0 {
        let y = rect.bottom() - 1.0;
        p.line_segment([pos2(rect.left(), y), pos2(rect.left() + rect.width() * ease(hov), y)], Stroke::new(1.0, mix(MUTED, ACCENT_HI, hov)));
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}
