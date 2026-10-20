use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Gdi::*;

use super::DIM;
use super::pill::{Pill, render_pill};
use crate::platform::gfx::{Dib, Painter};
use crate::platform::rect::Rect;

pub(super) struct Frozen {
    pub(super) bright: Dib,
    pub(super) dim: Dib,
    /// The hint pills drawn over the bright image, shown where a highlight covers a hint.
    pub(super) hints: Vec<Pill>,
}

impl Frozen {
    pub(super) fn capture(area: Rect, hints: &[Rect], hint: &str, painter: &Painter, font: &IDWriteTextFormat) -> Result<Frozen, String> {
        unsafe {
            let bright = Dib::new(area.w, area.h)?;
            let mut dim = Dib::new(area.w, area.h)?;
            let screen = GetDC(None);
            let _ = BitBlt(bright.dc, 0, 0, area.w, area.h, Some(screen), area.x, area.y, ROP_CODE(SRCCOPY.0 | CAPTUREBLT.0));
            ReleaseDC(None, screen);
            let _ = GdiFlush();
            for (d, &s) in dim.pixels_mut().iter_mut().zip(bright.pixels()) {
                let rb = (((s & 0x00FF_00FF) * DIM) >> 8) & 0x00FF_00FF;
                let g = (((s & 0x0000_FF00) * DIM) >> 8) & 0x0000_FF00;
                *d = rb | g;
            }
            // The hint is baked into the dimmed image only: the bright one is what a screenshot
            // captures. A copy drawn over the bright image shows where a highlight covers it.
            let mut pills = Vec::with_capacity(hints.len());
            for r in hints {
                let on_dim = render_pill(painter, font, hint, &dim, r.x, r.y)?;
                if let Some(src) = &on_dim.dib {
                    let _ = BitBlt(dim.dc, r.x, r.y, r.w, r.h, Some(src.dc), 0, 0, SRCCOPY);
                }
                pills.push(render_pill(painter, font, hint, &bright, r.x, r.y)?);
            }
            let _ = GdiFlush();
            Ok(Frozen { bright, dim, hints: pills })
        }
    }
}
