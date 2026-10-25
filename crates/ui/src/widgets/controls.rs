use std::ops::RangeInclusive;

use eframe::egui::{
    self, Align, Align2, Color32, CursorIcon, EventFilter, FontId, Galley, Id, Key, Layout, Margin, Painter, Rect,
    Response, RichText, Role, Sense, Shape, Stroke, StrokeKind, Ui, UiBuilder, UiStackInfo, WidgetInfo, accesskit,
    pos2, vec2,
};

use super::motion::{anim, ease, glide, mix, reveal};
use super::tokens::{LABEL_GAP, LABEL_W, Look, Palette, medium, mono, mono_medium, sans};

/// 2 px ring in text colour with a 1 px gap, only after keyboard navigation.
pub(super) fn focus_ring(ui: &Ui, look: &Look, resp: &Response, rect: Rect, radius: f32) {
    if look.keyboard && resp.has_focus() {
        ui.painter().rect_stroke(rect.expand(2.0), radius + 2.0, Stroke::new(2.0, look.p.focus), StrokeKind::Middle);
    }
}

pub(super) fn bounds(r: Rect) -> accesskit::Rect {
    accesskit::Rect { x0: r.min.x.into(), y0: r.min.y.into(), x1: r.max.x.into(), y1: r.max.y.into() }
}

const ROW_TAG: &str = "glint-row";

pub(super) fn in_row(ui: &Ui, id: Id) {
    let Some(row) = ui.stack().iter().find_map(|s| s.tags().get_downcast::<Id>(ROW_TAG).copied()) else { return };
    if ui.ctx().accesskit_node_builder(id, |_| ()).is_some() {
        ui.data_mut(|d| d.get_temp_mut_or_default::<Vec<Id>>(row).push(id));
    }
}

fn name_inputs(ui: &Ui, row: Id, label: &str) {
    let inputs = ui.data_mut(|d| d.remove_temp::<Vec<Id>>(row)).unwrap_or_default();
    let only = inputs.len() == 1;
    for id in inputs {
        ui.ctx().accesskit_node_builder(id, |n| match n.label().filter(|l| !l.trim().is_empty()).map(str::to_owned) {
            Some(own) if only => {
                n.set_description(own);
                n.set_label(label);
            }
            Some(_) => {}
            None => n.set_label(label),
        });
    }
}

// ---- layout -----------------------------------------------------------------------------------

#[cfg(feature = "test-rows")]
thread_local! {
    pub static ROWS: std::cell::RefCell<Vec<(String, Rect, Rect)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Space above and below a row's content.
const ROW_PAD: f32 = 11.0;
/// The same for [`toggle_row`]: a bare switch reads lighter with less air.
const LIGHT_PAD: f32 = 9.0;
/// How far a [`nested_row`]'s label steps in.
const NEST: f32 = 12.0;

/// One settings row: a divider, the label wrapping in its fixed column, the control column.
/// `label_dy` lines the label's first line up with the control (7 for 32 px controls, 1 for
/// toggles, 0 for sliders).
pub fn row(ui: &mut Ui, look: &Look, label: &str, label_dy: f32, add: impl FnOnce(&mut Ui)) {
    framed_row(ui, look, label, label_dy, ROW_PAD, false, add);
}

/// A [`row`] that belongs to the one above it, shown only while that one is on (keystrokes'
/// position): its label steps in and dims.
pub fn nested_row(ui: &mut Ui, look: &Look, label: &str, label_dy: f32, add: impl FnOnce(&mut Ui)) {
    framed_row(ui, look, label, label_dy, ROW_PAD, true, add);
}

/// A [`toggle`] on a lighter row, for switches that need no hint.
pub fn toggle_row(ui: &mut Ui, look: &Look, label: &str, on: &mut bool, text: &str) -> Response {
    framed_row(ui, look, label, 1.0, LIGHT_PAD, false, |ui| toggle(ui, look, on, text))
}

fn framed_row<R>(ui: &mut Ui, look: &Look, label: &str, label_dy: f32, pad: f32, nested: bool, add: impl FnOnce(&mut Ui) -> R) -> R {
    let full = ui.available_rect_before_wrap();
    let top = full.top();
    ui.painter().hline(full.x_range(), top + 0.5, Stroke::new(1.0, look.p.line));
    let (indent, color) = if nested { (NEST, look.p.t2) } else { (0.0, look.p.t1) };
    let text = ui.painter().layout(label.to_owned(), medium(13.0), color, LABEL_W - indent);
    let label_rect = Rect::from_min_size(pos2(full.left() + indent, top + pad + label_dy), text.size());
    let column = Rect::from_min_max(pos2(full.left() + LABEL_W + LABEL_GAP, top + pad), full.right_bottom());
    let id = ui.next_auto_id();
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(column)
            .layout(Layout::top_down(Align::Min))
            .ui_stack_info(UiStackInfo::default().with_tag_value(ROW_TAG, id))
            .accessibility_role(Role::Group)
            .accessibility_label(label),
    );
    child.interact(label_rect, id.with("label"), Sense::hover()).widget_info(|| WidgetInfo::labeled(Role::Label, true, label));
    child.spacing_mut().item_spacing = vec2(8.0, 6.0);
    let out = add(&mut child);
    name_inputs(ui, id, label);
    let bottom = child.min_rect().bottom().max(label_rect.bottom()).max(top + pad) + pad;
    let rect = Rect::from_min_max(full.left_top(), pos2(full.right(), bottom));
    ui.ctx().accesskit_node_builder(child.unique_id(), |n| n.set_bounds(bounds(rect)));
    #[cfg(feature = "test-rows")]
    ROWS.with_borrow_mut(|rows| rows.push((label.to_owned(), label_rect, child.min_rect())));
    ui.painter().galley(label_rect.min, text, color);
    ui.advance_cursor_after_rect(rect);
    out
}

