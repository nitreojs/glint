use std::time::Instant;

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::*;

use super::pill::Pill;
use super::{RING, Screen, State, rgn};
use crate::platform::log::log;
use crate::platform::rect::{Rect, ring};

impl State {
    /// Replaces the selection and, if `label` is Some, the label; invalidates exactly the
    /// pixels that change.
    pub(super) fn set_selection(&mut self, sel: Option<Rect>, label: Option<Option<Pill>>) {
        let (old, new) = (self.sel, sel);
        if old == new && label.is_none() {
            return;
        }
        self.sel = sel;
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
            // Pixels that switch between dimmed and bright.
            let empty = Rect::default();
            let (a, b) = (rgn(old.unwrap_or(empty)), rgn(new.unwrap_or(empty)));
            let xor = CreateRectRgn(0, 0, 0, 0);
            CombineRgn(Some(xor), Some(a), Some(b), RGN_XOR);
            let _ = DeleteObject(a.into());
            let _ = DeleteObject(b.into());
            add(xor);
            for s in [old, new].into_iter().flatten() {
                let r = rgn(s.inflate(RING));
                let inner = rgn(s);
                CombineRgn(Some(r), Some(r), Some(inner), RGN_DIFF);
                let _ = DeleteObject(inner.into());
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
            return live.sync(self.sel, self.label.as_ref(), &self.painter, &self.font);
        }
        // Rendering can change what the editor covers (a tooltip appears): paint that too.
        if let (Some(ed), Screen::Frozen(f)) = (self.edit.as_mut(), &self.screen) {
            let before = ed.extent();
            if let Err(e) = ed.render(&f.bright, &self.painter, &self.font) {
                log(&format!("editor render failed: {e}"));
            }
            let after = ed.extent();
            if after != before {
                self.invalidate(&after);
            }
        }
        let Screen::Frozen(f) = &self.screen else { return };
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(self.hwnd, &mut ps);
            self.last_paint.set(Instant::now());
            let rp = Rect::from_win(&ps.rcPaint);
            let label = self.label.as_ref();

            // Dimmed screen around everything else.
            let saved = SaveDC(hdc);
            let mut holes: Vec<Rect> = self.sel.iter().map(|s| s.inflate(RING)).collect();
            holes.extend(label.map(|l| l.rect));
            for h in &holes {
                ExcludeClipRect(hdc, h.x, h.y, h.right(), h.bottom());
            }
            let _ = BitBlt(hdc, rp.x, rp.y, rp.w, rp.h, Some(f.dim.dc), rp.x, rp.y, SRCCOPY);
            let _ = RestoreDC(hdc, saved);

            if let Some(ed) = &self.edit {
                if let Err(e) = ed.paint(hdc, &self.painter, &self.font, self.white, self.black) {
                    log(&format!("editor paint failed: {e}"));
                }
            } else if let Some(s) = self.sel {
                let saved = SaveDC(hdc);
                if let Some(l) = label {
                    ExcludeClipRect(hdc, l.rect.x, l.rect.y, l.rect.right(), l.rect.bottom());
                }
                if let Some(i) = s.intersect(&rp) {
                    let _ = BitBlt(hdc, i.x, i.y, i.w, i.h, Some(f.bright.dc), i.x, i.y, SRCCOPY);
                }
                for r in ring(s.inflate(1), s) {
                    FillRect(hdc, &r, self.white);
                }
                for r in ring(s.inflate(2), s.inflate(1)) {
                    FillRect(hdc, &r, self.black);
                }
                IntersectClipRect(hdc, s.x, s.y, s.right(), s.bottom());
                for h in f.hints.iter().filter(|h| h.rect.intersect(&s).is_some()) {
                    if let Some(dib) = &h.dib {
                        let r = h.rect;
                        let _ = BitBlt(hdc, r.x, r.y, r.w, r.h, Some(dib.dc), 0, 0, SRCCOPY);
                    }
                }
                let _ = RestoreDC(hdc, saved);
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
