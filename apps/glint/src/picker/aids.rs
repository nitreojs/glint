//! Selection aids over the frozen screen, each one a setting: a crosshair, dashed lines through
//! the pixel the picker aims at across the whole overlay, and a magnifier beside it, that pixel's
//! neighbourhood blown up with its position and colour under it. They aim at the cursor, or while
//! dragging at the selection's moving corner (snapped and fitted), the lines then leaving the
//! selection and its ring alone and the magnifier keeping to the side the drag heads for.
//!
//! They follow the cursor every frame, so nothing here goes through Direct2D per frame (a pass
//! costs a few hundred µs before it draws anything): the readout's glyphs and shapes are drawn
//! once into coverage masks and blended by hand.

use std::cell::RefCell;

use glint_core::geom::rect::Rect;
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Gdi::*;

use super::pill::{PILL_BG, PILL_TEXT, Pill};
use glint_shell::gfx::{Dib, Painter, rgba};

/// Which aids are on (the `crosshair` and `magnifier` settings).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Aids {
    pub crosshair: bool,
    pub magnifier: bool,
}

impl Aids {
    pub(super) fn any(self) -> bool {
        self.crosshair || self.magnifier
    }
}

/// Magnified pixels across and down; odd, so one sits in the middle.
const CELLS: i32 = 15;
/// The magnifier's border: 1 px black around 1 px white, like the selection's ring.
const BORDER: i32 = 2;
/// How much of the dash colour covers the screen under the crosshair, out of 256.
const DASH_INK: u32 = 150;
/// How far the grid between magnified pixels moves each one toward black or white, out of 256.
const GRID_INK: u32 = 56;
const WHITE: u32 = 0x00FF_FFFF;
const BLACK: u32 = 0;
/// What the readout can say.
const GLYPHS: &str = "0123456789, #ABCDEF";

/// Sizes in device pixels, for the system's DPI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Metrics {
    /// One magnified pixel, its grid line included.
    cell: i32,
    /// The magnified square, border included.
    zoom: i32,
    /// The readout under it.
    pill: (i32, i32),
    /// Between the aimed-at pixel and the magnifier, and between the square and the readout.
    gap: i32,
    inner: i32,
    dash: i32,
    /// The readout's swatch, the space before it (after the position) and after it (before the hex).
    swatch: i32,
    spread: i32,
    near: i32,
}

impl Metrics {
    /// The whole magnifier: the square over the readout.
    fn block(&self) -> (i32, i32) {
        (self.zoom.max(self.pill.0), self.zoom + self.inner + self.pill.1)
    }
}

/// How much of each pixel a shape covers, 0-255.
struct Mask {
    w: i32,
    h: i32,
    a: Vec<u8>,
}

impl Mask {
    /// The part `r` of `dib`, drawn in white on black.
    fn cut(dib: &Dib, r: Rect) -> Mask {
        let px = dib.pixels();
        let mut a = Vec::with_capacity((r.w * r.h) as usize);
        for y in r.y..r.bottom() {
            let row = &px[(y * dib.w + r.x) as usize..(y * dib.w + r.right()) as usize];
            // ClearType's coloured edges, averaged into grey ones
            a.extend(row.iter().map(|p| ((((p >> 16) & 0xFF) + ((p >> 8) & 0xFF) + (p & 0xFF)) / 3) as u8));
        }
        Mask { w: r.w, h: r.h, a }
    }

    /// Lays `colour` over the `stride` wide image `px` through the mask at (x, y), `alpha` of 256.
    fn blend(&self, px: &mut [u32], stride: i32, (x, y): (i32, i32), colour: u32, alpha: u32) {
        let rows = px.len() as i32 / stride;
        for my in (0..self.h).filter(|my| (0..rows).contains(&(y + my))) {
            for mx in (0..self.w).filter(|mx| (0..stride).contains(&(x + mx))) {
                let c = u32::from(self.a[(my * self.w + mx) as usize]);
                if c != 0 {
                    let i = ((y + my) * stride + x + mx) as usize;
                    px[i] = mix(px[i], colour, c * alpha / 255);
                }
            }
        }
    }
}