/// 12 px hint text under a control, wrapping.
pub fn hint(ui: &mut Ui, text: impl Into<String>, color: Color32) {
    ui.add(egui::Label::new(RichText::new(text).font(sans(12.0)).color(color).line_height(Some(16.0))).wrap());
}

/// A small mono heading over the rows of one group ("while recording").
pub fn subheading(ui: &mut Ui, look: &Look, title: &str) {
    ui.add_space(18.0);
    let text = ui.painter().layout_no_wrap(title.to_owned(), mono_medium(11.0), look.p.t3);
    let (rect, resp) = ui.allocate_exact_size(vec2(text.size().x, 14.0), Sense::hover());
    ui.painter().galley(pos2(rect.left(), rect.center().y - text.size().y / 2.0), text, look.p.t3);
    resp.widget_info(|| WidgetInfo::labeled(Role::Heading, true, title));
    ui.add_space(6.0);
}

/// A line of muted text where a row would be, for what's left out ("more options appear when…").
pub fn note_row(ui: &mut Ui, look: &Look, text: &str) {
    let full = ui.available_rect_before_wrap();
    ui.painter().hline(full.x_range(), full.top() + 0.5, Stroke::new(1.0, look.p.line));
    ui.add_space(14.0);
    hint(ui, text, look.p.t3);
    ui.add_space(14.0);
}

fn galley(ui: &Ui, text: &str, font: FontId) -> std::sync::Arc<Galley> {
    ui.painter().layout_no_wrap(text.to_string(), font, Color32::PLACEHOLDER)
}

// ---- controls ---------------------------------------------------------------------------------

pub enum Kind {
    /// Filled control with a ring (browse…, close / cancel).
    Secondary,
    /// Accent fill (save).
    Primary,
    /// Text only (reset …).
    Text,
}

pub fn button(ui: &mut Ui, look: &Look, text: &str, kind: Kind, height: f32, enabled: bool) -> Response {
    let p = &look.p;
    let pad = match kind {
        Kind::Secondary if height >= 30.0 => 14.0,
        Kind::Secondary => 12.0,
        Kind::Primary => 16.0,
        Kind::Text => 8.0,
    };
    let g = galley(ui, text, medium(13.0));
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 2.0 * pad, height), sense);
    let h = anim(ui, resp.id.with("hov"), enabled && resp.hovered(), 0.08);
    let d = anim(ui, resp.id.with("prs"), enabled && resp.is_pointer_button_down_on(), 0.08);
    let on = anim(ui, resp.id.with("on"), enabled, 0.12);
    let (fill, ring, color) = match kind {
        Kind::Secondary => {
            let fill = mix(mix(p.ctl, p.ctlh, h), p.ctlp, d);
            (mix(Color32::TRANSPARENT, fill, on), mix(p.line, p.line2, on), mix(p.t4, p.t1, on))
        }
        Kind::Primary => {
            let fill = mix(mix(p.acc, p.acch, h), p.accp, d);
            (mix(Color32::TRANSPARENT, fill, on), p.line2.gamma_multiply(1.0 - on), mix(p.t4, p.on_acc, on))
        }
        Kind::Text => {
            let fill = mix(mix(Color32::TRANSPARENT, p.hov, h), p.prs, d);
            (fill, Color32::TRANSPARENT, mix(p.t4, mix(p.t2, p.t1, h), on))
        }
    };
    let painter = ui.painter();
    painter.rect(rect, 5, fill, Stroke::new(1.0, ring), StrokeKind::Inside);
    painter.galley(rect.center() - g.size() / 2.0, g, color);
    focus_ring(ui, look, &resp, rect, 5.0);
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, text));
    if enabled { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp }
}

/// Underlined text link.
pub fn link(ui: &mut Ui, look: &Look, text: &str) -> Response {
    underlined(ui, look, text, sans(13.0), look.p.t1, 18.0)
}

