use glint_core::config::UiElements;
use glint_core::geom::monitor::Output as Monitor;
use glint_core::geom::rect::Rect;
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Gdi::{BitBlt, SRCCOPY};

use super::{Purpose, RING};
use glint_shell::gfx::{Canvas, Dib, Painter, rgba};

const PILL_BG: D2D1_COLOR_F = rgba(16, 16, 20, 0.86);
const PILL_TEXT: D2D1_COLOR_F = rgba(255, 255, 255, 0.96);

/// A pre-rendered pill (label or hint) and where it sits, in client coordinates.
pub(super) struct Pill {
    pub(super) rect: Rect,
    pub(super) dib: Option<Dib>,
    pub(super) text: String,
}

/// Renders a pill with `text` over a copy of `bg` at (x, y).
pub(super) fn render_pill(painter: &Painter, font: &IDWriteTextFormat, text: &str, bg: &Dib, x: i32, y: i32) -> Result<Pill, String> {
    let (w, h) = pill_size(painter, font, text);
    let dib = Dib::new(w, h)?;
    unsafe {
        let _ = BitBlt(dib.dc, 0, 0, dib.w, dib.h, Some(bg.dc), x, y, SRCCOPY);
    }
    painter.draw(&dib, |c| draw_pill(c, painter, font, text, w as f32, h as f32))?;
    Ok(Pill { rect: Rect { x, y, w, h }, dib: Some(dib), text: text.to_string() })
}

pub(super) fn draw_pill(c: &Canvas, painter: &Painter, font: &IDWriteTextFormat, text: &str, w: f32, h: f32) {
    let pad = painter.px(11.0);
    c.round_rect(0.0, 0.0, w, h, h / 2.0, PILL_BG);
    c.text(text, font, [pad, 0.0, w - pad * 2.0 + 2.0, h], PILL_TEXT);
}

pub(super) fn pill_size(painter: &Painter, font: &IDWriteTextFormat, text: &str) -> (i32, i32) {
    let (tw, _) = painter.measure(text, font);
    ((tw.ceil() + painter.px(11.0) * 2.0) as i32, painter.px(26.0) as i32)
}

/// Where a `size` label for the highlight `near` goes on `mon`: just above it, else inside its
/// top-left corner (always inside with `inside_only`). Never under the toolbar `bar`: a label it
/// would cover moves inside, and below the bar when the bar covers the top of the highlight too.
pub(super) fn label_spot(near: Rect, mon: Rect, (w, h): (i32, i32), gap: i32, bar: Option<Rect>, inside_only: bool) -> (i32, i32) {
    let fit = |x: i32, y: i32| Rect { x: x.clamp(mon.x, (mon.right() - w).max(mon.x)), y: y.min((mon.bottom() - h).max(mon.y)), w, h };
    let clear = |r: Rect| bar.is_none_or(|b| r.intersect(&b).is_none());
    let part = near.intersect(&mon).unwrap_or(mon);
    let above = fit(near.x - RING, near.y - RING - gap - h);
    let inside = fit(part.x + gap + 2, part.y + gap + 2);
    let spot = match bar {
        _ if part.y == near.y && above.y >= mon.y && !inside_only && clear(above) => above,
        Some(b) if !clear(inside) => fit(inside.x, b.bottom() + gap),
        _ => inside,
    };
    (spot.x, spot.y)
}

/// The hint for `purpose`; Ctrl+A is mentioned only with more than one monitor, UI elements only
/// when they're detected.
pub(super) fn hint_text(purpose: Purpose, monitors: usize, elements: UiElements) -> String {
    let verb = match purpose {
        Purpose::Record { .. } => "record",
        Purpose::Shot { .. } => "capture",
    };
    let wheel = match elements {
        UiElements::Shift => "   ·   Shift for parts of apps",
        UiElements::Always => "   ·   Scroll for bigger or smaller parts",
        UiElements::Off => "",
    };
    let all = if monitors > 1 { "   ·   Ctrl+A for all screens" } else { "" };
    format!(
        "Drag to select a region (Ctrl moves it, Alt skips snapping)   ·   Click to {verb} what's highlighted{wheel}   ·   Ctrl+click to add more{all}   ·   Esc to cancel"
    )
}

pub(super) fn hint_rects(painter: &Painter, font: &IDWriteTextFormat, hint: &str, outputs: &[Monitor], origin: (i32, i32)) -> Vec<Rect> {
    let (w, h) = pill_size(painter, font, hint);
    let top = painter.px(28.0) as i32;
    outputs
        .iter()
        .map(|o| {
            let r = o.rect.offset(-origin.0, -origin.1);
            Rect { x: r.x + (r.w - w) / 2, y: r.y + top, w, h }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MON: Rect = Rect { x: 0, y: 0, w: 1920, h: 1080 };
    const SIZE: (i32, i32) = (100, 26);
    /// Docked at the top centre, as in draw-first.
    const BAR: Rect = Rect { x: 600, y: 10, w: 720, h: 40 };

    #[test]
    fn labels_go_above_then_inside_and_never_under_the_toolbar() {
        let spot = |near: Rect, bar| label_spot(near, MON, SIZE, 6, bar, false);
        let low = Rect { x: 700, y: 300, w: 400, h: 300 };
        assert_eq!(spot(low, Some(BAR)), (698, 266), "above, clear of the bar");
        let top = Rect { x: 0, y: 0, w: 400, h: 300 };
        assert_eq!(spot(top, None), (8, 8), "inside when there's no room above");
        let under_bar = Rect { x: 700, y: 60, w: 400, h: 300 };
        assert_eq!(spot(under_bar, None), (698, 26), "above with no bar");
        assert_eq!(spot(under_bar, Some(BAR)), (708, 68), "inside when the bar is above it");
        let across_bar = Rect { x: 700, y: 20, w: 400, h: 300 };
        assert_eq!(spot(across_bar, Some(BAR)), (708, 56), "below the bar when it covers the top");
        let beside_bar = Rect { x: 100, y: 20, w: 400, h: 300 };
        assert_eq!(spot(beside_bar, Some(BAR)), (108, 28), "inside, where the bar isn't");
        assert_eq!(label_spot(low, MON, SIZE, 6, Some(BAR), true), (708, 308), "inside only");
    }
}
