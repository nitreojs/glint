use std::time::Instant;

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::*;

use super::frozen::Frozen;
use super::pill::Pill;
use super::{RING, Screen, State, rgn};
use crate::platform::gfx::Dib;
use crate::platform::log::log;
use crate::platform::rect::Rect;

impl State {
    /// Replaces the selection and, if `label` is Some, the label; invalidates exactly the
    /// pixels that change. The chosen rects stay lit alongside the selection.
    pub(super) fn set_selection(&mut self, sel: Option<Rect>, label: Option<Option<Pill>>) {
        let lit: Vec<Rect> = self.chosen.iter().map(|&c| self.to_client(c)).collect();
        if self.lit == lit && self.sel == sel && label.is_none() {
            return;
        }
        let old = (std::mem::replace(&mut self.lit, lit), std::mem::replace(&mut self.sel, sel));
        let mut labels = Vec::new();
        if let Some(label) = label {
            labels.extend(self.label.as_ref().map(|l| l.rect));
            self.label = label;
            labels.extend(self.label.as_ref().map(|l| l.rect));
        }
        if matches!(self.screen, Screen::Live(_)) {
            return self.schedule_live_sync();
        }
        unsafe {
            let damage = CreateRectRgn(0, 0, 0, 0);
            let add = |r: HRGN| {
                CombineRgn(Some(damage), Some(damage), Some(r), RGN_OR);
                let _ = DeleteObject(r.into());
            };
            // Pixels that switch between dimmed and bright, and every ring.
            let [a, a_white, a_black] = regions(&old.0, old.1);
            let [b, b_white, b_black] = regions(&self.lit, self.sel);
            add(combine(a, b, RGN_XOR));
            for r in [a, b] {
                let _ = DeleteObject(r.into());
            }
            for r in [a_white, a_black, b_white, b_black] {
                add(r);
            }
            for l in labels {
                add(rgn(l));
            }
            let _ = InvalidateRgn(self.hwnd, Some(damage), false);
            let _ = DeleteObject(damage.into());
        }
    }

    pub(super) fn schedule_live_sync(&self) {
        let corner = RECT { left: 0, top: 0, right: 1, bottom: 1 };
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), Some(&corner), false);
        }
    }

    pub(super) fn invalidate(&self, rects: &[Rect]) {
        for r in rects {
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), Some(&r.to_win()), false);
            }
        }
    }

    /// Every pixel is written exactly once per paint, so nothing flickers.
    pub(super) unsafe fn paint(&mut self) {
        if let Screen::Live(live) = &mut self.screen {
            unsafe {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(self.hwnd, &mut ps);
                self.last_paint.set(Instant::now());
                FillRect(hdc, &ps.rcPaint, self.black);
                let _ = EndPaint(self.hwnd, &ps);
            }
            return live.sync(&self.lit, self.sel, self.label.as_ref(), &self.painter, &self.font);
        }
        // Rendering can change what the editor covers (a tooltip appears): paint that too.
        if let (Some(ed), Screen::Frozen(f)) = (self.edit.as_mut(), &self.screen) {
            match ed.render(&f.bright, &self.painter, &self.font) {
                Ok(moved) => self.invalidate(&moved),
                Err(e) => log(&format!("editor render failed: {e}")),
            }
        }
        let Screen::Frozen(f) = &self.screen else { return };
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(self.hwnd, &mut ps);
            self.last_paint.set(Instant::now());
            let rp = Rect::from_win(&ps.rcPaint);
            let label = self.label.as_ref();

            if let Some(ed) = self.edit.as_mut() {
                // Dimmed screen around everything else.
                let saved = SaveDC(hdc);
                let mut holes = ed.extent();
                holes.extend(label.map(|l| l.rect));
                for h in &holes {
                    ExcludeClipRect(hdc, h.x, h.y, h.right(), h.bottom());
                }
                let _ = BitBlt(hdc, rp.x, rp.y, rp.w, rp.h, Some(f.dim.dc), rp.x, rp.y, SRCCOPY);
                let _ = RestoreDC(hdc, saved);

                let saved = SaveDC(hdc);
                if let Some(l) = label {
                    ExcludeClipRect(hdc, l.rect.x, l.rect.y, l.rect.right(), l.rect.bottom());
                }
                if let Err(e) = ed.paint(hdc, rp, &f.dim, &self.painter, &self.font) {
                    log(&format!("editor paint failed: {e}"));
                }
                let _ = RestoreDC(hdc, saved);
            } else {
                paint_pick(hdc, rp, f, &self.lit, self.sel, label.map(|l| l.rect), [self.white, self.black]);
            }
            if let Some(l) = label
                && let Some(dib) = &l.dib
            {
                let r = l.rect;
                let _ = BitBlt(hdc, r.x, r.y, r.w, r.h, Some(dib.dc), 0, 0, SRCCOPY);
            }
            let _ = EndPaint(self.hwnd, &ps);
        }
    }
}