struct Glyph {
    ch: char,
    advance: f32,
    /// Drawn with its origin `glyph_pad` in.
    mask: Mask,
}

/// The aids' sizes and the readout's pieces, made when the picker opens, and the scratch images
/// each paint composes into.
pub(super) struct Kit {
    pub(super) m: Metrics,
    glyphs: Vec<Glyph>,
    glyph_pad: i32,
    pill: Mask,
    swatch: Mask,
    swatch_ring: Mask,
    pill_colour: (u32, u32),
    text_colour: (u32, u32),
    /// The crosshair's pieces and the magnifier, composed off-screen: one row of the overlay,
    /// one column, the magnifier block.
    row: RefCell<Option<Dib>>,
    col: RefCell<Option<Dib>>,
    back: RefCell<Option<Dib>>,
}

impl Kit {
    /// The readout is as wide as the widest it gets on a `screen` this size, so it doesn't twitch.
    pub(super) fn new(painter: &Painter, font: &IDWriteTextFormat, screen: (i32, i32)) -> Result<Kit, String> {
        let cell = painter.px(10.0).max(3.0) as i32;
        let zoom = CELLS * cell + 2 * BORDER;
        let (h, swatch, pad) = (painter.px(26.0) as i32, painter.px(10.0) as i32, painter.px(3.0) as i32);
        let mut m = Metrics {
            cell,
            zoom,
            pill: (0, h),
            gap: painter.px(16.0) as i32,
            inner: painter.px(4.0) as i32,
            dash: painter.px(4.0).max(2.0) as i32,
            swatch,
            spread: painter.px(10.0) as i32,
            near: painter.px(5.0) as i32,
        };
        let advances: Vec<(char, f32)> = GLYPHS.chars().map(|ch| (ch, painter.measure(&ch.to_string(), font).0)).collect();
        let width = |s: &str| s.chars().filter_map(|c| advances.iter().find(|a| a.0 == c)).map(|a| a.1).sum::<f32>();
        let widest = |n: i32| n.max(1).to_string().chars().map(|_| '8').collect::<String>();
        let longest = content(&width, &m, &format!("{}, {}", widest(screen.0 - 1), widest(screen.1 - 1)), "#DDDDDD");
        m.pill.0 = ((longest + painter.px(11.0) * 2.0).ceil() as i32).max(zoom);
        // one Direct2D pass draws every piece side by side; the masks are cut out of it
        let mut x = 0;
        let cells: Vec<Rect> = advances
            .iter()
            .map(|a| {
                let r = Rect { x, y: 0, w: a.1.ceil() as i32 + 2 * pad, h };
                x = r.right();
                r
            })
            .collect();
        let pill = Rect { x, y: 0, w: m.pill.0, h };
        let dot = Rect { x: pill.right(), y: 0, w: swatch, h: swatch };
        let ring = dot.offset(swatch, 0);
        let atlas = Dib::new(ring.right(), h)?;
        let white = rgba(255, 255, 255, 1.0);
        let f = |v: i32| v as f32;
        painter.draw(&atlas, |c| {
            for ((ch, advance), r) in advances.iter().zip(&cells) {
                c.text(&ch.to_string(), font, [f(r.x + pad), 0.0, advance + f(pad), f(h)], white);
            }
            c.round_rect(f(pill.x), 0.0, f(pill.w), f(h), f(h) / 2.0, white);
            c.round_rect(f(dot.x), 0.0, f(swatch), f(swatch), painter.px(2.0), white);
            c.round_rect_outline([f(ring.x) + 0.5, 0.5, f(swatch - 1), f(swatch - 1)], painter.px(2.0), 1.0, white);
        })?;
        unsafe {
            let _ = GdiFlush();
        }
        let glyphs = advances.iter().zip(&cells).map(|(&(ch, advance), &r)| Glyph { ch, advance, mask: Mask::cut(&atlas, r) });
        Ok(Kit {
            m,
            glyphs: glyphs.collect(),
            glyph_pad: pad,
            pill: Mask::cut(&atlas, pill),
            swatch: Mask::cut(&atlas, dot),
            swatch_ring: Mask::cut(&atlas, ring),
            pill_colour: split(PILL_BG),
            text_colour: split(PILL_TEXT),
            row: RefCell::new(None),
            col: RefCell::new(None),
            back: RefCell::new(None),
        })
    }

