//! The selection aids, headless: painted the way the picker paints them, over a made-up desktop.
//! `review_aids` (ignored) renders them to PNGs; `aids_frame_time` (ignored) times frames with
//! them on and off.

use std::time::Instant;

use glint_core::geom::rect::Rect;
use glint_media::rect::WinRect;
use glint_shell::gfx::{Dib, Painter, rgba};
use windows::Win32::Foundation::COLORREF;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Gdi::*;

use super::aids::{self, Aids, Kit, Shown};
use super::frozen::Frozen;
use super::paint::{paint_pick, selection_damage};
use super::pill::{Pill, render_pill};
use super::{RING, rgn};

const SCREEN: Rect = Rect { x: 0, y: 0, w: 3440, h: 1440 };
const ON: Aids = Aids { crosshair: true, magnifier: true };

/// A busy desktop: a light window, a dark one, a gradient and small text, so the aids have
/// light, dark and fine detail to show up on.
fn desktop(painter: &Painter, font: &IDWriteTextFormat) -> Dib {
    let mut dib = Dib::new(SCREEN.w, SCREEN.h).unwrap();
    for (i, p) in dib.pixels_mut().iter_mut().enumerate() {
        let (x, y) = ((i as i32 % SCREEN.w) as u32, (i as i32 / SCREEN.w) as u32);
        *p = ((x * 255 / 3440) << 16) | ((y * 255 / 1440) << 8) | 0x60;
    }
    painter
        .draw(&dib, |c| {
            c.fill(300.0, 200.0, 1300.0, 900.0, rgba(243, 243, 243, 1.0));
            c.fill(300.0, 200.0, 1300.0, 36.0, rgba(0, 95, 184, 1.0));
            c.fill(1800.0, 300.0, 1400.0, 1000.0, rgba(30, 30, 30, 1.0));
            c.fill(1800.0, 300.0, 1400.0, 36.0, rgba(60, 60, 60, 1.0));
            for row in 0..40 {
                let y = 260.0 + row as f32 * 20.0;
                c.text("the quick brown fox jumps over the lazy dog 0123456789", font, [330.0, y, 900.0, 20.0], rgba(20, 20, 20, 1.0));
                let y = 360.0 + row as f32 * 22.0;
                c.text("fn main() { println!(\"hello, glint\"); } // 0xC0FFEE", font, [1830.0, y, 900.0, 20.0], rgba(220, 220, 170, 1.0));
            }
            c.round_rect(700.0, 900.0, 400.0, 120.0, 12.0, rgba(229, 64, 47, 1.0));
        })
        .unwrap();
    unsafe {
        let _ = GdiFlush();
    }
    dib
}

struct Scene {
    painter: Painter,
    font: IDWriteTextFormat,
    frozen: Frozen,
    window: Dib,
    brushes: [HBRUSH; 2],
    kit: Kit,
}

impl Drop for Scene {
    fn drop(&mut self) {
        for b in self.brushes {
            unsafe {
                let _ = DeleteObject(b.into());
            }
        }
    }
}

impl Scene {
    fn new(scale: Option<f32>) -> Scene {
        let mut painter = Painter::new().unwrap();
        if let Some(s) = scale {
            painter.scale = s;
        }
        let font = painter.font(13.0).unwrap();
        let frozen = Frozen::new(desktop(&painter, &font)).unwrap();
        let brushes = unsafe { [CreateSolidBrush(COLORREF(0x00FF_FFFF)), CreateSolidBrush(COLORREF(0))] };
        let kit = Kit::new(&painter, &font, (SCREEN.w, SCREEN.h)).unwrap();
        Scene { window: Dib::new(SCREEN.w, SCREEN.h).unwrap(), painter, font, frozen, brushes, kit }
    }

    fn layout(&self, aids: Aids, at: (i32, i32), drag: Option<(Rect, (bool, bool))>) -> Shown {
        let clear = drag.map(|(sel, _)| sel.inflate(RING));
        let toward = drag.map_or((false, false), |d| d.1);
        aids::layout(aids, at, clear, toward, SCREEN, &[SCREEN], &self.kit.m)
    }

    fn label(&self, sel: Rect) -> Pill {
        let text = format!("{} × {}", sel.w, sel.h);
        let gap = self.painter.px(6.0) as i32;
        let h = self.painter.px(26.0) as i32;
        render_pill(&self.painter, &self.font, &text, &self.frozen.dim, sel.x - RING, sel.y - RING - gap - h).unwrap()
    }

