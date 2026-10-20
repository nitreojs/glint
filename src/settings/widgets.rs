//! Tokens and hand-drawn controls of the settings window (design 1a, "ledger"): warm graphite,
//! labels in a fixed column, red only on the mark, the selected section number, active
//! toggles and save.

use std::ops::RangeInclusive;
use std::sync::LazyLock;

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, CursorIcon, EventFilter, FontFamily, FontId, Galley, Id, Key, Layout,
    Margin, Painter, PointerButton, Rect, Response, RichText, Sense, Shape, Stroke, StrokeKind, Ui, UiBuilder, pos2,
    vec2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};
use windows::core::BOOL;

pub const WINDOW: [f32; 2] = [680.0, 600.0];
pub const BAR_H: f32 = 40.0;
pub const SIDEBAR_W: f32 = 168.0;
pub const FOOTER_H: f32 = 56.0;
pub const CONTENT_X: f32 = 28.0;
const LABEL_W: f32 = 132.0;
const LABEL_GAP: f32 = 16.0;
/// Window-control colours are Windows' own, in both themes.
const CLOSE_HOVER: Color32 = Color32::from_rgb(0xC4, 0x2B, 0x1C);
const CLOSE_PRESSED: Color32 = Color32::from_rgb(0xA7, 0x24, 0x17);
const MARK: Color32 = Color32::from_rgb(0xE5, 0x40, 0x2F);

#[derive(Clone, Copy, PartialEq)]
pub struct Palette {
    /// Title bar and sidebar.
    pub side: Color32,
    pub surf: Color32,
    /// Inset fields, segmented tracks, off toggles.
    pub field: Color32,
    pub ctl: Color32,
    pub ctlh: Color32,
    pub ctlp: Color32,
    pub hov: Color32,
    pub prs: Color32,
    pub sel: Color32,
    pub line: Color32,
    pub line2: Color32,
    pub line3: Color32,
    pub t1: Color32,
    pub t2: Color32,
    pub t3: Color32,
    pub t4: Color32,
    pub acc: Color32,
    pub acch: Color32,
    pub accp: Color32,
    /// Accent for text (the selected section number).
    pub acct: Color32,
    pub warn: Color32,
    pub focus: Color32,
}

const fn rgb(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// White at `a` alpha (premultiplied), usable in constants.
const fn white(a: u8) -> Color32 {
    Color32::from_rgba_premultiplied(a, a, a, a)
}

pub const DARK: Palette = Palette {
    side: rgb(0x1A1A19),
    surf: rgb(0x222221),
    field: rgb(0x171716),
    ctl: rgb(0x2C2C2B),
    ctlh: rgb(0x353534),
    ctlp: rgb(0x262625),
    hov: white(11),
    prs: white(20),
    sel: rgb(0x2C2C2B),
    line: white(18),
    line2: white(33),
    line3: white(66),
    t1: rgb(0xEDEBE7),
    t2: rgb(0xB0ADA7),
    t3: rgb(0x8E8B85),
    t4: rgb(0x5C5A56),
    acc: rgb(0xCF3D2C),
    acch: rgb(0xDB4A38),
    accp: rgb(0xB53424),
    acct: rgb(0xF06A55),
    warn: rgb(0xE3A33C),
    focus: rgb(0xEDEBE7),
};

pub const LIGHT: Palette = Palette {
    side: rgb(0xF1F0ED),
    surf: rgb(0xFAF9F7),
    field: rgb(0xFFFFFF),
    ctl: rgb(0xFFFFFF),
    ctlh: rgb(0xF4F3F0),
    ctlp: rgb(0xECEBE7),
    hov: Color32::from_black_alpha(11),
    prs: Color32::from_black_alpha(20),
    sel: rgb(0xE5E3DF),
    line: Color32::from_black_alpha(20),
    line2: Color32::from_black_alpha(38),
    line3: Color32::from_black_alpha(77),
    t1: rgb(0x1C1B19),
    t2: rgb(0x4F4C47),
    t3: rgb(0x68655F),
    t4: rgb(0xA29E97),
    acc: rgb(0xC43A29),
    acch: rgb(0xB23322),
    accp: rgb(0x9E2D1E),
    acct: rgb(0xC43A29),
    warn: rgb(0x9A5F06),
    focus: rgb(0x1C1B19),
};

/// What every control needs to draw itself this frame.
pub struct Look {
    pub p: Palette,
    /// The last input was Tab / arrows: focus rings show only then.
    pub keyboard: bool,
}

// ---- type -------------------------------------------------------------------------------------

pub static MEDIUM: LazyLock<FontFamily> = LazyLock::new(|| FontFamily::Name("sans-medium".into()));
pub static SEMIBOLD: LazyLock<FontFamily> = LazyLock::new(|| FontFamily::Name("sans-semibold".into()));
pub static MONO_MEDIUM: LazyLock<FontFamily> = LazyLock::new(|| FontFamily::Name("mono-medium".into()));

pub fn sans(size: f32) -> FontId {
    FontId::proportional(size)
}
pub fn medium(size: f32) -> FontId {
    FontId::new(size, MEDIUM.clone())
}
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, SEMIBOLD.clone())
}
pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}
pub fn mono_medium(size: f32) -> FontId {
    FontId::new(size, MONO_MEDIUM.clone())
}