fn underlined(ui: &mut Ui, look: &Look, text: &str, font: FontId, color: Color32, height: f32) -> Response {
    let g = galley(ui, text, font);
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x, height), Sense::click());
    let h = anim(ui, resp.id.with("hov"), resp.hovered(), 0.08);
    let top = rect.center().y - g.size().y / 2.0;
    let painter = ui.painter();
    painter.hline(rect.x_range(), (top + g.size().y).round() + 1.5, Stroke::new(1.0, mix(look.p.line3, look.p.t1, h)));
    painter.galley(pos2(rect.left(), top), g, mix(color, look.p.t1, h));
    focus_ring(ui, look, &resp, rect.expand2(vec2(2.0, 0.0)), 3.0);
    resp.widget_info(|| WidgetInfo::labeled(Role::Link, true, text));
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Draws keycaps from `x`, centred on `y`; returns where the next element starts.
pub fn keycaps(painter: &Painter, ui: &Ui, p: &Palette, mut x: f32, y: f32, keys: &[String], warn: bool) -> f32 {
    let color = if warn { p.warn } else { p.t1 };
    for (i, key) in keys.iter().enumerate() {
        if i > 0 {
            let plus = painter.text(pos2(x, y), Align2::LEFT_CENTER, "+", mono(12.0), p.t3);
            x = plus.right() + 5.0;
        }
        let g = galley(ui, key, mono_medium(12.0));
        let cap = Rect::from_min_size(pos2(x, y - 10.0), vec2(g.size().x + 12.0, 20.0));
        // 1 px border, 2 px at the bottom: the border colour underneath, the face on top.
        painter.rect_filled(cap, 3, if warn { p.warn } else { p.line2 });
        let face = Rect::from_min_max(cap.min + vec2(1.0, 1.0), cap.max - vec2(1.0, 2.0));
        painter.rect_filled(face, 2, if warn { p.field } else { p.ctl });
        painter.galley(pos2(face.left() + 5.0, face.center().y - g.size().y / 2.0), g, color);
        x = cap.right() + 5.0;
    }
    x
}

/// How wide [`keycaps`] draws `keys`.
fn keycaps_width(ui: &Ui, keys: &[String]) -> f32 {
    let plus = galley(ui, "+", mono(12.0)).size().x;
    let caps: f32 = keys.iter().map(|k| galley(ui, k, mono_medium(12.0)).size().x + 12.0).sum();
    caps + keys.len().saturating_sub(1) as f32 * (plus + 10.0)
}

/// A legend of keyboard shortcuts, wrapping: each "ctrl+c" as keycaps, what it does beside it.
pub fn shortcuts(ui: &mut Ui, look: &Look, keys: &[(&str, &str)]) {
    let p = &look.p;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(16.0, 8.0);
        for &(combo, what) in keys {
            let caps: Vec<String> = combo.split('+').map(str::to_owned).collect();
            let caps_w = keycaps_width(ui, &caps);
            let text = galley(ui, what, sans(12.0));
            let (rect, resp) = ui.allocate_exact_size(vec2(caps_w + 6.0 + text.size().x, 20.0), Sense::hover());
            let painter = ui.painter();
            let x = keycaps(painter, ui, p, rect.left(), rect.center().y, &caps, false);
            painter.galley(pos2(x + 1.0, rect.center().y - text.size().y / 2.0), text, p.t2);
            resp.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("{combo} {what}")));
        }
    });
}

#[derive(Clone, Copy)]
pub enum HotkeyState<'a> {
    Idle,
    /// Recording a new combo; `held` are the modifiers down right now.
    Listening { held: &'a [String] },
    /// The last combo (`keys`) can't be used, for reason `why`; still listening for the next one.
    Error { keys: &'a [String], why: &'a str },
}

/// One line of [`hotkeys`].
pub struct Hotkey<'a> {
    /// What it does: "record", "save".
    pub action: &'a str,
    pub keys: &'a [String],
    pub state: HotkeyState<'a>,
}

/// Height of one [`hotkeys`] line.
const HOTKEY_H: f32 = 36.0;