    /// What the picker's WM_PAINT does for `rp` with no editor up: the screen with its holes,
    /// the label, the aids.
    fn paint(&self, rp: Rect, sel: Option<Rect>, label: Option<&Pill>, shown: &Shown) {
        let hdc = self.window.dc;
        let (f, monitors) = (&self.frozen, [SCREEN]);
        let mut holes: Vec<Rect> = label.map(|l| l.rect).into_iter().collect();
        holes.extend(shown.rects());
        unsafe {
            paint_pick(hdc, rp, (0, 0), f, &[], sel, &holes, &monitors, self.brushes);
            if let Some(l) = label
                && let Some(dib) = &l.dib
            {
                let saved = SaveDC(hdc);
                if let Some(c) = shown.loupe() {
                    ExcludeClipRect(hdc, c.x, c.y, c.right(), c.bottom());
                }
                let r = l.rect;
                let _ = BitBlt(hdc, r.x, r.y, r.w, r.h, Some(dib.dc), 0, 0, SRCCOPY);
                let _ = RestoreDC(hdc, saved);
            }
            let base = |dc: HDC, r: Rect, org: (i32, i32)| paint_pick(dc, r, org, f, &[], sel, &[], &monitors, self.brushes);
            aids::paint(hdc, rp, shown, &self.kit, &f.bright, &base, label, &[]);
            let _ = GdiFlush();
        }
    }

    /// Paints only `damage`, as BeginPaint clips a window's paint to its update region.
    fn paint_damage(&self, damage: HRGN, sel: Option<Rect>, label: Option<&Pill>, shown: &Shown) {
        unsafe {
            let mut b = windows::Win32::Foundation::RECT::default();
            if GetRgnBox(damage, &mut b) == NULLREGION {
                return;
            }
            let saved = SaveDC(self.window.dc);
            SelectClipRgn(self.window.dc, Some(damage));
            SetMetaRgn(self.window.dc);
            self.paint(Rect::from_win(&b), sel, label, shown);
            let _ = RestoreDC(self.window.dc, saved);
        }
    }

    fn save(&self, dir: &std::path::Path, name: &str, area: Rect) {
        let a = area.intersect(&SCREEN).unwrap();
        let px = self.window.pixels();
        let mut out = Vec::with_capacity((a.w * a.h) as usize);
        for y in a.y..a.bottom() {
            out.extend(px[(y * SCREEN.w + a.x) as usize..(y * SCREEN.w + a.right()) as usize].iter().map(|p| p | 0xFF00_0000));
        }
        let png = glint_media::image::encode(&out, a.w as u32, a.h as u32, false, glint_media::image::ImageFormat::Png, 100).unwrap();
        std::fs::write(dir.join(format!("{name}.png")), png).unwrap();
    }
}

/// Renders the aids to look at: `%TEMP%\glint-review\picker\*.png`.
#[test]
#[ignore = "writes review PNGs: cargo test --release -- --ignored review_aids"]
fn review_aids() {
    let dir = std::env::temp_dir().join("glint-review").join("picker");
    std::fs::create_dir_all(&dir).unwrap();
    for (scale, tag) in [(None, ""), (Some(1.5), "-150")] {
        let s = Scene::new(scale);
        let around = |p: (i32, i32)| Rect { x: p.0 - 450, y: p.1 - 300, w: 900, h: 600 };
        // hovering a window: it's highlighted with its size, the aids follow the cursor
        let window = Rect { x: 300, y: 200, w: 1300, h: 900 };
        let label = s.label(window);
        for (name, at) in [("centre", (523, 449)), ("corner", (3425, 1430)), ("right-edge", (3400, 700)), ("bottom-edge", (1200, 1420))] {
            let shown = s.layout(ON, at, None);
            s.paint(SCREEN, Some(window), Some(&label), &shown);
            s.save(&dir, &format!("hover-{name}{tag}"), around(at));
        }
        let at = (1840, 368);
        for (name, aids) in [("crosshair-only", Aids { crosshair: true, magnifier: false }), ("magnifier-only", Aids { crosshair: false, magnifier: true })] {
            let shown = s.layout(aids, at, None);
            s.paint(SCREEN, None, None, &shown);
            s.save(&dir, &format!("{name}{tag}"), around(at));
        }
        // dragging: the aids sit on the moving corner, clear of the selection
        for (name, sel, toward) in [
            ("drag-down-right", Rect { x: 640, y: 420, w: 700, h: 380 }, (false, false)),
            ("drag-up-left", Rect { x: 2000, y: 500, w: 600, h: 400 }, (true, true)),
        ] {
            let at = (if toward.0 { sel.x } else { sel.right() - 1 }, if toward.1 { sel.y } else { sel.bottom() - 1 });
            let shown = s.layout(ON, at, Some((sel, toward)));
            let label = s.label(sel);
            s.paint(SCREEN, Some(sel), Some(&label), &shown);
            s.save(&dir, &format!("{name}{tag}"), Rect { x: sel.x - 500, y: sel.y - 300, w: sel.w + 1000, h: sel.h + 600 });
        }
        if scale.is_none() {
            let shown = s.layout(ON, (905, 433), None);
            s.paint(SCREEN, Some(window), Some(&label), &shown);
            s.save(&dir, "whole-screen", SCREEN);
        }
    }
    println!("review PNGs in {}", dir.display());
}