// ---- motion -----------------------------------------------------------------------------------

/// Windows' "show animations in Windows" setting; when it's off every transition snaps.
static MOTION: LazyLock<bool> = LazyLock::new(|| {
    let mut on = BOOL(1);
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut on as *mut BOOL as _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    ok.is_err() || on.as_bool()
});

/// Linear 0..=1 progress toward `on`, over `secs`.
pub fn anim(ui: &Ui, id: Id, on: bool, secs: f32) -> f32 {
    if *MOTION {
        ui.ctx().animate_bool_with_time(id, on, secs)
    } else if on {
        1.0
    } else {
        0.0
    }
}

/// The design's ease-out, cubic-bezier(0.2, 0, 0, 1).
pub fn ease(t: f32) -> f32 {
    let bez = |s: f32, a: f32, b: f32| 3.0 * (1.0 - s).powi(2) * s * a + 3.0 * (1.0 - s) * s * s * b + s * s * s;
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..20 {
        let mid = (lo + hi) / 2.0;
        if bez(mid, 0.2, 0.0) < t { lo = mid } else { hi = mid }
    }
    bez((lo + hi) / 2.0, 0.0, 1.0)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    a.lerp_to_gamma(b, t.clamp(0.0, 1.0))
}

#[derive(Clone, Copy)]
struct Glide {
    from: f32,
    to: f32,
    start: f64,
}

/// `target`, reached with the design's ease-out over `secs`; when the target changes mid-way
/// the motion restarts from wherever it is, so it never jumps. `secs` 0 follows exactly.
pub fn glide(ui: &Ui, id: Id, target: f32, secs: f32) -> f32 {
    let now = ui.input(|i| i.time);
    let ctx = ui.ctx();
    let progress = |g: &Glide| if *MOTION && secs > 0.0 { ((now - g.start) as f32 / secs).clamp(0.0, 1.0) } else { 1.0 };
    let value = |g: &Glide| g.from + (g.to - g.from) * ease(progress(g));
    let mut g = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(id, || Glide { from: target, to: target, start: now }));
    if g.to != target {
        g = Glide { from: value(&g), to: target, start: now };
        ctx.data_mut(|d| d.insert_temp(id, g));
    }
    if progress(&g) < 1.0 {
        ctx.request_repaint();
    }
    value(&g)
}

/// 2 px ring in text colour with a 1 px gap, only after keyboard navigation.
fn focus_ring(ui: &Ui, look: &Look, resp: &Response, rect: Rect, radius: f32) {
    if look.keyboard && resp.has_focus() {
        ui.painter().rect_stroke(rect.expand(2.0), radius + 2.0, Stroke::new(2.0, look.p.focus), StrokeKind::Middle);
    }
}

// ---- layout -----------------------------------------------------------------------------------

