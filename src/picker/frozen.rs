use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Gdi::*;

use super::pill::{Pill, render_pill};
use super::{DIM, Monitor};
use crate::platform::capture;
use crate::platform::gfx::{Dib, Painter};
use crate::platform::rect::Rect;

pub(super) struct Frozen {
    pub(super) bright: Dib,
    pub(super) dim: Dib,
    /// The hint pills drawn over the bright image, shown where a highlight covers a hint.
    pub(super) hints: Vec<Pill>,
}

impl Frozen {
    /// Freezes the virtual screen `area`; what no output shows is black.
    pub(super) fn capture(
        area: Rect,
        outputs: &[Monitor],
        hints: &[Rect],
        hint: &str,
        painter: &Painter,
        font: &IDWriteTextFormat,
    ) -> Result<Frozen, String> {
        let monitors: Vec<Rect> = outputs.iter().map(|o| o.rect).collect();
        let mut bright = Dib::new(area.w, area.h)?;
        capture::grab(&mut bright, area, &monitors)?;
        unsafe {
            let mut dim = Dib::new(area.w, area.h)?;
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
