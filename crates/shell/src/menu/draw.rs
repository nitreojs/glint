//! What a menu panel looks like: the settings window's dark palette, Plex Sans, thin line glyphs.

use std::f32::consts::PI;

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;

use super::layout::{Layout, Metrics};
use super::{Glyph, Item, Kind};
use crate::gfx::{Canvas, rgba};

// the settings window's dark tokens (glint_ui::widgets::tokens)
const SURF: D2D1_COLOR_F = rgba(0x22, 0x22, 0x21, 0.985);
const T1: D2D1_COLOR_F = rgba(0xED, 0xEB, 0xE7, 1.0);
const T2: D2D1_COLOR_F = rgba(0xB0, 0xAD, 0xA7, 1.0);
const T3: D2D1_COLOR_F = rgba(0x8E, 0x8B, 0x85, 1.0);
const T4: D2D1_COLOR_F = rgba(0x5C, 0x5A, 0x56, 1.0);
const BORDER: D2D1_COLOR_F = rgba(255, 255, 255, 0.09);
const LINE: D2D1_COLOR_F = rgba(255, 255, 255, 0.07);
const HIGHLIGHT: D2D1_COLOR_F = rgba(255, 255, 255, 0.075);
const TRACK_OFF: D2D1_COLOR_F = rgba(255, 255, 255, 0.11);

pub(crate) struct Fonts {
    pub label: IDWriteTextFormat,
    pub hint: IDWriteTextFormat,
    pub title: IDWriteTextFormat,
}

/// One panel's frame.
pub(crate) struct Frame<'a> {
    pub m: &'a Metrics,
    pub layout: &'a Layout,
    pub items: &'a [Item],
    /// The header's title, status and title width.
    pub header: Option<(&'a str, &'a str, f32)>,
    /// Each item's label and hint widths; hints are right-aligned.
    pub widths: &'a [(f32, f32)],
    pub accent: [u8; 3],
    /// The highlight's top and opacity, gliding between rows.
    pub highlight: Option<(f32, f32)>,
    /// The row the keyboard or mouse is on.
    pub hot: Option<usize>,
    /// The panel's top-left in the window (it slides in).
    pub at: (f32, f32),
}

fn with_alpha(c: D2D1_COLOR_F, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { a: c.a * a, ..c }
}

fn accent_color([r, g, b]: [u8; 3], toward_black: f32) -> D2D1_COLOR_F {
    let k = 1.0 - toward_black;
    D2D1_COLOR_F { r: r as f32 / 255.0 * k, g: g as f32 / 255.0 * k, b: b as f32 / 255.0 * k, a: 1.0 }
}

pub(crate) fn frame(c: &Canvas, f: &Frame, fonts: &Fonts) {
    let m = f.m;
    let s = m.s;
    let (w, h) = (f.layout.w, f.layout.h);
    c.translate(f.at.0, f.at.1);
    // a soft shadow, a little lower than the panel
    for k in 1..=7 {
        let d = k as f32 * 2.0 * s;
        c.round_rect(-d, -d + 3.0 * s, w + 2.0 * d, h + 2.0 * d, m.radius + d, rgba(0, 0, 0, 0.032));
    }
    c.round_rect(0.0, 0.0, w, h, m.radius, SURF);
    let half = 0.5 * s.max(1.0);
    c.round_rect_outline([half, half, w - 2.0 * half, h - 2.0 * half], m.radius - half, s.max(1.0), BORDER);

    let mark = accent_color(f.accent, 0.0);
    if let Some((title, status, title_w)) = f.header {
        let top = f.layout.top;
        let x = f.layout.text_x;
        let centre = (m.glyph_x + m.glyph / 2.0, (m.pad + top) / 2.0);
        c.circle(centre, 5.0 * s, mark);
        c.circle((centre.0 + 1.6 * s, centre.1 - 1.6 * s), 1.5 * s, rgba(255, 255, 255, 0.8));
        let band = top - m.pad;
        c.text(title, &fonts.title, [x, m.pad, w - x - m.right, band], T1);
        let sx = x + title_w + 8.0 * s;
        c.text(status, &fonts.hint, [sx, m.pad, w - sx - m.right, band], T3);
        let y = top.floor() + half;
        c.line((0.0, y), (w, y), s.max(1.0), LINE);
    }

    if let Some((y, a)) = f.highlight {
        let inset = m.inset;
        c.round_rect(inset, y, w - 2.0 * inset, m.row, 6.0 * s, with_alpha(HIGHLIGHT, a));
    }

    let on = accent_color(f.accent, 0.09);
    for (i, (item, row)) in f.items.iter().zip(&f.layout.rows).enumerate() {
        if matches!(item.kind, Kind::Separator) {
            let y = (row.y + row.h / 2.0).floor() + half;
            c.line((m.inset * 2.0, y), (w - m.inset * 2.0, y), s.max(1.0), LINE);
            continue;
        }
        let hot = f.hot == Some(i);
        let (label, muted, glyph) = match (item.enabled, hot) {
            (false, _) => (T4, T4, T4),
            (true, true) => (T1, T3, T2),
            (true, false) => (T1, T4, T3),
        };
        if let Some(g) = item.glyph {
            let color = if item.accent && item.enabled { mark } else { glyph };
            let dot = if item.enabled { mark } else { T4 };
            draw_glyph(c, g, (m.glyph_x, row.y + (row.h - m.glyph) / 2.0), m.glyph, color, dot, s);
        }
        let x = f.layout.text_x;
        c.text(&item.label, &fonts.label, [x, row.y, w - x, row.h], label);
        let right = w - m.right;
        let mid = row.y + row.h / 2.0;
        let hint_w = f.widths.get(i).map_or(0.0, |w| w.1);
        let hint = |end: f32| c.text(&item.hint, &fonts.hint, [end - hint_w, row.y, hint_w + 1.0, row.h], muted);
        match (&item.kind, item.check) {
            (Kind::Sub(_), _) => {
                let (cx, r) = (right - m.chevron / 2.0 - 1.5 * s, 3.5 * s);
                c.polyline(&[(cx - r / 2.0, mid - r), (cx + r / 2.0, mid), (cx - r / 2.0, mid + r)], 1.4 * s, muted);
                if !item.hint.is_empty() {
                    hint(right - 2.0 * m.chevron);
                }
            }
            (_, Some(checked)) => {
                let (sw, sh) = m.switch;
                let (x0, y0) = (right - sw, mid - sh / 2.0);
                let track = match (checked, item.enabled) {
                    (true, true) => on,
                    (true, false) => T4,
                    (false, _) => TRACK_OFF,
                };
                c.round_rect(x0, y0, sw, sh, sh / 2.0, track);
                let r = sh / 2.0 - 2.0 * s;
                let kx = if checked { x0 + sw - sh / 2.0 } else { x0 + sh / 2.0 };
                c.circle((kx, mid), r, if checked { rgba(255, 255, 255, 1.0) } else { T2 });
            }
            _ if !item.hint.is_empty() => hint(right),
            _ => {}
        }
    }
}

