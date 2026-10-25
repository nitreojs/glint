use glint_core::geom::rect::Rect;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Gdi::GdiFlush;

use super::pill::{Pill, render_pill};
use super::{DIM, Monitor};
use crate::platform::capture;
use crate::platform::cursor::{self, CursorImage};
use glint_shell::gfx::{Dib, Painter};

pub(super) struct Frozen {
    pub(super) bright: Dib,
    pub(super) dim: Dib,
    /// The hint pills, over the dimmed image and over the bright one (where a highlight covers a hint).
    /// Neither image has them baked in: the editor and screenshots use both.
    pub(super) hints: Vec<(Pill, Pill)>,
    /// The screenshot's cursor drawn into both images while picking, and what it covered.
    cursor: Option<Covered>,
}

/// Pixels of both images under `rect`, as they were before the cursor was drawn over them.
struct Covered {
    rect: Rect,
    bright: Vec<u32>,
    dim: Vec<u32>,
}

impl Frozen {
    /// Freezes the virtual screen `area`; what no output shows is black.
    pub(super) fn capture(area: Rect, outputs: &[Monitor]) -> Result<Frozen, String> {
        let monitors: Vec<Rect> = outputs.iter().map(|o| o.rect).collect();
        let mut bright = Dib::new(area.w, area.h)?;
        capture::grab(&mut bright, area, &monitors)?;
        Frozen::new(bright)
    }

    pub(super) fn new(bright: Dib) -> Result<Frozen, String> {
        let mut dim = Dib::new(bright.w, bright.h)?;
        for (d, &s) in dim.pixels_mut().iter_mut().zip(bright.pixels()) {
            *d = dimmed(s);
        }
        unsafe {
            let _ = GdiFlush();
        }
        Ok(Frozen { bright, dim, hints: Vec::new(), cursor: None })
    }

    /// Draws the cursor the screenshot keeps into the frozen screen, hotspot at `at` (client
    /// coordinates), so picking shows it where the shot will have it.
    pub(super) fn show_cursor(&mut self, img: &CursorImage, at: (i32, i32)) {
        self.hide_cursor();
        let whole = Rect { x: 0, y: 0, w: self.bright.w, h: self.bright.h };
        let box_ = Rect { x: at.0 - img.hot.0, y: at.1 - img.hot.1, w: img.w, h: img.h };
        let Some(rect) = box_.intersect(&whole) else { return };
        let (bright, dim) = (copy(&self.bright, rect), copy(&self.dim, rect));
        let w = self.bright.w;
        cursor::stamp(self.bright.pixels_mut(), w, img, at);
        for y in rect.y..rect.bottom() {
            let row = (y * w) as usize;
            for i in row + rect.x as usize..row + rect.right() as usize {
                self.dim.pixels_mut()[i] = dimmed(self.bright.pixels()[i]);
            }
        }
        unsafe {
            let _ = GdiFlush();
        }
        self.cursor = Some(Covered { rect, bright, dim });
    }

    /// Takes the cursor back out (the editor shows it as a mark of its own); where it was.
    pub(super) fn hide_cursor(&mut self) -> Option<Rect> {
        let Covered { rect, bright, dim } = self.cursor.take()?;
        paste(&mut self.bright, rect, &bright);
        paste(&mut self.dim, rect, &dim);
        unsafe {
            let _ = GdiFlush();
        }
        Some(rect)
    }

    pub(super) fn add_hints(
        &mut self,
        rects: &[Rect],
        hint: &str,
        painter: &Painter,
        font: &IDWriteTextFormat,
    ) -> Result<(), String> {
        for r in rects {
            let on_dim = render_pill(painter, font, hint, &self.dim, r.x, r.y)?;
            let on_bright = render_pill(painter, font, hint, &self.bright, r.x, r.y)?;
            self.hints.push((on_dim, on_bright));
        }
        unsafe {
            let _ = GdiFlush();
        }
        Ok(())
    }
}

/// A pixel as the dimmed copy of the screen shows it.
fn dimmed(s: u32) -> u32 {
    let rb = (((s & 0x00FF_00FF) * DIM) >> 8) & 0x00FF_00FF;
    let g = (((s & 0x0000_FF00) * DIM) >> 8) & 0x0000_FF00;
    rb | g
}

fn copy(dib: &Dib, r: Rect) -> Vec<u32> {
    let px = dib.pixels();
    (r.y..r.bottom()).flat_map(|y| px[(y * dib.w + r.x) as usize..(y * dib.w + r.right()) as usize].iter().copied()).collect()
}

fn paste(dib: &mut Dib, r: Rect, rows: &[u32]) {
    let w = dib.w;
    for (row, y) in rows.chunks_exact(r.w as usize).zip(r.y..) {
        dib.pixels_mut()[(y * w + r.x) as usize..(y * w + r.right()) as usize].copy_from_slice(row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Frozen {
        let mut dib = Dib::new(8, 6).unwrap();
        for (i, p) in dib.pixels_mut().iter_mut().enumerate() {
            *p = 0xFF00_0000 | (i as u32 * 0x0003_0507);
        }
        Frozen::new(dib).unwrap()
    }

    #[test]
    fn the_cursor_shows_while_picking_and_comes_out_cleanly() {
        let img = CursorImage { w: 3, h: 3, hot: (1, 1), px: vec![0xFFFF_FFFF; 9] };
        let mut f = screen();
        let (bright, dim) = (f.bright.pixels().to_vec(), f.dim.pixels().to_vec());
        f.show_cursor(&img, (7, 0));
        assert_eq!(f.bright.pixels()[6], 0xFFFF_FFFF, "drawn where its hotspot puts it");
        assert_eq!(f.dim.pixels()[6], dimmed(0xFFFF_FFFF), "and dimmed outside the selection");
        assert_ne!(f.bright.pixels(), &bright[..]);
        assert_eq!(f.hide_cursor(), Some(Rect { x: 6, y: 0, w: 2, h: 2 }), "clipped to the screen");
        assert_eq!((f.bright.pixels(), f.dim.pixels()), (&bright[..], &dim[..]), "what it covered is back");
        assert_eq!(f.hide_cursor(), None);
    }
}