/// Hotkeys as a compact table, one line each: the action in the label column, its keycaps flush
/// right. Clicking a line records a new combo right there (esc cancels); a combo that can't be
/// used turns red with the reason beside it. Returns the line clicked.
pub fn hotkeys(ui: &mut Ui, look: &Look, lines: &[Hotkey]) -> Option<usize> {
    let p = &look.p;
    let w = ui.available_width();
    let mut clicked = None;
    for (i, line) in lines.iter().enumerate() {
        let (rect, resp) = ui.allocate_exact_size(vec2(w, HOTKEY_H), Sense::click());
        let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
        if resp.clicked() {
            clicked = Some(i);
        }
        let (listening, why) = match line.state {
            HotkeyState::Idle => (false, None),
            HotkeyState::Listening { .. } => (true, None),
            HotkeyState::Error { why, .. } => (false, Some(why)),
        };
        let h = anim(ui, resp.id.with("hov"), resp.hovered() || resp.has_focus(), 0.08);
        let l = ease(anim(ui, resp.id.with("lis"), listening, 0.1));
        let e = ease(anim(ui, resp.id.with("err"), why.is_some(), 0.12));
        let y = rect.center().y;
        let painter = ui.painter();
        painter.hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, p.line));
        let name = painter.layout_no_wrap(line.action.to_owned(), medium(13.0), p.t1);
        let name_rect = Rect::from_min_size(pos2(rect.left(), y - name.size().y / 2.0), name.size());
        painter.galley(name_rect.min, name, p.t1);

        // The keys sit bare; hovering, recording or a refusal puts them in a field, flush right.
        let (keys, warn, prompt) = match line.state {
            HotkeyState::Idle => (line.keys, false, None),
            HotkeyState::Listening { held: [] } => (&[][..], false, Some("press a shortcut…")),
            HotkeyState::Listening { held } => (held, false, Some("+ …")),
            HotkeyState::Error { keys, .. } => (keys, true, None),
        };
        let prompt_g = prompt.map(|t| galley(ui, t, if keys.is_empty() { sans(13.0) } else { mono(12.0) }));
        let gap = if keys.is_empty() { 0.0 } else { 5.0 };
        let inner = keycaps_width(ui, keys) + prompt_g.as_ref().map_or(0.0, |g| gap + g.size().x);
        let field = Rect::from_min_max(pos2(rect.right() - inner - 16.0, y - 14.0), pos2(rect.right(), y + 14.0));
        let shown = h.max(l).max(e);
        let border = mix(mix(p.line2, p.acc, l), p.warn, e);
        painter.rect(field, 5, p.field.gamma_multiply(shown), Stroke::new(1.0 + l.max(e), border.gamma_multiply(shown)), StrokeKind::Inside);
        let x = keycaps(painter, ui, p, field.left() + 8.0, y, keys, warn);
        if let Some(g) = prompt_g {
            let at = if keys.is_empty() { field.left() + 8.0 } else { x };
            painter.galley(pos2(at, y - g.size().y / 2.0), g, p.t2);
        }
        // Beside the field: how to back out while recording, or why the combo was refused.
        let room = field.left() - 12.0 - (name_rect.right() + 16.0);
        let aside = match why {
            Some(why) => Some((why, sans(12.0), p.warn)),
            None if listening => Some(("esc to cancel", mono(11.0), p.t3)),
            None => None,
        };
        if let Some((text, font, color)) = aside.filter(|_| room > 0.0) {
            let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
            job.wrap = egui::text::TextWrapping::truncate_at_width(room);
            let g = painter.layout_job(job);
            painter.galley(pos2(field.left() - 12.0 - g.size().x, y - g.size().y / 2.0), g, color);
        }
        focus_ring(ui, look, &resp, field, 5.0);
        #[cfg(feature = "test-rows")]
        ROWS.with_borrow_mut(|rows| rows.push((line.action.to_owned(), name_rect, field)));
        resp.widget_info(|| {
            let value = match line.state {
                HotkeyState::Idle => line.keys.join("+"),
                HotkeyState::Listening { held: [] } => "press a shortcut…".to_owned(),
                HotkeyState::Listening { held } => format!("{} + …", held.join("+")),
                HotkeyState::Error { keys, .. } => keys.join("+"),
            };
            WidgetInfo { current_text_value: Some(value), ..WidgetInfo::labeled(Role::Button, true, line.action) }
        });
        if let Some(why) = why {
            ui.ctx().accesskit_node_builder(resp.id, |n| n.set_description(why));
        }
    }
    clicked
}

