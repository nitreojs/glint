//! Custom colours: the palette's last swatch opens a picker (a saturation / brightness square, a
//! hue strip and a hex field) under the toolbar. What it picks colours the picked mark and what's
//! drawn next; the last few picked stay on the palette as extra swatches, in this shot and the next.

use std::cell::RefCell;
use std::sync::Mutex;
use std::time::Instant;

use glint_core::config::color::{hsv_to_rgb, rgb_to_hsv, rgb_to_hsv_near};
use glint_core::geom::rect::Rect;
use glint_shell::gfx::{Canvas, rgba};

use super::marks::{Ink, Mark, PALETTE};
use super::text::Style;
use super::textbar::{PANEL_MS, faded, progress};
use super::toolbar::{BAR_BG, Item, WHITE};
use super::{Drag, Editor, Event};

/// Custom colours kept on the palette.
pub(super) const CUSTOMS: usize = 3;

/// The custom colours picked lately, newest first.
static RECENT: Mutex<Vec<Ink>> = Mutex::new(Vec::new());

pub(super) fn recent_customs() -> Vec<Ink> {
    RECENT.lock().map(|r| r.clone()).unwrap_or_default()
}

/// `ink` joins the front of `list` (once, and not when it's on the palette), which keeps `CUSTOMS`.
pub(super) fn remembered(list: &[Ink], ink: Ink) -> Vec<Ink> {
    if PALETTE.contains(&ink) {
        return list.to_vec();
    }
    let mut out = vec![ink];
    out.extend(list.iter().copied().filter(|c| *c != ink));
    out.truncate(CUSTOMS);
    out
}

/// `#RRGGBB`.
pub(super) fn hex(ink: Ink) -> String {
    let [r, g, b] = ink.0;
    format!("#{r:02X}{g:02X}{b:02X}")
}

/// Six hex digits (a leading `#` or spaces allowed) as a colour.
pub(super) fn parse_hex(s: &str) -> Option<Ink> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
    Some(Ink([byte(0)?, byte(2)?, byte(4)?]))
}

/// Saturation and brightness at `p` in the square `r`: saturation grows to the right, brightness
/// upwards; outside, the nearest edge.
pub(super) fn sv_at(r: Rect, (x, y): (i32, i32)) -> (f32, f32) {
    let s = (x - r.x) as f32 / (r.w - 1).max(1) as f32;
    let v = 1.0 - (y - r.y) as f32 / (r.h - 1).max(1) as f32;
    (s.clamp(0.0, 1.0), v.clamp(0.0, 1.0))
}

/// The hue (0 to 1) at `x` along the strip `r`.
pub(super) fn hue_at(r: Rect, x: i32) -> f32 {
    ((x - r.x) as f32 / (r.w - 1).max(1) as f32).clamp(0.0, 1.0)
}

/// The square's pixels for `hue`: `w`×`h`, white top left, the pure hue top right, black below.
pub(super) fn sv_pixels(hue: f32, w: u32, h: u32) -> Vec<u32> {
    let mut px = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        let v = 1.0 - y as f32 / (h - 1).max(1) as f32;
        for x in 0..w {
            let s = x as f32 / (w - 1).max(1) as f32;
            let [r, g, b] = hsv_to_rgb([hue, s, v]);
            px.push(u32::from_be_bytes([0xFF, r, g, b]));
        }
    }
    px
}

/// The strip's pixels: every hue, left to right, `w`×`h`.
fn hue_pixels(w: u32, h: u32) -> Vec<u32> {
    let row: Vec<u32> = (0..w)
        .map(|x| {
            let [r, g, b] = hsv_to_rgb([x as f32 / (w - 1).max(1) as f32, 1.0, 1.0]);
            u32::from_be_bytes([0xFF, r, g, b])
        })
        .collect();
    (0..h).flat_map(|_| row.iter().copied()).collect()
}

/// Which part of the picker a press is dragging.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Part {
    Square,
    Strip,
}

/// The open picker: the colour as hue / saturation / brightness (kept apart, so a grey keeps its
/// hue), the hex digits being typed, and the square drawn for the current hue.
pub(super) struct Picker {
    pub(super) hsv: [f32; 3],
    pub(super) hex: String,
    /// Typing goes to the hex field even while a text is being typed.
    pub(super) hex_focus: bool,
    pub(super) since: Instant,
    square: RefCell<Option<Square>>,
}

/// The square's pixels for a hue (its bits) at a size.
type Square = (u32, (u32, u32), Vec<u32>);

/// The picker's parts (client coordinates).
pub(super) struct Layout {
    pub(super) rect: Rect,
    pub(super) square: Rect,
    pub(super) strip: Rect,
    pub(super) swatch: Rect,
    pub(super) hex: Rect,
}