    fn glyph(&self, ch: char) -> Option<&Glyph> {
        self.glyphs.iter().find(|g| g.ch == ch)
    }

    fn width(&self, text: &str) -> f32 {
        text.chars().filter_map(|c| self.glyph(c)).map(|g| g.advance).sum()
    }

    fn content_width(&self, coords: &str, hex: &str) -> f32 {
        content(&|s| self.width(s), &self.m, coords, hex)
    }

    /// Writes `text` from x = `pen` along the `stride` wide image `px`, its line box at `top`.
    fn text(&self, px: &mut [u32], stride: i32, text: &str, mut pen: f32, top: i32) {
        let (colour, alpha) = self.text_colour;
        for g in text.chars().filter_map(|c| self.glyph(c)) {
            g.mask.blend(px, stride, (pen.round() as i32 - self.glyph_pad, top), colour, alpha);
            pen += g.advance;
        }
    }

    /// The readout pill at `r` in `back`: the pixel's position, a swatch of its colour, its hex.
    fn readout(&self, back: &mut Dib, r: Rect, at: (i32, i32), colour: u32) {
        let (stride, m) = (back.w, self.m);
        let px = back.pixels_mut();
        let (bg, bg_alpha) = self.pill_colour;
        self.pill.blend(px, stride, (r.x, r.y), bg, bg_alpha);
        let (coords, hex) = (format!("{}, {}", at.0, at.1), format!("#{colour:06X}"));
        let cw = self.width(&coords);
        let x = r.x as f32 + ((r.w as f32 - self.content_width(&coords, &hex)) / 2.0).round();
        self.text(px, stride, &coords, x, r.y);
        let sx = (x + cw).round() as i32 + m.spread;
        let sy = r.y + (r.h - m.swatch) / 2;
        self.swatch.blend(px, stride, (sx, sy), colour, 256);
        self.swatch_ring.blend(px, stride, (sx, sy), WHITE, 115);
        self.text(px, stride, &hex, (sx + m.swatch + m.near) as f32, r.y);
    }
}

/// How wide the readout's content is with text `width`s: the position, a gap, the swatch, a
/// little gap, the hex.
fn content(width: &dyn Fn(&str) -> f32, m: &Metrics, coords: &str, hex: &str) -> f32 {
    width(coords) + (m.spread + m.swatch + m.near) as f32 + width(hex)
}

/// A Direct2D colour as 0x00RRGGBB and its alpha out of 256.
fn split(c: D2D1_COLOR_F) -> (u32, u32) {
    let ch = |v: f32| (v * 255.0).round() as u32;
    ((ch(c.r) << 16) | (ch(c.g) << 8) | ch(c.b), (c.a * 256.0).round() as u32)
}

/// A scratch image at least `w` x `h`, made again only when it's too small.
fn scratch(slot: &RefCell<Option<Dib>>, w: i32, h: i32) -> Option<std::cell::RefMut<'_, Dib>> {
    let mut s = slot.borrow_mut();
    if s.as_ref().is_none_or(|d| d.w < w || d.h < h) {
        *s = Some(Dib::new(w, h).ok()?);
    }
    Some(std::cell::RefMut::map(s, |d| d.as_mut().unwrap()))
}

/// What the aids show (client coordinates); equal layouts paint the same pixels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Shown {
    /// The pixel aimed at.
    at: (i32, i32),
    /// The crosshair's pieces, 1 px tall and 1 px wide; they never overlap.
    rows: Vec<Rect>,
    cols: Vec<Rect>,
    /// The magnifier: the square over the readout.
    loupe: Option<Rect>,
}