/// Frame time at 3440x1440: the picker's work for one mouse move (aids layout, the damage, the
/// paint clipped to it) with the aids off and on, hovering (the highlight stays put) and dragging
/// (the selection follows the cursor).
#[test]
#[ignore = "timing: cargo test --release -- --ignored --nocapture aids_frame_time"]
fn aids_frame_time() {
    let s = Scene::new(None);
    let window = Rect { x: 300, y: 200, w: 1300, h: 900 };
    let label = s.label(window);
    let path: Vec<(i32, i32)> = (0..400).map(|i| (500 + i * 6, 300 + i * 2)).collect();
    let report = |what: &str, mut times: Vec<f64>| {
        times.sort_by(f64::total_cmp);
        let n = times.len();
        println!("{what:<26} median {:.3} ms, p90 {:.3} ms, max {:.3} ms", times[n / 2], times[n * 9 / 10], times[n - 1]);
    };
    s.paint(SCREEN, Some(window), Some(&label), &Shown::default());
    for aids in [Aids::default(), Aids { crosshair: true, magnifier: false }, Aids { crosshair: false, magnifier: true }, ON] {
        let name = |what: &str| format!("{what} {}{}", if aids.crosshair { "+cross" } else { "" }, if aids.magnifier { "+loupe" } else { "" });
        // hovering: only the aids move
        let mut shown = Shown::default();
        let mut times = Vec::new();
        for &p in &path {
            let t0 = Instant::now();
            let now = s.layout(aids, p, None);
            if now != shown {
                let damage = unsafe { CreateRectRgn(0, 0, 0, 0) };
                for r in shown.rects().chain(now.rects()) {
                    let one = rgn(r);
                    unsafe {
                        CombineRgn(Some(damage), Some(damage), Some(one), RGN_OR);
                        let _ = DeleteObject(one.into());
                    }
                }
                shown = now;
                s.paint_damage(damage, Some(window), Some(&label), &shown);
                unsafe {
                    let _ = DeleteObject(damage.into());
                }
            }
            times.push(t0.elapsed().as_secs_f64() * 1000.0);
        }
        report(&name("hover"), times);
        // dragging from (500, 300): the selection, its label and the aids move
        let start = (500, 300);
        let (mut sel, mut shown, mut was) = (None::<Rect>, Shown::default(), None::<Rect>);
        let mut times = Vec::new();
        for &p in &path[1..] {
            let t0 = Instant::now();
            let now_sel = Rect::from_points(start.0, start.1, p.0 + 1, p.1 + 1);
            let label = s.label(now_sel);
            let at = (now_sel.right() - 1, now_sel.bottom() - 1);
            let now = s.layout(aids, at, Some((now_sel, (false, false))));
            let damage = selection_damage((&[], sel), (&[], Some(now_sel)), &[SCREEN]);
            for r in shown.rects().chain(now.rects()).chain(was).chain([label.rect]) {
                let one = rgn(r);
                unsafe {
                    CombineRgn(Some(damage), Some(damage), Some(one), RGN_OR);
                    let _ = DeleteObject(one.into());
                }
            }
            (sel, shown, was) = (Some(now_sel), now, Some(label.rect));
            s.paint_damage(damage, sel, Some(&label), &shown);
            unsafe {
                let _ = DeleteObject(damage.into());
            }
            times.push(t0.elapsed().as_secs_f64() * 1000.0);
        }
        report(&name("drag"), times);
        let t0 = Instant::now();
        s.paint(SCREEN, Some(window), Some(&label), &s.layout(aids, (905, 433), None));
        println!("{:<26} {:.3} ms", name("whole screen"), t0.elapsed().as_secs_f64() * 1000.0);
    }
}