/// A row of options; `width` None fills the column.
pub fn segmented<T: Copy + PartialEq>(
    ui: &mut Ui,
    look: &Look,
    value: &mut T,
    options: &[(T, &str)],
    width: Option<f32>,
) -> Response {
    let p = &look.p;
    let w = width.unwrap_or_else(|| ui.available_width());
    let (rect, mut resp) = ui.allocate_exact_size(vec2(w, 32.0), Sense::hover());
    let group = ui.new_child(UiBuilder::new().max_rect(rect).accessibility_role(Role::RadioGroup));
    ui.ctx().accesskit_node_builder(group.unique_id(), |n| n.set_bounds(bounds(rect)));
    in_row(ui, group.unique_id());
    ui.painter().rect(rect, 6, p.field, Stroke::new(1.0, p.line2), StrokeKind::Inside);
    let inner = rect.shrink(3.0);
    let n = options.len();
    let seg_w = (inner.width() - 2.0 * (n - 1) as f32) / n as f32;
    let mut focused = None;
    let mut cells = Vec::with_capacity(n);
    for (i, (v, _)) in options.iter().enumerate() {
        let r = Rect::from_min_size(pos2(inner.left() + i as f32 * (seg_w + 2.0), inner.top()), vec2(seg_w, 26.0));
        let cell = group.interact(r, resp.id.with(i), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        if cell.clicked() && *value != *v {
            *value = *v;
            resp.mark_changed();
        }
        if cell.has_focus() {
            focused = Some((i, cell.clone()));
        }
        cells.push((r, cell));
    }
    // Left / right move the selection (and focus) while a segment is focused.
    if let Some((i, cell)) = &focused {
        let filter = EventFilter { horizontal_arrows: true, ..Default::default() };
        ui.memory_mut(|m| m.set_focus_lock_filter(cell.id, filter));
        let step = ui.input(|inp| inp.key_pressed(Key::ArrowRight) as i32 - inp.key_pressed(Key::ArrowLeft) as i32);
        let j = (*i as i32 + step).clamp(0, n as i32 - 1) as usize;
        if j != *i {
            *value = options[j].0;
            resp.mark_changed();
            ui.memory_mut(|m| m.request_focus(resp.id.with(j)));
        }
    }
    // The selected segment's highlight slides; labels light up as it passes.
    let selected = options.iter().position(|(v, _)| *v == *value).unwrap_or(0);
    let pos = glide(ui, resp.id.with("pos"), selected as f32, 0.16);
    let painter = ui.painter();
    let highlight = Rect::from_min_size(pos2(inner.left() + pos * (seg_w + 2.0), inner.top()), vec2(seg_w, 26.0));
    painter.rect(highlight, 4, p.ctlh, Stroke::new(1.0, p.line2), StrokeKind::Inside);
    for (i, ((v, label), (r, cell))) in options.iter().zip(&cells).enumerate() {
        let near = (1.0 - (pos - i as f32).abs()).clamp(0.0, 1.0);
        let h = anim(ui, cell.id.with("hov"), cell.hovered(), 0.08);
        let font = if i == selected { medium(13.0) } else { sans(13.0) };
        painter.text(r.center(), Align2::CENTER_CENTER, *label, font, mix(p.t2, p.t1, near.max(h)));
        cell.widget_info(|| WidgetInfo::selected(Role::RadioButton, true, *v == *value, *label));
    }
    if look.keyboard && focused.is_some() {
        painter.rect_stroke(rect.expand(2.0), 8.0, Stroke::new(2.0, p.focus), StrokeKind::Middle);
    }
    resp
}

/// 16 px box with a white check when on, `text` beside it; the whole line is clickable.
pub fn checkbox(ui: &mut Ui, look: &Look, on: &mut bool, text: &str) -> Response {
    let p = &look.p;
    let g = galley(ui, text, sans(13.0));
    let (rect, mut resp) = ui.allocate_exact_size(vec2(26.0 + g.size().x, 18.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = anim(ui, resp.id, *on, 0.08);
    let h = anim(ui, resp.id.with("hov"), resp.hovered(), 0.08);
    let bx = Rect::from_min_size(pos2(rect.left(), rect.center().y - 8.0), vec2(16.0, 16.0));
    let painter = ui.painter();
    painter.rect(bx, 3, mix(p.field, p.acc, t), Stroke::new(1.0, p.line3.gamma_multiply(1.0 - t)), StrokeKind::Inside);
    painter.rect_filled(bx, 3, p.hov.gamma_multiply(h));
    if t > 0.0 {
        let check = vec![
            pos2(bx.left() + 4.0, bx.center().y + 0.5),
            pos2(bx.left() + 7.0, bx.bottom() - 4.5),
            pos2(bx.right() - 3.5, bx.top() + 4.5),
        ];
        painter.add(Shape::line(check, Stroke::new(2.0, p.on_acc.gamma_multiply(t))));
    }
    painter.galley(pos2(bx.right() + 10.0, rect.center().y - g.size().y / 2.0), g, p.t1);
    focus_ring(ui, look, &resp, bx, 3.0);
    resp.widget_info(|| WidgetInfo::selected(Role::CheckBox, true, *on, text));
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// 24 px colour disc; the selected one wears a ring in text colour.
pub fn swatch(ui: &mut Ui, look: &Look, color: Color32, selected: bool) -> Response {
    let p = &look.p;
    let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
    let h = anim(ui, resp.id.with("hov"), resp.hovered(), 0.08);
    let s = ease(anim(ui, resp.id.with("sel"), selected, 0.12));
    let painter = ui.painter();
    let c = rect.center();
    painter.circle_filled(c, 9.0 + h - 2.0 * s, color);
    painter.circle_stroke(c, 11.0, Stroke::new(1.5, p.t1.gamma_multiply(s)));
    painter.circle_stroke(c, 9.0 + h - 2.0 * s, Stroke::new(1.0, p.line2));
    focus_ring(ui, look, &resp, rect, 12.0);
    resp.widget_info(|| {
        WidgetInfo::selected(Role::RadioButton, true, selected, format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b()))
    });
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// 36 x 20 switch with `text` beside it.
pub fn toggle(ui: &mut Ui, look: &Look, on: &mut bool, text: &str) -> Response {
    let p = &look.p;
    let g = galley(ui, text, sans(13.0));
    let (rect, mut resp) = ui.allocate_exact_size(vec2(46.0 + g.size().x, 20.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ease(anim(ui, resp.id, *on, 0.14));
    let d = ease(anim(ui, resp.id.with("prs"), resp.is_pointer_button_down_on(), 0.08));
    let h = anim(ui, resp.id.with("hov"), resp.hovered(), 0.08);
    let track = Rect::from_min_size(rect.min, vec2(36.0, 20.0));
    let painter = ui.painter();
    painter.rect(track, 10, mix(p.field, p.acc, t), Stroke::new(1.0, p.line3.gamma_multiply(1.0 - t)), StrokeKind::Inside);
    painter.rect_filled(track, 10, p.hov.gamma_multiply(h));
    // The knob stretches toward its travel while pressed.
    let w = 14.0 + 4.0 * d;
    let x = track.left() + 3.0 + (16.0 - 4.0 * d) * t;
    let knob = Rect::from_min_size(pos2(x, track.top() + 3.0), vec2(w, 14.0));
    painter.rect_filled(knob, 7, mix(p.t2, p.on_acc, t));
    painter.galley(pos2(track.right() + 10.0, rect.center().y - g.size().y / 2.0), g, p.t2);
    focus_ring(ui, look, &resp, track, 10.0);
    resp.widget_info(|| WidgetInfo::selected(Role::CheckBox, true, *on, text));
    in_row(ui, resp.id);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Value readout, track, and the two ends of the scale.
pub fn slider(
    ui: &mut Ui,
    look: &Look,
    value: &mut u32,
    range: RangeInclusive<u32>,
    word: &str,
    lo: &str,
    hi: &str,
) -> Response {
    scale_slider(ui, look, None, value, range, word, lo, hi)
}

/// A [`slider`] whose readout starts with its own `label`, for several in one row (the
/// fine-tune inset of a [`preset_row`]).
#[allow(clippy::too_many_arguments)]
pub fn tuned_slider(
    ui: &mut Ui,
    look: &Look,
    label: &str,
    value: &mut u32,
    range: RangeInclusive<u32>,
    word: &str,
    lo: &str,
    hi: &str,
) -> Response {
    scale_slider(ui, look, Some(label), value, range, word, lo, hi)
}

/// Where a [`tuned_slider`]'s value starts, after its label.
const TUNED_VALUE_X: f32 = 104.0;

#[allow(clippy::too_many_arguments)]
fn scale_slider(
    ui: &mut Ui,
    look: &Look,
    label: Option<&str>,
    value: &mut u32,
    range: RangeInclusive<u32>,
    word: &str,
    lo: &str,
    hi: &str,
) -> Response {
    let p = look.p;
    let (min, max) = (*range.start(), *range.end());
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        let w = ui.available_width();
        let (readout, _) = ui.allocate_exact_size(vec2(w, 18.0), Sense::hover());
        let (track, mut resp) = ui.allocate_exact_size(vec2(w, 20.0), Sense::click_and_drag());
        let (scale, _) = ui.allocate_exact_size(vec2(w, 14.0), Sense::hover());

        // a value outside the range (one the caller is about to replace) shows at its nearer end
        let mut v = (*value).clamp(min, max);
        let shown = v;
        if let Some(pos) = resp.interact_pointer_pos()
            && (resp.is_pointer_button_down_on() || resp.clicked())
        {
            let t = ((pos.x - track.left()) / track.width()).clamp(0.0, 1.0);
            v = (min as f32 + t * (max - min) as f32).round() as u32;
        }
        if resp.has_focus() {
            let filter = EventFilter { horizontal_arrows: true, vertical_arrows: true, ..Default::default() };
            ui.memory_mut(|m| m.set_focus_lock_filter(resp.id, filter));
            let step = ui.input(|i| {
                let k = |key| i.key_pressed(key) as i32;
                k(Key::ArrowRight) + k(Key::ArrowUp) - k(Key::ArrowLeft) - k(Key::ArrowDown) + 5 * (k(Key::PageUp) - k(Key::PageDown))
            });
            v = (v as i32 + step).clamp(min as i32, max as i32) as u32;
            if ui.input(|i| i.key_pressed(Key::Home)) {
                v = min;
            }
            if ui.input(|i| i.key_pressed(Key::End)) {
                v = max;
            }
        }
        if v != shown {
            *value = v;
            resp.mark_changed();
        }

        let painter = ui.painter();
        let y = readout.center().y;
        let x = match label {
            Some(label) => {
                painter.text(pos2(readout.left(), y), Align2::LEFT_CENTER, label, medium(12.0), p.t2);
                readout.left() + TUNED_VALUE_X
            }
            None => readout.left(),
        };
        let size = if label.is_some() { 12.0 } else { 13.0 };
        painter.text(pos2(x, y), Align2::LEFT_CENTER, v.to_string(), mono_medium(12.0), p.t1);
        painter.text(pos2(x + 28.0, y), Align2::LEFT_CENTER, word, sans(size), if label.is_some() { p.t3 } else { p.t2 });

        // The thumb glides to clicks, keys and resets, and follows the pointer while dragged.
        let target = (v - min) as f32 / (max - min).max(1) as f32;
        let t = glide(ui, resp.id.with("pos"), target, if resp.dragged() { 0.0 } else { 0.14 });
        let kx = track.left() + t * track.width();
        let line = |x0: f32, x1: f32, c: Color32| {
            painter.rect_filled(Rect::from_min_max(pos2(x0, track.top() + 9.0), pos2(x1, track.top() + 11.0)), 1, c)
        };
        line(track.left(), track.right(), p.line2);
        line(track.left(), kx, p.acc);
        let grow = ease(anim(ui, resp.id.with("grow"), resp.hovered() || resp.dragged(), 0.08));
        let drag = anim(ui, resp.id.with("drag"), resp.dragged(), 0.08);
        let thumb = Rect::from_center_size(pos2(kx, track.center().y), vec2(6.0 + 2.0 * grow, 16.0 + 2.0 * grow));
        painter.rect(thumb, 3, mix(p.t1, p.acc, drag), Stroke::new(1.0, p.surf), StrokeKind::Outside);

        painter.text(scale.left_center(), Align2::LEFT_CENTER, lo, mono(11.0), p.t3);
        painter.text(scale.right_center(), Align2::RIGHT_CENTER, hi, mono(11.0), p.t3);
        focus_ring(ui, look, &resp, track, 3.0);
        resp.widget_info(|| WidgetInfo::slider(true, *value as f64, label.unwrap_or("")));
        ui.ctx().accesskit_node_builder(resp.id, |n| {
            n.set_min_numeric_value(min as f64);
            n.set_max_numeric_value(max as f64);
            n.set_numeric_value_step(1.0);
            n.set_value(format!("{value} {word}").trim_end());
        });
        in_row(ui, resp.id);
        resp.on_hover_cursor(CursorIcon::PointingHand)
    })
    .inner
}

/// A row of presets that each set several values at once (quality and encoder effort), the
/// current preset's `hint` with a "fine-tune" link beside it, and `fine_tune` (the values
/// themselves, as [`tuned_slider`]s) in an inset the link opens. `current` is the preset the
/// values match; `None` adds a selected "custom" segment. Returns the preset picked this frame,
/// for the caller to apply.
pub fn preset_row<T: Copy + PartialEq>(
    ui: &mut Ui,
    look: &Look,
    label: &str,
    current: Option<T>,
    presets: &[(T, &str)],
    hint_text: &str,
    fine_tune: impl FnOnce(&mut Ui),
) -> Option<T> {
    let p = look.p;
    let open_id = Id::unique(("fine-tune", label));
    let mut open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
    let mut picked = None;
    row(ui, look, label, 7.0, |ui| {
        let mut options: Vec<(Option<T>, &str)> = presets.iter().map(|&(v, l)| (Some(v), l)).collect();
        if current.is_none() {
            options.push((None, "custom"));
        }
        let mut value = current;
        if segmented(ui, look, &mut value, &options, None).changed() {
            picked = value;
        }
        let link_text = if open { "hide details" } else { "fine-tune" };
        let link_w = galley(ui, link_text, sans(12.0)).size().x;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let left = (ui.available_width() - link_w - 12.0).max(0.0);
            ui.allocate_ui_with_layout(vec2(left, 16.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(left);
                hint(ui, hint_text, p.t3);
            });
            ui.add_space(12.0);
            if underlined(ui, look, link_text, sans(12.0), p.t2, 16.0).clicked() {
                open = !open;
            }
        });
        reveal(ui, open_id.with("inset"), open, |ui| {
            ui.add_space(5.0);
            egui::Frame::new()
                .fill(mix(p.surf, p.field, 0.4))
                .stroke(Stroke::new(1.0, p.line))
                .corner_radius(5)
                .inner_margin(Margin { left: 12, right: 12, top: 10, bottom: 12 })
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 12.0;
                    fine_tune(ui);
                });
        });
    });
    ui.data_mut(|d| d.insert_temp(open_id, open));
    picked
}

/// 32 px inset text field in mono.
pub fn text_field(ui: &mut Ui, look: &Look, text: &mut String, placeholder: &str, warn: bool) -> Response {
    let p = &look.p;
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, 32.0), Sense::hover());
    let background = ui.painter().add(Shape::Noop);
    let edit = egui::TextEdit::singleline(text)
        .frame(egui::Frame::NONE.inner_margin(Margin::symmetric(10, 0)))
        .font(mono(12.0))
        .text_color(p.t1)
        .hint_text(RichText::new(placeholder).font(mono(12.0)).color(p.t3))
        .align(Align2::LEFT_CENTER)
        .desired_width(w)
        .min_size(vec2(w, 32.0));
    let resp = ui.put(rect, edit);
    let h = anim(ui, resp.id.with("hov"), resp.hovered(), 0.08);
    let f = anim(ui, resp.id.with("foc"), resp.has_focus(), 0.08);
    let border = if warn { p.warn } else { mix(mix(p.line2, p.line3, h), p.t2, f) };
    let shape = egui::epaint::RectShape::new(rect, 5, p.field, Stroke::new(1.0 + f, border), StrokeKind::Inside);
    ui.painter().set(background, shape);
    in_row(ui, resp.id);
    resp
}

