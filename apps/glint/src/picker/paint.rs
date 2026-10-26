use std::time::Instant;

use glint_core::geom::outline::{inward, outer};
use glint_core::geom::rect::Rect;
use glint_core::log::log;
use glint_media::rect::WinRect;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::*;

use super::frozen::Frozen;
use super::pill::Pill;
use super::{RING, Screen, State, rgn};
use glint_shell::gfx::Dib;

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
            let monitors = self.monitors();
            let [a, a_white, a_black] = regions(&old.0, old.1, &monitors);
            let [b, b_white, b_black] = regions(&self.lit, self.sel, &monitors);
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
        let monitors = self.monitors();
        if let Screen::Live(live) = &mut self.screen {
            unsafe {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(self.hwnd, &mut ps);
                self.last_paint.set(Instant::now());
                FillRect(hdc, &ps.rcPaint, self.black);
                let _ = EndPaint(self.hwnd, &ps);
            }
            return live.sync(&self.lit, self.sel, self.label.as_ref(), &monitors, &self.painter, &self.font);
        }
        // Rendering can change what the editor covers (a tooltip appears): paint that too.
        if let (Some(ed), Screen::Frozen(f)) = (self.edit.as_mut(), &self.screen) {
            let moved = if self.region {
                Ok(ed.render_chrome(&self.painter, &self.font))
            } else {
                ed.render(&f.bright, &self.painter, &self.font)
            };
            match moved {
                Ok(moved) => self.invalidate(&moved),
                Err(e) => log(&format!("editor render failed: {e}")),
            }
        }
        let f = match (&self.flat, &self.screen) {
            (Some(flat), _) => flat,
            (None, Screen::Frozen(f)) => f,
            (None, Screen::Live(_)) => return,
        };
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(self.hwnd, &mut ps);
            self.last_paint.set(Instant::now());
            let rp = Rect::from_win(&ps.rcPaint);
            let label = self.label.as_ref();
            // Draw-first's region tool: the toolbar and its pills stay over the picker.
            let chrome: Vec<Rect> = match &self.edit {
                Some(ed) if self.region => ed.chrome().into_iter().filter_map(|c| c.intersect(&rp)).collect(),
                _ => Vec::new(),
            };

            match self.edit.as_mut() {
                Some(ed) if !self.region => {
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
                }
                _ => {
                    let brushes = [self.white, self.black];
                    let mut holes = chrome.clone();
                    holes.extend(label.map(|l| l.rect));
                    paint_pick(hdc, rp, (0, 0), f, &self.lit, self.sel, &holes, &monitors, brushes);
                    if let Some(ed) = &self.edit {
                        for &c in &chrome {
                            // composed off-screen so each pixel is written once
                            let Ok(back) = Dib::new(c.w, c.h) else { continue };
                            paint_pick(back.dc, c, (c.x, c.y), f, &self.lit, self.sel, &[], &monitors, brushes);
                            if let Some(l) = label.filter(|l| l.rect.intersect(&c).is_some())
                                && let Some(dib) = &l.dib
                            {
                                let r = l.rect.offset(-c.x, -c.y);
                                let _ = BitBlt(back.dc, r.x, r.y, r.w, r.h, Some(dib.dc), 0, 0, SRCCOPY);
                            }
                            if let Err(e) = ed.draw_chrome(back.dc, c, c, &self.painter, &self.font) {
                                log(&format!("toolbar paint failed: {e}"));
                            }
                            let _ = GdiFlush();
                            let _ = BitBlt(hdc, c.x, c.y, c.w, c.h, Some(back.dc), 0, 0, SRCCOPY);
                        }
                    }
                }
            }
            if let Some(l) = label
                && let Some(dib) = &l.dib
            {
                let r = l.rect;
                let saved = SaveDC(hdc);
                for c in &chrome {
                    ExcludeClipRect(hdc, c.x, c.y, c.right(), c.bottom());
                }
                let _ = BitBlt(hdc, r.x, r.y, r.w, r.h, Some(dib.dc), 0, 0, SRCCOPY);
                let _ = RestoreDC(hdc, saved);
            }
            let _ = EndPaint(self.hwnd, &ps);
        }
        // a paint can leave font list rows for the next frame
        if let Some(ed) = &self.edit {
            super::schedule_tick(self.hwnd, &self.tick_due, ed);
        }
    }
}