impl Shown {
    /// Every pixel the aids cover.
    pub(super) fn rects(&self) -> impl Iterator<Item = Rect> + '_ {
        self.rows.iter().chain(&self.cols).copied().chain(self.loupe)
    }

    pub(super) fn loupe(&self) -> Option<Rect> {
        self.loupe
    }
}

/// Where the aids go for the pixel `at` on `screen` (the whole overlay): the crosshair's lines
/// leave `clear` alone (the selection and its ring while dragging), and the magnifier goes to the
/// side `toward` says (left, up) on the monitor under `at`, flipping where it wouldn't fit.
pub(super) fn layout(
    aids: Aids,
    at: (i32, i32),
    clear: Option<Rect>,
    toward: (bool, bool),
    screen: Rect,
    monitors: &[Rect],
    m: &Metrics,
) -> Shown {
    let mut shown = Shown { at, ..Shown::default() };
    if aids.crosshair {
        let row = Rect { x: screen.x, y: at.1, w: screen.w, h: 1 };
        let col = Rect { x: at.0, y: screen.y, w: 1, h: screen.h };
        // the row has the middle pixel
        let hole = clear.unwrap_or(Rect { x: at.0, y: at.1, w: 1, h: 1 });
        shown.rows = minus(row, clear);
        shown.cols = minus(col, Some(hole));
    }
    if aids.magnifier
        && let Some(mon) = monitors.iter().find(|r| r.contains(at.0, at.1))
    {
        let (w, h) = m.block();
        let x = beside(at.0, w, m.gap, toward.0, (mon.x, mon.right()));
        let y = beside(at.1, h, m.gap, toward.1, (mon.y, mon.bottom()));
        shown.loupe = Some(Rect { x, y, w, h });
    }
    shown
}

/// A 1 px line without what of it `hole` covers: up to two pieces.
fn minus(line: Rect, hole: Option<Rect>) -> Vec<Rect> {
    let Some(cut) = hole.and_then(|h| h.intersect(&line)) else { return vec![line] };
    let pieces = if line.h == 1 {
        [Rect { w: cut.x - line.x, ..line }, Rect { x: cut.right(), w: line.right() - cut.right(), ..line }]
    } else {
        [Rect { h: cut.y - line.y, ..line }, Rect { y: cut.bottom(), h: line.bottom() - cut.bottom(), ..line }]
    };
    pieces.into_iter().filter(|p| p.w > 0 && p.h > 0).collect()
}

/// Where a `len` long box goes on one axis: `gap` past the pixel at `p`, before it if `before`,
/// on the other side where it doesn't fit inside `lo..hi`, pushed inside if neither side fits.
fn beside(p: i32, len: i32, gap: i32, before: bool, (lo, hi): (i32, i32)) -> i32 {
    let (ahead, behind) = (p + 1 + gap, p - gap - len);
    let (fits_ahead, fits_behind) = (ahead + len <= hi, behind >= lo);
    match (before, fits_ahead, fits_behind) {
        (false, true, _) | (true, true, false) => ahead,
        (true, _, true) | (false, false, true) => behind,
        _ => ahead.min(hi - len).max(lo),
    }
}