/// One settings row: a divider, the label in its fixed column, the control column. `label_dy`
/// lines the label up with the control (7 for 32 px controls, 1 for toggles, 0 for sliders).
pub fn row(ui: &mut Ui, look: &Look, label: &str, label_dy: f32, add: impl FnOnce(&mut Ui)) {
    let full = ui.available_rect_before_wrap();
    let top = full.top();
    ui.painter().hline(full.x_range(), top + 0.5, Stroke::new(1.0, look.p.line));
    let column = Rect::from_min_max(pos2(full.left() + LABEL_W + LABEL_GAP, top + 15.0), full.right_bottom());
    let mut child = ui.new_child(UiBuilder::new().max_rect(column).layout(Layout::top_down(Align::Min)));
    child.spacing_mut().item_spacing = vec2(8.0, 6.0);
    add(&mut child);
    let bottom = child.min_rect().bottom().max(top + 15.0) + 14.0;
    ui.painter().text(pos2(full.left(), top + 15.0 + label_dy), Align2::LEFT_TOP, label, medium(13.0), look.p.t1);
    ui.advance_cursor_after_rect(Rect::from_min_max(full.left_top(), pos2(full.right(), bottom)));
}

/// 12 px hint text under a control, wrapping.
pub fn hint(ui: &mut Ui, text: impl Into<String>, color: Color32) {
    ui.add(egui::Label::new(RichText::new(text).font(sans(12.0)).color(color).line_height(Some(16.0))).wrap());
}

fn galley(ui: &Ui, text: &str, font: FontId) -> std::sync::Arc<Galley> {
    ui.painter().layout_no_wrap(text.to_string(), font, Color32::PLACEHOLDER)
}

// ---- window chrome ----------------------------------------------------------------------------

pub enum TitleAction {
    None,
    Minimize,
    Close,
}

/// The 40 px bar: mark, name, drag area, minimize and close. `active` is 1 while focused.
pub fn title_bar(ui: &mut Ui, look: &Look, rect: Rect, active: f32, radius: u8) -> TitleAction {
    let p = &look.p;
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius { nw: radius, ne: radius, sw: 0, se: 0 }, p.side);

    // The mark: a 14 px disc whose highlight is cut out (side colour) while inactive.
    let disc = Rect::from_min_size(pos2(rect.left() + 18.0, rect.center().y - 7.0), vec2(14.0, 14.0));
    painter.circle_filled(disc.center(), 7.0, mix(p.t4, MARK, active));
    painter.circle_filled(pos2(disc.right() - 4.0, disc.top() + 4.0), 2.0, mix(p.side, Color32::WHITE, active));
    let name = painter.text(
        pos2(disc.right() + 9.0, rect.center().y),
        Align2::LEFT_CENTER,
        "glint",
        semibold(13.0),
        mix(p.t4, p.t1, active),
    );
    painter.text(pos2(name.right() + 9.0, rect.center().y), Align2::LEFT_CENTER, "settings", sans(13.0), mix(p.t4, p.t3, active));

    // Everything but the buttons moves the window; a double-click does nothing (fixed size).
    let buttons = 2.0 * 46.0;
    let drag = ui.interact(
        Rect::from_min_max(rect.min, pos2(rect.right() - buttons, rect.bottom())),
        Id::unique("title-drag"),
        Sense::CLICK | Sense::DRAG,
    );
    if drag.is_pointer_button_down_on() && ui.input(|i| i.pointer.button_pressed(PointerButton::Primary)) {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }

    let fg = mix(p.t4, p.t1, active);
    let min_rect = Rect::from_min_max(pos2(rect.right() - buttons, rect.top()), pos2(rect.right() - 46.0, rect.bottom()));
    let min = ui.interact(min_rect, Id::unique("title-min"), Sense::CLICK);
    let (h, d) = (anim(ui, min.id, min.hovered(), 0.08), anim(ui, min.id.with("p"), min.is_pointer_button_down_on(), 0.08));
    let painter = ui.painter();
    painter.rect_filled(min_rect, 0, p.hov.gamma_multiply(h * (1.0 - d)));
    painter.rect_filled(min_rect, 0, p.prs.gamma_multiply(d));
    let c = min_rect.center();
    painter.hline((c.x - 5.0).round()..=(c.x + 5.0).round(), c.y.round() + 0.5, Stroke::new(1.0, fg));

    let close_rect = Rect::from_min_max(pos2(rect.right() - 46.0, rect.top()), rect.right_bottom());
    let close = ui.interact(close_rect, Id::unique("title-close"), Sense::CLICK);
    let (h, d) =
        (anim(ui, close.id, close.hovered(), 0.08), anim(ui, close.id.with("p"), close.is_pointer_button_down_on(), 0.08));
    let painter = ui.painter();
    let corner = CornerRadius { nw: 0, ne: radius, sw: 0, se: 0 };
    painter.rect_filled(close_rect, corner, mix(Color32::TRANSPARENT, mix(CLOSE_HOVER, CLOSE_PRESSED, d), h.max(d)));
    let glyph = mix(fg, mix(Color32::WHITE, Color32::from_white_alpha(204), d), h.max(d));
    let c = pos2(close_rect.center().x.round() + 0.5, close_rect.center().y.round() + 0.5);
    for (dx, dy) in [(5.0, 5.0), (5.0, -5.0)] {
        painter.line_segment([pos2(c.x - dx, c.y - dy), pos2(c.x + dx, c.y + dy)], Stroke::new(1.0, glyph));
    }

    if min.clicked() {
        TitleAction::Minimize
    } else if close.clicked() {
        TitleAction::Close
    } else {
        TitleAction::None
    }
}