pub fn dropdown(ui: &mut Ui, look: &Look, text: &str, items: &[String], width: f32) -> Option<usize> {
    let rows: Vec<(&str, &str)> = items.iter().map(|i| (i.as_str(), "")).collect();
    dropdown_detail(ui, look, text, &rows, width, width).picked
}

pub struct Dropped {
    pub picked: Option<usize>,
    pub opened: bool,
    pub response: Response,
}

pub fn dropdown_detail(
    ui: &mut Ui,
    look: &Look,
    text: &str,
    rows: &[(&str, &str)],
    width: f32,
    list_width: f32,
) -> Dropped {
    let p = &look.p;
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 32.0), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp));
    let opened = resp.clicked() && !open;
    let h = anim(ui, resp.id.with("hov"), resp.hovered() || open, 0.08);
    let painter = ui.painter();
    painter.rect(rect, 5, mix(p.field, p.ctl, h), Stroke::new(1.0, mix(p.line2, p.line3, h)), StrokeKind::Inside);
    let label = galley(ui, text, sans(13.0));
    let text_area = Rect::from_min_max(pos2(rect.left() + 10.0, rect.top()), pos2(rect.right() - 28.0, rect.bottom()));
    let clip = painter.with_clip_rect(text_area.intersect(painter.clip_rect()));
    clip.galley(pos2(rect.left() + 10.0, rect.center().y - label.size().y / 2.0), label, p.t1);
    let c = pos2(rect.right() - 16.0, rect.center().y);
    let chevron = vec![c + vec2(-4.0, -2.0), c + vec2(0.0, 2.0), c + vec2(4.0, -2.0)];
    painter.add(Shape::line(chevron, Stroke::new(1.5, mix(p.t3, p.t1, h))));
    focus_ring(ui, look, &resp, rect, 5.0);
    resp.widget_info(|| WidgetInfo { current_text_value: Some(text.to_owned()), ..WidgetInfo::new(Role::ComboBox) });
    ui.ctx().accesskit_node_builder(resp.id, |n| n.set_expanded(open));
    in_row(ui, resp.id);
    let frame = egui::Frame::NONE.fill(p.surf).stroke(Stroke::new(1.0, p.line2)).corner_radius(6).inner_margin(4);
    let align = if list_width > width { egui::RectAlign::BOTTOM_END } else { egui::RectAlign::BOTTOM_START };
    let popup = egui::Popup::from_toggle_button_response(&resp).width(list_width).align(align).gap(4.0).frame(frame);
    let mut picked = None;
    popup.show(|ui| {
        egui::ScrollArea::vertical().max_height(336.0).auto_shrink([false, true]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let row_w = list_width - 8.0;
            for (i, (name, detail)) in rows.iter().enumerate() {
                let (r, item_resp) = ui.allocate_exact_size(vec2(row_w, 28.0), Sense::click());
                let hov = anim(ui, item_resp.id.with("hov"), item_resp.hovered(), 0.06);
                ui.painter().rect_filled(r, 4, p.hov.gamma_multiply(hov));
                let clip = ui.painter().with_clip_rect(r.shrink2(vec2(8.0, 0.0)).intersect(ui.clip_rect()));
                let g = galley(ui, name, sans(13.0));
                let after = r.left() + 8.0 + g.size().x + 10.0;
                clip.galley(pos2(r.left() + 8.0, r.center().y - g.size().y / 2.0), g, p.t1);
                if !detail.is_empty() {
                    let d = galley(ui, detail, sans(12.0));
                    clip.galley(pos2(after, r.center().y - d.size().y / 2.0), d, p.t3);
                }
                item_resp.widget_info(|| WidgetInfo::labeled(Role::ListBoxOption, true, name));
                if item_resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    picked = Some(i);
                }
            }
        });
    });
    Dropped { picked, opened, response: resp }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::fonts;
    use crate::widgets::palette;

    /// Where the slider's thumb is drawn (its centre x) after a frame showing `value`.
    fn thumb(ctx: &egui::Context, value: u32, time: f64) -> f32 {
        let look = Look { p: palette(false, [0xE5, 0x40, 0x2F]), keyboard: false };
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 100.0))),
            time: Some(time),
            ..Default::default()
        };
        let out = ctx.run_ui(input, |ui| {
            let mut v = value;
            slider(ui, &look, &mut v, 4..=60, "", "", "");
        });
        out.shapes
            .iter()
            .find_map(|c| match &c.shape {
                Shape::Rect(r) if r.rect.height() >= 16.0 && r.rect.width() <= 8.0 => Some(r.rect.center().x),
                _ => None,
            })
            .expect("the thumb")
    }

    #[test]
    fn a_value_below_the_range_rests_at_its_end_and_moves_on_from_there() {
        let ctx = egui::Context::default();
        fonts(&ctx);
        let mut time = 0.0;
        let mut at = |v| {
            time += 0.05;
            thumb(&ctx, v, time)
        };
        let start = at(4);
        for _ in 0..10 {
            at(0);
        }
        assert!((at(0) - start).abs() < 0.5, "0 shows like 4, at the left end");
        let fifteen = (0..20).map(|_| at(15)).last().unwrap();
        let width = 300.0 - 2.0 * 8.0;
        let expected = start + (15.0 - 4.0) / (60.0 - 4.0) * width;
        assert!((fifteen - expected).abs() < 20.0, "15 lands near {expected}, not at {fifteen}");
    }
}