/// Paints the aids in `shown` that fall in `rp`, each pixel once: `base(hdc, r, org)` draws what's
/// under client rect `r` at `r - org`. The lines stay under the size `label`, the editor's
/// `chrome` and the magnifier; the magnifier stays under `chrome` and goes over `label`, whose
/// pixels it takes in.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn paint(
    hdc: HDC,
    rp: Rect,
    shown: &Shown,
    kit: &Kit,
    bright: &Dib,
    base: &dyn Fn(HDC, Rect, (i32, i32)),
    label: Option<&Pill>,
    chrome: &[Rect],
) {
    unsafe {
        let blit = |src: &Dib, r: Rect, keep: &mut dyn Iterator<Item = Rect>| {
            let saved = SaveDC(hdc);
            for k in keep {
                ExcludeClipRect(hdc, k.x, k.y, k.right(), k.bottom());
            }
            let _ = BitBlt(hdc, r.x, r.y, r.w, r.h, Some(src.dc), 0, 0, SRCCOPY);
            let _ = RestoreDC(hdc, saved);
        };
        let lines = shown.rows.iter().map(|&r| (r, true)).chain(shown.cols.iter().map(|&r| (r, false)));
        for (line, across) in lines {
            let Some(piece) = line.intersect(&rp) else { continue };
            let slot = if across { &kit.row } else { &kit.col };
            let Some(mut strip) = scratch(slot, piece.w, piece.h) else { continue };
            base(strip.dc, piece, (piece.x, piece.y));
            let _ = GdiFlush();
            let (start, len) = if across { (piece.x, piece.w) } else { (piece.y, piece.h) };
            // a row's pixels sit side by side; a column's one per row, its width apart
            let step = if across { 1 } else { strip.w as usize };
            dash(strip.pixels_mut().iter_mut().step_by(step).take(len as usize), start, kit.m.dash);
            let mut keep = chrome.iter().copied().chain(label.map(|l| l.rect)).chain(shown.loupe);
            blit(&strip, piece, &mut keep);
        }
        let Some(block) = shown.loupe.filter(|l| l.intersect(&rp).is_some()) else { return };
        let Some(mut back) = scratch(&kit.back, block.w, block.h) else { return };
        base(back.dc, block, (block.x, block.y));
        if let Some(l) = label.filter(|l| l.rect.intersect(&block).is_some())
            && let Some(dib) = &l.dib
        {
            let r = l.rect.offset(-block.x, -block.y);
            let _ = BitBlt(back.dc, r.x, r.y, r.w, r.h, Some(dib.dc), 0, 0, SRCCOPY);
        }
        let _ = GdiFlush();
        let m = kit.m;
        let zoom = Rect { x: (block.w - m.zoom) / 2, y: 0, w: m.zoom, h: m.zoom };
        magnify(&mut back, zoom, &m, bright, shown.at);
        let colour = bright.pixel(shown.at.0, shown.at.1).unwrap_or(0) & WHITE;
        let pill = Rect { x: (block.w - m.pill.0) / 2, y: m.zoom + m.inner, w: m.pill.0, h: m.pill.1 };
        kit.readout(&mut back, pill, shown.at, colour);
        blit(&back, block, &mut chrome.iter().copied());
    }
}

/// Two-tone dashes along a 1 px line whose first pixel is `start` along its axis: anchored to the
/// screen, so they hold still while the cursor moves along the line.
fn dash<'a>(px: impl Iterator<Item = &'a mut u32>, start: i32, len: i32) {
    for (p, at) in px.zip(start..) {
        let ink = if at.div_euclid(len) % 2 == 0 { WHITE } else { BLACK };
        *p = mix(*p, ink, DASH_INK);
    }
}

/// `a` moved `t`/256 of the way to `b`, per channel.
fn mix(a: u32, b: u32, t: u32) -> u32 {
    let ch = |s: u32| {
        let (x, y) = ((a >> s) & 0xFF, (b >> s) & 0xFF);
        ((x * (256 - t) + y * t) >> 8) << s
    };
    ch(0) | ch(8) | ch(16)
}

/// Fills `r` (clipped to the image) in the `w` wide image `px`.
fn fill(px: &mut [u32], w: i32, r: Rect, colour: u32) {
    let h = px.len() as i32 / w;
    let Some(r) = r.intersect(&Rect { x: 0, y: 0, w, h }) else { return };
    for y in r.y..r.bottom() {
        px[(y * w + r.x) as usize..(y * w + r.right()) as usize].fill(colour);
    }
}