/// A sidebar entry: mono index, label, and a dot while the section has unsaved changes. The
/// selection background is drawn by the caller and slides between entries; `near` (0..=1)
/// says how close it is, for the colours.
pub fn nav_item(
    ui: &mut Ui,
    look: &Look,
    rect: Rect,
    index: usize,
    label: &str,
    (selected, near): (bool, f32),
    dirty: bool,
) -> Response {
    let p = &look.p;
    let resp = ui.interact(rect, Id::unique(("nav", index)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
    let h = anim(ui, resp.id.with("hov"), resp.hovered(), 0.08);
    let dot = anim(ui, resp.id.with("dirty"), dirty, 0.12);
    let painter = ui.painter();
    painter.rect_filled(rect, 5, mix(Color32::TRANSPARENT, p.hov, h * (1.0 - near)));
    let y = rect.center().y;
    let (num_font, text_font) = if selected { (mono_medium(11.0), medium(13.0)) } else { (mono(11.0), sans(13.0)) };
    painter.text(pos2(rect.left() + 10.0, y), Align2::LEFT_CENTER, index.to_string(), num_font, mix(p.t3, p.acct, near));
    painter.text(pos2(rect.left() + 32.0, y), Align2::LEFT_CENTER, label, text_font, mix(p.t2, p.t1, near));
    if dot > 0.0 {
        painter.circle_filled(pos2(rect.right() - 12.5, y), 2.5, p.t2.gamma_multiply(dot));
    }
    focus_ring(ui, look, &resp, rect, 5.0);
    resp
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
            (mix(Color32::TRANSPARENT, fill, on), p.line2.gamma_multiply(1.0 - on), mix(p.t4, Color32::WHITE, on))
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
    if enabled { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp }
}

/// Underlined text link.
pub fn link(ui: &mut Ui, look: &Look, text: &str) -> Response {
    let g = galley(ui, text, sans(13.0));
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x, 18.0), Sense::click());
    let h = anim(ui, resp.id.with("hov"), resp.hovered(), 0.08);
    let top = rect.center().y - g.size().y / 2.0;
    let painter = ui.painter();
    painter.hline(rect.x_range(), (top + g.size().y).round() + 1.5, Stroke::new(1.0, mix(look.p.line3, look.p.t1, h)));
    painter.galley(pos2(rect.left(), top), g, look.p.t1);
    focus_ring(ui, look, &resp, rect.expand2(vec2(2.0, 0.0)), 3.0);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Draws keycaps from `x`, centred on `y`; returns where the next element starts.
fn keycaps(painter: &Painter, ui: &Ui, p: &Palette, mut x: f32, y: f32, keys: &[String], warn: bool) -> f32 {
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

pub enum HotkeyState<'a> {
    Idle,
    /// Recording a new combo; `held` are the modifiers down right now.
    Listening { held: &'a [String] },
    /// The last combo can't be used; still listening for the next one.
    Error { keys: &'a [String] },
}

pub fn hotkey_field(ui: &mut Ui, look: &Look, keys: &[String], state: HotkeyState) -> Response {
    let p = &look.p;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    let listening = matches!(state, HotkeyState::Listening { .. });
    let error = matches!(state, HotkeyState::Error { .. });
    let h = anim(ui, resp.id.with("hov"), resp.hovered() && !listening && !error, 0.08);
    let l = ease(anim(ui, resp.id.with("lis"), listening, 0.1));
    let e = ease(anim(ui, resp.id.with("err"), error, 0.12));
    let border = mix(mix(mix(p.line2, p.line3, h), p.acc, l), p.warn, e);
    let painter = ui.painter();
    painter.rect(rect, 5, mix(p.field, p.ctl, h), Stroke::new(1.0 + l.max(e), border), StrokeKind::Inside);
    let y = rect.center().y;
    let right = |text: &str, color: Color32| {
        painter.text(pos2(rect.right() - 10.0, y), Align2::RIGHT_CENTER, text, mono(11.0), color);
    };
    match state {
        HotkeyState::Idle => {
            keycaps(painter, ui, p, rect.left() + 6.0, y, keys, false);
            right("change", mix(p.t3, p.t2, h));
        }
        HotkeyState::Listening { held: [] } => {
            painter.text(pos2(rect.left() + 10.0, y), Align2::LEFT_CENTER, "press a shortcut…", sans(13.0), p.t2);
            right("esc to cancel", p.t3);
        }
        HotkeyState::Listening { held } => {
            let x = keycaps(painter, ui, p, rect.left() + 6.0, y, held, false);
            painter.text(pos2(x, y), Align2::LEFT_CENTER, "+ …", mono(12.0), p.t3);
            right("esc to cancel", p.t3);
        }
        HotkeyState::Error { keys } => {
            keycaps(painter, ui, p, rect.left() + 6.0, y, keys, true);
            right("esc to cancel", p.t3);
        }
    }
    focus_ring(ui, look, &resp, rect, 5.0);
    resp
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
    ui.painter().rect(rect, 6, p.field, Stroke::new(1.0, p.line2), StrokeKind::Inside);
    let inner = rect.shrink(3.0);
    let n = options.len();
    let seg_w = (inner.width() - 2.0 * (n - 1) as f32) / n as f32;
    let mut focused = None;
    let mut cells = Vec::with_capacity(n);
    for (i, (v, _)) in options.iter().enumerate() {
        let r = Rect::from_min_size(pos2(inner.left() + i as f32 * (seg_w + 2.0), inner.top()), vec2(seg_w, 26.0));
        let cell = ui.interact(r, resp.id.with(i), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
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
    for (i, ((_, label), (r, cell))) in options.iter().zip(&cells).enumerate() {
        let near = (1.0 - (pos - i as f32).abs()).clamp(0.0, 1.0);
        let h = anim(ui, cell.id.with("hov"), cell.hovered(), 0.08);
        let font = if i == selected { medium(13.0) } else { sans(13.0) };
        painter.text(r.center(), Align2::CENTER_CENTER, *label, font, mix(p.t2, p.t1, near.max(h)));
    }
    if look.keyboard && focused.is_some() {
        painter.rect_stroke(rect.expand(2.0), 8.0, Stroke::new(2.0, p.focus), StrokeKind::Middle);
    }
    resp
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
    painter.rect_filled(knob, 7, mix(p.t2, Color32::WHITE, t));
    painter.galley(pos2(track.right() + 10.0, rect.center().y - g.size().y / 2.0), g, p.t2);
    focus_ring(ui, look, &resp, track, 10.0);
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
    let p = look.p;
    let (min, max) = (*range.start(), *range.end());
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        let w = ui.available_width();
        let (readout, _) = ui.allocate_exact_size(vec2(w, 18.0), Sense::hover());
        let (track, mut resp) = ui.allocate_exact_size(vec2(w, 20.0), Sense::click_and_drag());
        let (scale, _) = ui.allocate_exact_size(vec2(w, 14.0), Sense::hover());

        let mut v = *value;
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
        if v != *value {
            *value = v;
            resp.mark_changed();
        }

        let painter = ui.painter();
        let y = readout.center().y;
        painter.text(pos2(readout.left(), y), Align2::LEFT_CENTER, value.to_string(), mono_medium(12.0), p.t1);
        painter.text(pos2(readout.left() + 28.0, y), Align2::LEFT_CENTER, word, sans(13.0), p.t2);

        // The thumb glides to clicks, keys and resets, and follows the pointer while dragged.
        let target = (*value - min) as f32 / (max - min) as f32;
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
        resp.on_hover_cursor(CursorIcon::PointingHand)
    })
    .inner
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
    resp
}