/// The union of `rects`, each grown by `by`.
fn union(rects: &[Rect], by: i32) -> HRGN {
    unsafe {
        let all = CreateRectRgn(0, 0, 0, 0);
        for r in rects {
            let one = rgn(r.inflate(by));
            CombineRgn(Some(all), Some(all), Some(one), RGN_OR);
            let _ = DeleteObject(one.into());
        }
        all
    }
}

fn combine(a: HRGN, b: HRGN, mode: RGN_COMBINE_MODE) -> HRGN {
    unsafe {
        let out = CreateRectRgn(0, 0, 0, 0);
        CombineRgn(Some(out), Some(a), Some(b), mode);
        out
    }
}

/// What's bright, in the 1 px white ring and in the 1 px black ring around it. Overlapping
/// chosen rects share one outline; the selection's ring stays whole over them.
fn regions(chosen: &[Rect], sel: Option<Rect>) -> [HRGN; 3] {
    let sel: Vec<Rect> = sel.into_iter().collect();
    let s = [union(&sel, 0), union(&sel, 1), union(&sel, RING)];
    let c = [union(chosen, 0), union(chosen, 1), union(chosen, RING)];
    let bright = combine(c[0], s[2], RGN_DIFF);
    unsafe {
        CombineRgn(Some(bright), Some(bright), Some(s[0]), RGN_OR);
    }
    let ring = |i: usize| {
        let own = combine(s[i], s[i - 1], RGN_DIFF);
        let shared = combine(c[i], c[i - 1], RGN_DIFF);
        unsafe {
            CombineRgn(Some(shared), Some(shared), Some(s[2]), RGN_DIFF);
            CombineRgn(Some(own), Some(own), Some(shared), RGN_OR);
            let _ = DeleteObject(shared.into());
        }
        own
    };
    let (white, black) = (ring(1), ring(2));
    for r in s.into_iter().chain(c) {
        unsafe {
            let _ = DeleteObject(r.into());
        }
    }
    [bright, white, black]
}

/// Pick mode: the lit rects and the selection bright inside their rings, dimmed screen elsewhere,
/// the label's pixels left out.
unsafe fn paint_pick(
    hdc: HDC,
    rp: Rect,
    f: &Frozen,
    chosen: &[Rect],
    sel: Option<Rect>,
    label: Option<Rect>,
    brushes: [HBRUSH; 2],
) {
    unsafe {
        let free = rgn(rp);
        if let Some(l) = label {
            let l = rgn(l);
            CombineRgn(Some(free), Some(free), Some(l), RGN_DIFF);
            let _ = DeleteObject(l.into());
        }
        let [bright, white, black] = regions(chosen, sel);
        let dim = combine(free, bright, RGN_DIFF);
        for r in [white, black] {
            CombineRgn(Some(dim), Some(dim), Some(r), RGN_DIFF);
        }
        layer(hdc, rp, dim, &f.dim, f.hints.iter().map(|h| &h.0));
        CombineRgn(Some(bright), Some(bright), Some(free), RGN_AND);
        layer(hdc, rp, bright, &f.bright, f.hints.iter().map(|h| &h.1));
        for (ring, brush) in [white, black].into_iter().zip(brushes) {
            CombineRgn(Some(ring), Some(ring), Some(free), RGN_AND);
            let _ = FillRgn(hdc, ring, brush);
        }
        for r in [free, dim, bright, white, black] {
            let _ = DeleteObject(r.into());
        }
    }
}

/// Draws `base` inside `clip`, with the pills over it, writing each pixel once.
unsafe fn layer<'a>(hdc: HDC, rp: Rect, clip: HRGN, base: &Dib, pills: impl Iterator<Item = &'a Pill> + Clone) {
    unsafe {
        let rest = combine(clip, clip, RGN_COPY);
        for p in pills.clone() {
            let r = rgn(p.rect);
            CombineRgn(Some(rest), Some(rest), Some(r), RGN_DIFF);
            let _ = DeleteObject(r.into());
        }
        SelectClipRgn(hdc, Some(rest));
        let _ = BitBlt(hdc, rp.x, rp.y, rp.w, rp.h, Some(base.dc), rp.x, rp.y, SRCCOPY);
        let _ = DeleteObject(rest.into());
        for p in pills {
            let Some(dib) = &p.dib else { continue };
            let r = rgn(p.rect);
            CombineRgn(Some(r), Some(r), Some(clip), RGN_AND);
            SelectClipRgn(hdc, Some(r));
            let _ = BitBlt(hdc, p.rect.x, p.rect.y, p.rect.w, p.rect.h, Some(dib.dc), 0, 0, SRCCOPY);
            let _ = DeleteObject(r.into());
        }
        SelectClipRgn(hdc, None);
    }
}
