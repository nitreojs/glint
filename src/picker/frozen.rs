use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Gdi::GdiFlush;

use super::pill::{Pill, render_pill};
use super::{DIM, Monitor};
use crate::platform::capture;
use crate::platform::gfx::{Dib, Painter};
use crate::platform::rect::Rect;

pub(super) struct Frozen {
    pub(super) bright: Dib,
    pub(super) dim: Dib,
    /// The hint pills, over the dimmed image and over the bright one (where a highlight covers a hint).
    /// Neither image has them baked in: the editor and screenshots use both.
    pub(super) hints: Vec<(Pill, Pill)>,
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
            let rb = (((s & 0x00FF_00FF) * DIM) >> 8) & 0x00FF_00FF;
            let g = (((s & 0x0000_FF00) * DIM) >> 8) & 0x0000_FF00;
            *d = rb | g;
        }
        unsafe {
            let _ = GdiFlush();
        }
        Ok(Frozen { bright, dim, hints: Vec::new() })
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