impl Editor {
    /// The picker under the palette's picker swatch, beyond the bar and its panel, while it's open.
    pub(super) fn picker_layout(&self) -> Option<Layout> {
        self.picker.as_ref()?;
        let anchor = self.cells.iter().find(|(i, _)| *i == Item::Picker).map(|(_, r)| *r)?;
        let (pad, gap) = (self.px(10.0) as i32, self.px(8.0) as i32);
        let (sw, sh, strip_h, row) = (self.px(210.0) as i32, self.px(140.0) as i32, self.px(14.0) as i32, self.px(26.0) as i32);
        let (w, h) = (sw + 2 * pad, 2 * pad + sh + strip_h + row + 2 * gap);
        let (m, margin) = (self.bar_monitor, self.px(6.0) as i32);
        let panel = self.panel_layout().map(|(_, r, _)| r);
        let top = panel.map_or(self.bar.y, |p| p.y.min(self.bar.y));
        let bottom = panel.map_or(self.bar.bottom(), |p| p.bottom().max(self.bar.bottom()));
        let below = self.below_bar();
        let mut y = if below { bottom + margin } else { top - margin - h };
        if y < m.y || y + h > m.bottom() {
            y = if below { top - margin - h } else { bottom + margin };
        }
        let x = (anchor.x + anchor.w / 2 - w / 2).clamp(m.x, (m.right() - w).max(m.x));
        let square = Rect { x: x + pad, y: y + pad, w: sw, h: sh };
        let strip = Rect { x: square.x, y: square.bottom() + gap, w: sw, h: strip_h };
        let swatch = Rect { x: square.x, y: strip.bottom() + gap, w: row, h: row };
        let hex = Rect { x: swatch.right() + gap, y: swatch.y, w: sw - row - gap, h: row };
        Some(Layout { rect: Rect { x, y, w, h }, square, strip, swatch, hex })
    }

    /// The palette's picker swatch: opens the picker on the colour shown, or closes it.
    pub(super) fn toggle_picker(&mut self) -> Event {
        if self.picker.is_some() {
            self.close_picker();
        } else {
            let hsv = rgb_to_hsv(self.shown_ink().0);
            self.picker = Some(Picker { hsv, hex: String::new(), hex_focus: false, since: Instant::now(), square: RefCell::default() });
        }
        Event::Repaint
    }

    /// Closes the picker; a colour it picked joins the custom swatches.
    pub(super) fn close_picker(&mut self) {
        if self.picker.take().is_none() {
            return;
        }
        let customs = remembered(&self.customs, self.color);
        if customs != self.customs {
            if let Ok(mut recent) = RECENT.lock() {
                *recent = customs.clone();
            }
            self.customs = customs;
            self.place_bar();
        }
    }

    /// The colour from now on, and the picked mark's (one undo step when it changes).
    pub(super) fn set_ink(&mut self, ink: Ink) -> Event {
        if let Some(i) = self.picked
            && self.marks[i].color().is_some_and(|old| old != ink)
        {
            let was = self.marks[i].clone();
            if self.typing_into(i) {
                self.typing_checkpoint(i, was);
            } else {
                self.remember();
            }
        }
        self.tint(ink);
        if let Some(p) = self.picker.as_mut() {
            p.hsv = rgb_to_hsv_near(ink.0, p.hsv);
        }
        Event::Repaint
    }

    /// The colour from now on, and the picked mark's, without an undo step (a drag in the picker
    /// makes one when it ends).
    fn tint(&mut self, ink: Ink) {
        self.color = ink;
        let Some(i) = self.picked else { return };
        if self.marks[i].color().is_none_or(|old| old == ink) {
            return;
        }
        let before = self.ink();
        if let Some(slot) = self.marks[i].color_mut() {
            *slot = ink;
        }
        self.repaint_ink(before);
        self.repaint_ink(vec![self.marks[i].ink(self.scale)]);
        self.marks_rev += 1;
    }

    /// A press on the picker's square or strip at `p`: the colour follows the mouse from here.
    pub(super) fn grab_picker(&mut self, part: Part, p: (i32, i32)) -> Drag {
        let before = self.picked.filter(|&i| self.marks[i].color().is_some()).map(|i| (i, self.marks[i].clone()));
        self.drag_picker(part, p);
        Drag::Color { part, before }
    }

