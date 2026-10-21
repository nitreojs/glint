use std::f32::consts::TAU;

use eframe::egui::{
    Color32, CursorIcon, EventFilter, Key, Mesh, Painter, Pos2, Rect, Response, Role, Sense, Shape, Stroke, StrokeKind,
    Ui, Vec2, WidgetInfo, pos2, vec2,
};

use super::controls::focus_ring;
use super::motion::{anim, ease};
use super::tokens::Look;
use crate::config::color::hsv_to_rgb;

fn color(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

fn grid(painter: &Painter, rect: Rect, cells: [u32; 2], paint: impl Fn(f32, f32) -> Color32) {
    let [nx, ny] = cells;
    let mut mesh = Mesh::default();
    for j in 0..=ny {
        for i in 0..=nx {
            let (u, v) = (i as f32 / nx as f32, j as f32 / ny as f32);
            mesh.colored_vertex(pos2(rect.left() + u * rect.width(), rect.top() + v * rect.height()), paint(u, v));
        }
    }
    for j in 0..ny {
        for i in 0..nx {
            let a = j * (nx + 1) + i;
            mesh.add_triangle(a, a + 1, a + nx + 1);
            mesh.add_triangle(a + 1, a + nx + 2, a + nx + 1);
        }
    }
    painter.add(Shape::mesh(mesh));
}

fn ring(painter: &Painter, c: Pos2, inner: f32, outer: f32, centre: Option<Color32>, paint: impl Fn(f32) -> Color32) {
    const N: u32 = 48;
    let mut mesh = Mesh::default();
    for k in 0..=N {
        let t = k as f32 / N as f32;
        let dir = Vec2::angled(t * TAU - TAU / 4.0);
        mesh.colored_vertex(c + dir * inner, centre.unwrap_or_else(|| paint(t)));
        mesh.colored_vertex(c + dir * outer, paint(t));
    }
    for k in 0..N {
        let a = 2 * k;
        mesh.add_triangle(a, a + 1, a + 2);
        mesh.add_triangle(a + 1, a + 3, a + 2);
    }
    painter.add(Shape::mesh(mesh));
}

fn arrows(ui: &Ui, resp: &Response) -> Vec2 {
    let filter = EventFilter { horizontal_arrows: true, vertical_arrows: true, ..Default::default() };
    ui.memory_mut(|m| m.set_focus_lock_filter(resp.id, filter));
    ui.input(|i| {
        let k = |key| i.key_pressed(key) as i32 as f32;
        vec2(k(Key::ArrowRight) - k(Key::ArrowLeft), k(Key::ArrowUp) - k(Key::ArrowDown))
    })
}

fn pressed_at(resp: &Response) -> Option<Pos2> {
    resp.interact_pointer_pos().filter(|_| resp.is_pointer_button_down_on() || resp.clicked())
}

pub(crate) fn sv_square(ui: &mut Ui, look: &Look, hsv: &mut [f32; 3], size: Vec2) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let before = *hsv;
    if let Some(pos) = pressed_at(&resp) {
        hsv[1] = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        hsv[2] = 1.0 - ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
    }
    if resp.has_focus() {
        let step = arrows(ui, &resp) * 0.02;
        hsv[1] = (hsv[1] + step.x).clamp(0.0, 1.0);
        hsv[2] = (hsv[2] + step.y).clamp(0.0, 1.0);
    }
    if *hsv != before {
        resp.mark_changed();
    }
    let painter = ui.painter();
    let hue = hsv[0];
    grid(painter, rect, [16, 16], |u, v| color(hsv_to_rgb([hue, u, 1.0 - v])));
    painter.rect_stroke(rect, 0, Stroke::new(1.0, look.p.line2), StrokeKind::Outside);
    let c = pos2(rect.left() + hsv[1] * rect.width(), rect.top() + (1.0 - hsv[2]) * rect.height());
    let r = 6.0 + ease(anim(ui, resp.id.with("grow"), resp.hovered() || resp.dragged(), 0.08));
    painter.circle_filled(c, r, color(hsv_to_rgb(*hsv)));
    painter.circle_stroke(c, r, Stroke::new(2.0, Color32::WHITE));
    painter.circle_stroke(c, r + 1.5, Stroke::new(1.0, Color32::from_black_alpha(110)));
    focus_ring(ui, look, &resp, rect, 0.0);
    let [s, v] = [hsv[1], hsv[2]].map(|c| (c * 100.0).round());
    resp.widget_info(|| WidgetInfo {
        current_text_value: Some(format!("saturation {s} %, brightness {v} %")),
        ..WidgetInfo::labeled(Role::Slider, true, "saturation and brightness")
    });
    resp.on_hover_cursor(CursorIcon::Crosshair)
}