fn outline(px: &mut [u32], w: i32, r: Rect, colour: u32) {
    for side in [
        Rect { h: 1, ..r },
        Rect { y: r.bottom() - 1, h: 1, ..r },
        Rect { w: 1, ..r },
        Rect { x: r.right() - 1, w: 1, ..r },
    ] {
        fill(px, w, side, colour);
    }
}

/// The pixels around `at` in `bright`, nearest-neighbour into the square `zoom` of `back`: a
/// faint grid between them, the middle one ringed black and white, black past the screen's edge.
fn magnify(back: &mut Dib, zoom: Rect, m: &Metrics, bright: &Dib, at: (i32, i32)) {
    let w = back.w;
    let px = back.pixels_mut();
    outline(px, w, zoom, BLACK);
    outline(px, w, zoom.inflate(-1), WHITE);
    let (x0, y0, half, c) = (zoom.x + BORDER, zoom.y + BORDER, CELLS / 2, m.cell);
    for j in 0..CELLS {
        for i in 0..CELLS {
            let colour = bright.pixel(at.0 + i - half, at.1 + j - half).map_or(BLACK, |p| p & WHITE);
            let cell = Rect { x: x0 + i * c, y: y0 + j * c, w: c, h: c };
            fill(px, w, cell, colour);
            let toward = if luma(colour) > 140 { BLACK } else { WHITE };
            let grid = mix(colour, toward, GRID_INK);
            if i < CELLS - 1 {
                fill(px, w, Rect { x: cell.right() - 1, w: 1, ..cell }, grid);
            }
            if j < CELLS - 1 {
                fill(px, w, Rect { y: cell.bottom() - 1, h: 1, ..cell }, grid);
            }
        }
    }
    let middle = Rect { x: x0 + half * c, y: y0 + half * c, w: c, h: c };
    outline(px, w, middle.inflate(1), BLACK);
    outline(px, w, middle, WHITE);
}