    /// The picker's square or strip dragged to `p`.
    pub(super) fn drag_picker(&mut self, part: Part, p: (i32, i32)) -> Event {
        let Some(l) = self.picker_layout() else { return Event::None };
        let Some(picker) = self.picker.as_mut() else { return Event::None };
        match part {
            Part::Square => (picker.hsv[1], picker.hsv[2]) = sv_at(l.square, p),
            Part::Strip => picker.hsv[0] = hue_at(l.strip, p.0),
        }
        picker.hex.clear();
        let ink = Ink(hsv_to_rgb(picker.hsv));
        self.tint(ink);
        Event::Repaint
    }

    /// A drag in the picker ended: the picked mark's colour change, if any, is one undo step.
    pub(super) fn drop_picker(&mut self, before: Option<(usize, Mark)>) -> Event {
        if let Some((i, was)) = before
            && self.marks.get(i).is_some_and(|m| *m != was)
        {
            if self.typing_into(i) {
                self.typing_checkpoint(i, was);
            } else {
                let mut snapshot = self.marks.clone();
                snapshot[i] = was;
                self.record((snapshot, Some(i)));
            }
        }
        Event::Repaint
    }

    /// Whether typing goes to the hex field: the picker is open and no text is being typed (or the
    /// field was clicked).
    pub(super) fn hex_typing(&self) -> bool {
        self.picker.as_ref().is_some_and(|p| p.hex_focus || self.typing.is_none())
    }

    /// A character typed into the hex field; six digits make the colour.
    pub(super) fn hex_char(&mut self, c: char) -> Event {
        let Some(picker) = self.picker.as_mut() else { return Event::None };
        if !c.is_ascii_hexdigit() || picker.hex.len() >= 6 {
            return Event::None;
        }
        picker.hex.push(c.to_ascii_uppercase());
        match parse_hex(&picker.hex) {
            Some(ink) => self.set_ink(ink),
            None => Event::Repaint,
        }
    }

    /// Keys while the hex field takes typing: Backspace edits it, Enter closes the picker.
    pub(super) fn hex_key(&mut self, vk: u32) -> Option<Event> {
        let picker = self.picker.as_mut()?;
        match vk {
            0x08 => Some(if picker.hex.pop().is_some() { Event::Repaint } else { Event::None }),
            0x0D => {
                self.close_picker();
                Some(Event::Repaint)
            }
            _ => None,
        }
    }

    /// The colour the toolbar shows: the picked mark's, or what's drawn next.
    pub(super) fn shown_ink(&self) -> Ink {
        self.picked_mark().and_then(Mark::color).unwrap_or(self.color)
    }