/// The union of `rects`, each grown by `by`.
fn union(rects: &[Rect], by: i32) -> HRGN {
    unsafe {
        let all = CreateRectRgn(0, 0, 0, 0);
        for r in rects.iter().map(|r| r.inflate(by)).filter(|r| r.w > 0 && r.h > 0) {
            let one = rgn(r);
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
/// chosen rects share one outline; the selection's ring stays whole over them. Rings go around
/// the rects, or just inside them on sides at the edge of their `monitors`.
fn regions(chosen: &[Rect], sel: Option<Rect>, monitors: &[Rect]) -> [HRGN; 3] {
    let outline = |r: &Rect| outer(*r, inward(*r, monitors, RING), RING);
    let sel: Vec<Rect> = sel.iter().map(outline).collect();
    let chosen: Vec<Rect> = chosen.iter().map(outline).collect();
    let s = [union(&sel, -RING), union(&sel, 1 - RING), union(&sel, 0)];
    let c = [union(&chosen, -RING), union(&chosen, 1 - RING), union(&chosen, 0)];
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
/// `holes` left out. Draws client `rp` at `rp - org` on `hdc`.
#[allow(clippy::too_many_arguments)]
unsafe fn paint_pick(
    hdc: HDC,
    rp: Rect,
    org: (i32, i32),
    f: &Frozen,
    chosen: &[Rect],
    sel: Option<Rect>,
    holes: &[Rect],
    monitors: &[Rect],
    brushes: [HBRUSH; 2],
) {
    unsafe {
        let free = rgn(rp);
        for &h in holes {
            let h = rgn(h);
            CombineRgn(Some(free), Some(free), Some(h), RGN_DIFF);
            let _ = DeleteObject(h.into());
        }
        let [bright, white, black] = regions(chosen, sel, monitors);
        let dim = combine(free, bright, RGN_DIFF);
        for r in [white, black] {
            CombineRgn(Some(dim), Some(dim), Some(r), RGN_DIFF);
        }
        for r in [bright, white, black] {
            CombineRgn(Some(r), Some(r), Some(free), RGN_AND);
        }
        for r in [dim, bright, white, black] {
            OffsetRgn(r, -org.0, -org.1);
        }
        layer(hdc, rp, org, dim, &f.dim, f.hints.iter().map(|h| &h.0));
        layer(hdc, rp, org, bright, &f.bright, f.hints.iter().map(|h| &h.1));
        for (ring, brush) in [white, black].into_iter().zip(brushes) {
            let _ = FillRgn(hdc, ring, brush);
        }
        for r in [free, dim, bright, white, black] {
            let _ = DeleteObject(r.into());
        }
    }
}

/// Draws `base` inside `clip` (device coordinates, client minus `org`), with the pills over it,
/// writing each pixel once.
unsafe fn layer<'a>(
    hdc: HDC,
    rp: Rect,
    org: (i32, i32),
    clip: HRGN,
    base: &Dib,
    pills: impl Iterator<Item = &'a Pill> + Clone,
) {
    unsafe {
        let rest = combine(clip, clip, RGN_COPY);
        for p in pills.clone() {
            let r = rgn(p.rect.offset(-org.0, -org.1));
            CombineRgn(Some(rest), Some(rest), Some(r), RGN_DIFF);
            let _ = DeleteObject(r.into());
        }
        SelectClipRgn(hdc, Some(rest));
        let _ = BitBlt(hdc, rp.x - org.0, rp.y - org.1, rp.w, rp.h, Some(base.dc), rp.x, rp.y, SRCCOPY);
        let _ = DeleteObject(rest.into());
        for p in pills {
            let Some(dib) = &p.dib else { continue };
            let at = p.rect.offset(-org.0, -org.1);
            let r = rgn(at);
            CombineRgn(Some(r), Some(r), Some(clip), RGN_AND);
            SelectClipRgn(hdc, Some(r));
            let _ = BitBlt(hdc, at.x, at.y, at.w, at.h, Some(dib.dc), 0, 0, SRCCOPY);
            let _ = DeleteObject(r.into());
        }
        SelectClipRgn(hdc, None);
    }
}