pub(crate) fn hue_strip(ui: &mut Ui, look: &Look, hsv: &mut [f32; 3], size: Vec2) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let before = hsv[0];
    if let Some(pos) = pressed_at(&resp) {
        hsv[0] = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
    }
    if resp.has_focus() {
        let step = arrows(ui, &resp);
        hsv[0] = (hsv[0] + (step.x + step.y) / 360.0 * 2.0).clamp(0.0, 1.0);
    }
    if hsv[0] != before {
        resp.mark_changed();
    }
    let painter = ui.painter();
    grid(painter, rect, [36, 1], |u, _| color(hsv_to_rgb([u, 1.0, 1.0])));
    painter.rect_stroke(rect, 0, Stroke::new(1.0, look.p.line2), StrokeKind::Outside);
    let grow = ease(anim(ui, resp.id.with("grow"), resp.hovered() || resp.dragged(), 0.08));
    let x = rect.left() + hsv[0] * rect.width();
    let handle = Rect::from_center_size(pos2(x, rect.center().y), vec2(8.0 + grow, rect.height() + 8.0 + grow));
    painter.rect(handle, 3, color(hsv_to_rgb([hsv[0], 1.0, 1.0])), Stroke::new(2.0, Color32::WHITE), StrokeKind::Inside);
    painter.rect_stroke(handle, 3, Stroke::new(1.0, Color32::from_black_alpha(110)), StrokeKind::Outside);
    focus_ring(ui, look, &resp, rect, 0.0);
    resp.widget_info(|| WidgetInfo::slider(true, (hsv[0] * 360.0).round() as f64, "hue"));
    resp.on_hover_cursor(CursorIcon::ResizeHorizontal)
}

pub(crate) fn custom_swatch(ui: &mut Ui, look: &Look, custom: Option<Color32>, selected: bool) -> Response {
    let p = &look.p;
    let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
    let h = anim(ui, resp.id.with("hov"), resp.hovered(), 0.08);
    let s = ease(anim(ui, resp.id.with("sel"), selected, 0.12));
    let painter = ui.painter();
    let c = rect.center();
    let r = 9.0 + h - 2.0 * s;
    let hue = |t: f32| color(hsv_to_rgb([t, 0.85, 0.95]));
    match custom {
        Some(fill) => {
            ring(painter, c, r - 2.5, r, None, hue);
            painter.circle_filled(c, r - 3.5, fill);
        }
        None => ring(painter, c, 0.0, r, Some(Color32::WHITE), hue),
    }
    painter.circle_stroke(c, 11.0, Stroke::new(1.5, p.t1.gamma_multiply(s)));
    painter.circle_stroke(c, r, Stroke::new(1.0, p.line2));
    focus_ring(ui, look, &resp, rect, 12.0);
    resp.widget_info(|| WidgetInfo {
        current_text_value: custom.map(|c| format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())),
        ..WidgetInfo::selected(Role::RadioButton, true, selected, "custom colour")
    });
    resp.on_hover_cursor(CursorIcon::PointingHand)
}