    /// The picker, drawn onto `c` (which shows its box).
    pub(super) fn draw_picker(&self, c: &Canvas, l: &Layout) {
        let Some(picker) = &self.picker else { return };
        let s = self.scale;
        let a = progress(picker.since, PANEL_MS);
        let o = (l.rect.x, l.rect.y);
        let slide = (1.0 - a) * self.px(6.0) * if l.rect.y >= self.bar.y { -1.0 } else { 1.0 };
        let local = |r: Rect| [(r.x - o.0) as f32, (r.y - o.1) as f32 + slide, r.w as f32, r.h as f32];
        c.round_rect(0.0, slide, l.rect.w as f32, l.rect.h as f32, 10.0 * s, faded(BAR_BG, a));
        let [hue, sat, val] = picker.hsv;
        let (w, h) = (l.square.w as u32, l.square.h as u32);
        let mut square = picker.square.borrow_mut();
        if square.as_ref().is_none_or(|(k, size, _)| *k != hue.to_bits() || *size != (w, h)) {
            *square = Some((hue.to_bits(), (w, h), sv_pixels(hue, w, h)));
        }
        if let Some((_, _, px)) = square.as_ref() {
            c.image_faded(px, w as usize, (w, h), local(l.square), a);
        }
        let (sw, sh) = (l.strip.w as u32, l.strip.h as u32);
        c.image_faded(&hue_pixels(sw, sh), sw as usize, (sw, sh), local(l.strip), a);
        // the colour's spot on the square, and the hue's on the strip
        let [x, y, qw, qh] = local(l.square);
        let at = (x + sat * (qw - 1.0), y + (1.0 - val) * (qh - 1.0));
        let ink = Ink(hsv_to_rgb(picker.hsv));
        c.circle(at, 7.5 * s, rgba(0, 0, 0, 0.45 * a));
        c.circle(at, 6.5 * s, faded(WHITE, a));
        c.circle(at, 4.5 * s, faded(ink.d2d(), a));
        let [x, y, qw, qh] = local(l.strip);
        let hx = x + hue * (qw - 1.0);
        let pure = Ink(hsv_to_rgb([hue, 1.0, 1.0]));
        c.round_rect(hx - 4.5 * s, y - 3.5 * s, 9.0 * s, qh + 7.0 * s, 3.0 * s, rgba(0, 0, 0, 0.45 * a));
        c.round_rect(hx - 3.5 * s, y - 2.5 * s, 7.0 * s, qh + 5.0 * s, 2.5 * s, faded(WHITE, a));
        c.round_rect(hx - 2.0 * s, y - 1.0 * s, 4.0 * s, qh + 2.0 * s, 1.5 * s, faded(pure.d2d(), a));
        // the colour itself, and its hex (what's being typed, with a caret)
        let [x, y, qw, qh] = local(l.swatch);
        c.round_rect(x, y, qw, qh, 6.0 * s, faded(ink.d2d(), a));
        c.round_rect_outline([x, y, qw, qh], 6.0 * s, 1.0, rgba(255, 255, 255, 0.25 * a));
        let [x, y, qw, qh] = local(l.hex);
        let lit = if picker.hex_focus || self.hover == Some(Item::Hex) { 0.12 } else { 0.07 };
        c.round_rect(x, y, qw, qh, 6.0 * s, rgba(255, 255, 255, lit * a));
        let ui = Style { bold: false, ..Style::default() };
        let typing = !picker.hex.is_empty();
        let text = if typing { format!("#{}", picker.hex) } else { hex(ink) };
        let text_box = [x + 9.0 * s, y, qw - 12.0 * s, qh];
        self.fonts.draw_line(c, &text, &ui, 13.0 * s, text_box, faded(WHITE, if typing { 1.0 } else { 0.85 } * a));
        if typing || picker.hex_focus {
            let cx = text_box[0] + self.fonts.measure(&text, 13.0 * s) + 1.0 * s;
            c.fill(cx, y + qh * 0.25, 1.5 * s, qh * 0.5, faded(WHITE, a));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let ink = Ink([0x12, 0xAB, 0xFE]);
        assert_eq!(hex(ink), "#12ABFE");
        assert_eq!(parse_hex("#12abfe"), Some(ink));
        assert_eq!(parse_hex(" 12ABFE "), Some(ink));
        assert_eq!(parse_hex("12ABF"), None, "too short");
        assert_eq!(parse_hex("12ABFG"), None, "not hex");
        assert_eq!(parse_hex("#12ABFE0"), None, "too long");
    }

    #[test]
    fn hsv_round_trips_through_the_picker() {
        for rgb in [[0xE5, 0x40, 0x2F], [0x0A, 0x84, 0xFF], [0x34, 0xC7, 0x59], [0, 0, 0], [255, 255, 255], [0x80, 0x80, 0x80], [1, 2, 3]] {
            assert_eq!(hsv_to_rgb(rgb_to_hsv(rgb)), rgb, "{rgb:?}");
        }
        // a grey keeps the hue it was dragged at
        let [h, s, v] = rgb_to_hsv_near([0x80, 0x80, 0x80], [0.6, 0.9, 1.0]);
        assert_eq!((h, s), (0.6, 0.0));
        assert!((v - 0x80 as f32 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn the_square_and_strip_map_the_mouse() {
        let r = Rect { x: 100, y: 50, w: 201, h: 101 };
        assert_eq!(sv_at(r, (100, 50)), (0.0, 1.0), "top left: grey-white");
        assert_eq!(sv_at(r, (300, 150)), (1.0, 0.0), "bottom right: black");
        assert_eq!(sv_at(r, (200, 100)), (0.5, 0.5));
        assert_eq!(sv_at(r, (0, 999)), (0.0, 0.0), "outside: the nearest edge");
        assert_eq!(hue_at(r, 150), 0.25);
        assert_eq!(hue_at(r, 400), 1.0);
        let px = sv_pixels(0.0, 3, 2);
        assert_eq!(px[0], 0xFFFF_FFFF, "white top left");
        assert_eq!(px[2], 0xFFFF_0000, "the pure hue top right");
        assert!(px[3..].iter().all(|p| *p == 0xFF00_0000), "black along the bottom");
    }

    #[test]
    fn custom_colours_stay_newest_first() {
        let a = Ink([1, 2, 3]);
        let b = Ink([4, 5, 6]);
        let list = remembered(&[], a);
        let list = remembered(&list, b);
        assert_eq!(list, [b, a]);
        assert_eq!(remembered(&list, a), [a, b], "picked again: back to the front, once");
        assert_eq!(remembered(&list, PALETTE[0]), list, "palette colours aren't customs");
        let full = (0..5u8).fold(Vec::new(), |l, i| remembered(&l, Ink([i, i, i])));
        assert_eq!(full.len(), CUSTOMS);
        assert_eq!(full[0], Ink([4, 4, 4]));
    }
}