fn luma(c: u32) -> u32 {
    let (r, g, b) = ((c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF);
    (r * 54 + g * 183 + b * 19) >> 8
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: Metrics = Metrics { cell: 10, zoom: 154, pill: (170, 26), gap: 16, inner: 4, dash: 4, swatch: 10, spread: 10, near: 5 };
    const SCREEN: Rect = Rect { x: 0, y: 0, w: 3440 + 1920, h: 1440 };
    const LEFT: Rect = Rect { x: 0, y: 0, w: 3440, h: 1440 };
    const RIGHT: Rect = Rect { x: 3440, y: 0, w: 1920, h: 1080 };
    const BOTH: Aids = Aids { crosshair: true, magnifier: true };

    fn at(p: (i32, i32), toward: (bool, bool)) -> Rect {
        layout(BOTH, p, None, toward, SCREEN, &[LEFT, RIGHT], &M).loupe().unwrap()
    }

    #[test]
    fn the_magnifier_sits_down_right_and_flips_off_the_monitor_edges() {
        let (w, h) = M.block();
        assert_eq!((w, h), (170, 184));
        assert_eq!(at((1000, 500), (false, false)), Rect { x: 1017, y: 517, w, h }, "down and right of the pixel");
        assert_eq!(at((3400, 500), (false, false)).x, 3400 - 16 - w, "left near the monitor's right edge");
        assert_eq!(at((1000, 1400), (false, false)).y, 1400 - 16 - h, "up near its bottom");
        let corner = at((3439, 1439), (false, false));
        assert_eq!((corner.right(), corner.bottom()), (3439 - 16, 1439 - 16), "both in the corner");
        // the right monitor is shorter: its own bottom counts, not the overlay's
        assert_eq!(at((4000, 1000), (false, false)).y, 1000 - 16 - h);
        assert_eq!(at((1000, 500), (true, true)), Rect { x: 1000 - 16 - w, y: 500 - 16 - h, w, h }, "toward the drag");
        assert_eq!(at((10, 10), (true, true)), Rect { x: 27, y: 27, w, h }, "flipped back at the top left");
        let none = layout(BOTH, (4000, 1200), None, (false, false), SCREEN, &[LEFT, RIGHT], &M);
        assert!(none.loupe().is_none() && !none.rows.is_empty(), "no monitor under it: lines only");
    }

    #[test]
    fn the_crosshair_crosses_everything_but_the_selection_and_never_twice() {
        let s = layout(Aids { crosshair: true, magnifier: false }, (100, 50), None, (false, false), SCREEN, &[LEFT], &M);
        assert_eq!(s.rows, [Rect { x: 0, y: 50, w: SCREEN.w, h: 1 }]);
        assert_eq!(s.cols, [Rect { x: 100, y: 0, w: 1, h: 50 }, Rect { x: 100, y: 51, w: 1, h: 1389 }]);
        assert_eq!(s.loupe(), None);
        let sel = Rect { x: 40, y: 20, w: 61, h: 31 }.inflate(2);
        let s = layout(Aids { crosshair: true, magnifier: false }, (100, 50), Some(sel), (false, false), SCREEN, &[LEFT], &M);
        assert_eq!(s.rows, [Rect { x: 0, y: 50, w: 38, h: 1 }, Rect { x: 103, y: 50, w: SCREEN.w - 103, h: 1 }]);
        assert_eq!(s.cols, [Rect { x: 100, y: 0, w: 1, h: 18 }, Rect { x: 100, y: 53, w: 1, h: 1387 }]);
        let at_edge = layout(Aids { crosshair: true, magnifier: false }, (0, 0), None, (false, false), SCREEN, &[LEFT], &M);
        assert_eq!(at_edge.cols, [Rect { x: 0, y: 1, w: 1, h: 1439 }]);
        assert!(layout(Aids::default(), (5, 5), None, (false, false), SCREEN, &[LEFT], &M).rects().next().is_none());
    }

    #[test]
    fn dashes_hold_still_along_the_screen_and_read_on_black_and_white() {
        let mut a = [0u32; 12];
        dash(a.iter_mut(), 2, 4);
        let mut b = [0u32; 10];
        dash(b.iter_mut(), 4, 4);
        assert_eq!(a[2..], b, "the same screen pixels get the same dash");
        let (on_black, on_white) = (mix(BLACK, WHITE, DASH_INK), mix(WHITE, BLACK, DASH_INK));
        assert!(luma(on_black) > 120 && luma(on_white) < 135, "{on_black:06X} {on_white:06X}");
        assert_eq!(mix(0x0010_2030, 0x0010_2030, 99), 0x0010_2030);
    }

    #[test]
    fn the_readout_fits_the_widest_position_and_says_the_colour() {
        let painter = Painter::new().unwrap();
        let font = painter.font(13.0).unwrap();
        let kit = Kit::new(&painter, &font, (3440, 1440)).unwrap();
        let widest = kit.content_width("3439, 1439", "#DDDDDD");
        assert!(widest + painter.px(22.0) <= kit.m.pill.0 as f32, "{widest} in {}", kit.m.pill.0);
        assert_eq!(split(PILL_BG), (0x10_1014, 220));
        // drawn over white: dark pill, white text inside it, the swatch red
        let mut back = Dib::new(kit.m.pill.0, kit.m.pill.1).unwrap();
        back.pixels_mut().fill(WHITE);
        let r = Rect { x: 0, y: 0, w: kit.m.pill.0, h: kit.m.pill.1 };
        kit.readout(&mut back, r, (12, 34), 0x00FF_0000);
        let mid = |x: i32| back.pixel(x, r.h / 2).unwrap() & WHITE;
        assert!(luma(back.pixel(r.w / 2, 1).unwrap()) < 60, "the pill darkens what's under it");
        assert_eq!(back.pixel(0, 0), Some(WHITE), "rounded: the corner shows through");
        assert!((0..r.w).any(|x| mid(x) == 0x00FF_0000), "the swatch has the colour");
        assert!((0..r.w).filter(|&x| luma(mid(x)) > 200).count() > 4, "the text is light");
    }
}
