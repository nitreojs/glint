use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, Id, PointerButton, Rect, Response, Role, Sense, Stroke, Ui,
    WidgetInfo, pos2, vec2,
};

use super::controls::focus_ring;
use super::motion::{anim, mix};
use super::tokens::{CLOSE_HOVER, CLOSE_PRESSED, Look, medium, mono, mono_medium, sans, semibold};

pub enum TitleAction {
    None,
    Minimize,
    Close,
}

/// The 40 px bar: mark, name, drag area, minimize and close. `active` is 1 while focused.
pub fn title_bar(ui: &mut Ui, look: &Look, rect: Rect, what: &str, active: f32, radius: u8) -> TitleAction {
    let p = &look.p;
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius { nw: radius, ne: radius, sw: 0, se: 0 }, p.side);

    // The mark: a 14 px disc whose highlight is cut out (side colour) while inactive.
    let disc = Rect::from_min_size(pos2(rect.left() + 18.0, rect.center().y - 7.0), vec2(14.0, 14.0));
    painter.circle_filled(disc.center(), 7.0, mix(p.t4, p.mark, active));
    painter.circle_stroke(disc.center(), 7.0, Stroke::new(1.0, p.mark_ring.gamma_multiply(active)));
    painter.circle_filled(pos2(disc.right() - 4.0, disc.top() + 4.0), 2.0, mix(p.side, Color32::WHITE, active));
    let name = painter.text(
        pos2(disc.right() + 9.0, rect.center().y),
        Align2::LEFT_CENTER,
        "glint",
        semibold(13.0),
        mix(p.t4, p.t1, active),
    );
    painter.text(pos2(name.right() + 9.0, rect.center().y), Align2::LEFT_CENTER, what, sans(13.0), mix(p.t4, p.t3, active));

    // Everything but the buttons moves the window.
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
    min.widget_info(|| WidgetInfo::labeled(Role::Button, true, "minimize window"));
    let (h, d) = (anim(ui, min.id, min.hovered(), 0.08), anim(ui, min.id.with("p"), min.is_pointer_button_down_on(), 0.08));
    let painter = ui.painter();
    painter.rect_filled(min_rect, 0, p.hov.gamma_multiply(h * (1.0 - d)));
    painter.rect_filled(min_rect, 0, p.prs.gamma_multiply(d));
    let c = min_rect.center();
    painter.hline((c.x - 5.0).round()..=(c.x + 5.0).round(), c.y.round() + 0.5, Stroke::new(1.0, fg));

    let close_rect = Rect::from_min_max(pos2(rect.right() - 46.0, rect.top()), rect.right_bottom());
    let close = ui.interact(close_rect, Id::unique("title-close"), Sense::CLICK);
    close.widget_info(|| WidgetInfo::labeled(Role::Button, true, "close window"));
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
    resp.widget_info(|| WidgetInfo::selected(Role::Tab, true, selected, label));
    resp
}