/// Points along a circle round `(cx, cy)` from angle `a0` to `a1` (radians, clockwise on screen).
fn arc((cx, cy): (f32, f32), r: f32, a0: f32, a1: f32) -> Vec<(f32, f32)> {
    let n = (((a1 - a0).abs() / (PI / 16.0)).ceil() as usize).max(2);
    (0..=n)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / n as f32;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

/// An arrowhead at the end of `path`, pointing the way it runs.
fn head(path: &[(f32, f32)], len: f32) -> [(f32, f32); 3] {
    let (ex, ey) = path[path.len() - 1];
    let (px, py) = path[path.len() - 2];
    let (dx, dy) = (ex - px, ey - py);
    let l = (dx * dx + dy * dy).sqrt().max(1e-3);
    let (bx, by) = (-dx / l * len, -dy / l * len);
    let rot = |a: f32| (ex + bx * a.cos() - by * a.sin(), ey + bx * a.sin() + by * a.cos());
    [rot(0.75), (ex, ey), rot(-0.75)]
}

/// `g` in a `size`-pixel box at `(x, y)`, drawn on a 16-unit grid.
fn draw_glyph(c: &Canvas, g: Glyph, (x, y): (f32, f32), size: f32, color: D2D1_COLOR_F, dot: D2D1_COLOR_F, s: f32) {
    let k = size / 16.0;
    let p = |u: f32, v: f32| (x + u * k, y + v * k);
    let pts = |list: &[(f32, f32)]| list.iter().map(|&(u, v)| p(u, v)).collect::<Vec<_>>();
    let stroke = 1.35 * s;
    let line = |list: &[(f32, f32)]| c.polyline(&pts(list), stroke, color);
    let frame = |[u0, v0, u1, v1]: [f32; 4]| {
        let (a, b) = (p(u0, v0), p(u1, v1));
        c.round_rect_outline([a.0, a.1, b.0 - a.0, b.1 - a.1], 1.8 * k, stroke, color);
    };
    let ring = |u: f32, v: f32, r: f32| c.ring(p(u, v), r * k, stroke, color);
    let curve = |u: f32, v: f32, r: f32, a0: f32, a1: f32| arc(p(u, v), r * k, a0, a1);
    match g {
        Glyph::Record => c.circle(p(8.0, 8.0), 4.5 * k, color),
        Glyph::Stop => {
            let (a, w) = (p(4.0, 4.0), 8.0 * k);
            c.round_rect(a.0, a.1, w, w, 1.8 * k, color);
        }
        Glyph::Pause => {
            line(&[(5.5, 3.5), (5.5, 12.5)]);
            line(&[(10.5, 3.5), (10.5, 12.5)]);
        }
        Glyph::Play => line(&[(5.0, 3.5), (12.5, 8.0), (5.0, 12.5), (5.0, 3.5)]),
        Glyph::Region => {
            line(&[(1.5, 5.5), (1.5, 1.5), (5.5, 1.5)]);
            line(&[(10.5, 1.5), (14.5, 1.5), (14.5, 5.5)]);
            line(&[(14.5, 10.5), (14.5, 14.5), (10.5, 14.5)]);
            line(&[(5.5, 14.5), (1.5, 14.5), (1.5, 10.5)]);
        }
        Glyph::Screens => {
            frame([5.0, 6.0, 14.5, 13.0]);
            line(&[(3.0, 9.5), (1.5, 9.5), (1.5, 2.5), (11.0, 2.5), (11.0, 4.0)]);
        }
        Glyph::Monitor => {
            frame([1.5, 2.5, 14.5, 11.0]);
            line(&[(8.0, 11.0), (8.0, 13.5)]);
            line(&[(5.0, 13.5), (11.0, 13.5)]);
        }
        Glyph::Replay | Glyph::Update => {
            let path = if g == Glyph::Replay {
                curve(8.0, 8.0, 6.0, -0.1 * PI, -1.55 * PI)
            } else {
                curve(8.0, 8.0, 6.0, -0.55 * PI, 0.9 * PI)
            };
            c.polyline(&path, stroke, color);
            c.polyline(&head(&path, 3.2 * k), stroke, color);
            if g == Glyph::Replay {
                c.circle(p(8.0, 8.0), 1.7 * k, dot);
            }
        }
        Glyph::Trim => {
            ring(4.5, 11.5, 2.2);
            ring(11.5, 11.5, 2.2);
            line(&[(5.9, 9.7), (11.5, 1.5)]);
            line(&[(10.1, 9.7), (4.5, 1.5)]);
        }
        Glyph::Recent => {
            ring(8.0, 8.0, 6.3);
            line(&[(8.0, 4.5), (8.0, 8.0), (10.5, 9.8)]);
        }
        Glyph::Folder => line(&[(1.5, 12.5), (1.5, 3.0), (6.0, 3.0), (7.5, 5.0), (14.5, 5.0), (14.5, 12.5), (1.5, 12.5)]),
        Glyph::Settings => {
            line(&[(1.5, 5.0), (7.5, 5.0)]);
            line(&[(12.5, 5.0), (14.5, 5.0)]);
            ring(10.0, 5.0, 2.2);
            line(&[(1.5, 11.0), (3.5, 11.0)]);
            line(&[(8.5, 11.0), (14.5, 11.0)]);
            ring(6.0, 11.0, 2.2);
        }
        Glyph::Log => {
            frame([3.0, 1.5, 13.0, 14.5]);
            line(&[(5.5, 5.5), (10.5, 5.5)]);
            line(&[(5.5, 8.0), (10.5, 8.0)]);
            line(&[(5.5, 10.5), (8.5, 10.5)]);
        }
        Glyph::Quit => {
            c.polyline(&curve(8.0, 8.5, 5.8, -0.3 * PI, 1.3 * PI), stroke, color);
            line(&[(8.0, 1.5), (8.0, 7.5)]);
        }
        Glyph::Video => {
            frame([1.5, 3.0, 14.5, 13.0]);
            c.polygon(&pts(&[(6.5, 5.6), (10.6, 8.0), (6.5, 10.4)]), color);
        }
        Glyph::Image => {
            frame([1.5, 2.5, 14.5, 13.5]);
            line(&[(2.0, 12.5), (6.0, 8.5), (9.0, 11.0), (11.0, 9.5), (14.0, 12.0)]);
            c.circle(p(10.8, 6.0), 1.2 * k, color);
        }
        Glyph::Open => {
            line(&[(7.0, 2.5), (2.5, 2.5), (2.5, 13.5), (13.5, 13.5), (13.5, 9.0)]);
            line(&[(9.5, 2.5), (13.5, 2.5), (13.5, 6.5)]);
            line(&[(13.5, 2.5), (7.5, 8.5)]);
        }
        Glyph::Copy => {
            frame([5.0, 5.0, 14.5, 14.5]);
            line(&[(1.5, 10.5), (1.5, 1.5), (10.5, 1.5)]);
        }
        Glyph::Upload => {
            line(&[(8.0, 11.0), (8.0, 2.5)]);
            line(&[(4.5, 6.0), (8.0, 2.5), (11.5, 6.0)]);
            line(&[(2.5, 14.0), (13.5, 14.0)]);
        }
        Glyph::Clear => {
            line(&[(4.0, 4.0), (12.0, 12.0)]);
            line(&[(12.0, 4.0), (4.0, 12.0)]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arcs_run_from_start_to_end_and_heads_point_along() {
        let a = arc((0.0, 0.0), 10.0, 0.0, PI / 2.0);
        let (first, last) = (a[0], a[a.len() - 1]);
        assert!((first.0 - 10.0).abs() < 1e-4 && first.1.abs() < 1e-4);
        assert!(last.0.abs() < 1e-4 && (last.1 - 10.0).abs() < 1e-4, "clockwise on screen: ends below");
        let [l, tip, r] = head(&[(0.0, 0.0), (10.0, 0.0)], 3.0);
        assert_eq!(tip, (10.0, 0.0));
        assert!(l.0 < 10.0 && r.0 < 10.0 && (l.1 + r.1).abs() < 1e-4, "both barbs trail behind the tip");
    }
}
