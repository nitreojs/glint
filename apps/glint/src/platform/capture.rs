use glint_core::geom::rect::Rect;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{BitBlt, CAPTUREBLT, GdiFlush, GetDC, ROP_CODE, ReleaseDC, SRCCOPY};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use glint_shell::afterglow::with_note_visible;
use glint_shell::gfx::Dib;
use glint_shell::monitors;

const BLACK: u32 = 0xFF00_0000;

/// Every monitor at once as opaque 0xFFRRGGBB rows, and the desktop area they cover.
pub(crate) fn virtual_screen() -> Result<(Vec<u32>, Rect), String> {
    let monitors: Vec<Rect> = monitors::enumerate()
        .map_err(|e| format!("can't list monitors: {}", e.message()))?
        .into_iter()
        .map(|o| o.rect)
        .collect();
    let area = bounds(&monitors).ok_or("no monitors found")?;
    let mut dib = Dib::new(area.w, area.h)?;
    grab(&mut dib, area, &monitors)?;
    Ok((dib.pixels().to_vec(), area))
}

/// The monitor under the mouse as opaque 0xFFRRGGBB rows, and where it is on the desktop.
pub(crate) fn monitor_under_mouse() -> Result<(Vec<u32>, Rect), String> {
    let monitors: Vec<Rect> = monitors::enumerate()
        .map_err(|e| format!("can't list monitors: {}", e.message()))?
        .into_iter()
        .map(|o| o.rect)
        .collect();
    let mut pt = POINT::default();
    unsafe { GetCursorPos(&mut pt) }.map_err(|e| format!("can't find the mouse: {e}"))?;
    let area = under(&monitors, pt.x, pt.y).ok_or("no monitors found")?;
    let mut dib = Dib::new(area.w, area.h)?;
    grab(&mut dib, area, &[area])?;
    Ok((dib.pixels().to_vec(), area))
}

/// The monitor containing (x, y), else the nearest one.
fn under(monitors: &[Rect], x: i32, y: i32) -> Option<Rect> {
    let gap = |m: &Rect| {
        let dx = (m.x - x).max(x - (m.right() - 1)).max(0) as i64;
        let dy = (m.y - y).max(y - (m.bottom() - 1)).max(0) as i64;
        dx * dx + dy * dy
    };
    monitors.iter().min_by_key(|m| gap(m)).copied()
}

/// Copies the desktop's `area` into `dib` (sized like `area`); pixels outside `monitors` become
/// black. afterglow's note, kept out of capture otherwise, is in the copy.
pub(crate) fn grab(dib: &mut Dib, area: Rect, monitors: &[Rect]) -> Result<(), String> {
    with_note_visible(|| unsafe {
        let screen = GetDC(None);
        let copied =
            BitBlt(dib.dc, 0, 0, area.w, area.h, Some(screen), area.x, area.y, ROP_CODE(SRCCOPY.0 | CAPTUREBLT.0));
        ReleaseDC(None, screen);
        let _ = GdiFlush();
        copied.map_err(|e| format!("BitBlt {}x{}: {e}", area.w, area.h))
    })?;
    black_outside(dib.pixels_mut(), area, monitors);
    Ok(())
}

fn bounds(rects: &[Rect]) -> Option<Rect> {
    let first = *rects.first()?;
    Some(rects.iter().fold(first, |b, r| {
        Rect::from_points(b.x.min(r.x), b.y.min(r.y), b.right().max(r.right()), b.bottom().max(r.bottom()))
    }))
}

/// Blackens the pixels of `area` that no monitor covers and makes every pixel opaque.
fn black_outside(pixels: &mut [u32], area: Rect, monitors: &[Rect]) {
    let mut spans = Vec::with_capacity(monitors.len());
    for (row, y) in pixels.chunks_exact_mut(area.w as usize).zip(area.y..) {
        spans.clear();
        spans.extend(monitors.iter().filter(|m| m.y <= y && y < m.bottom()).filter_map(|m| {
            let (x0, x1) = (m.x.max(area.x) - area.x, m.right().min(area.right()) - area.x);
            (x1 > x0).then_some((x0 as usize, x1 as usize))
        }));
        spans.sort_unstable();
        let mut x = 0;
        for &(x0, x1) in &spans {
            if x0 > x {
                row[x..x0].fill(BLACK);
            }
            for p in &mut row[x0.max(x)..x1.max(x)] {
                *p |= BLACK;
            }
            x = x.max(x1);
        }
        row[x..].fill(BLACK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaps_between_monitors_turn_black_and_the_rest_opaque() {
        let monitors =
            [Rect { x: 0, y: 0, w: 3, h: 2 }, Rect { x: -2, y: 1, w: 2, h: 2 }, Rect { x: 2, y: -1, w: 2, h: 1 }];
        let area = bounds(&monitors).unwrap();
        assert_eq!(area, Rect { x: -2, y: -1, w: 6, h: 4 });
        let mut pixels = vec![0x0012_3456; 24];
        black_outside(&mut pixels, area, &monitors);
        let (lit, dark) = (0xFF12_3456, BLACK);
        #[rustfmt::skip]
        let want = [
            dark, dark, dark, dark, lit,  lit,
            dark, dark, lit,  lit,  lit,  dark,
            lit,  lit,  lit,  lit,  lit,  dark,
            lit,  lit,  dark, dark, dark, dark,
        ];
        assert_eq!(pixels, want);
    }

    #[test]
    fn the_monitor_under_the_mouse_or_the_nearest() {
        let wide = Rect { x: 0, y: 0, w: 3440, h: 1440 };
        let left = Rect { x: -1920, y: 540, w: 1920, h: 1080 };
        let top = Rect { x: 365, y: -1440, w: 2560, h: 1440 };
        let all = [wide, left, top];
        assert_eq!(under(&all, 100, 100), Some(wide));
        assert_eq!(under(&all, 3439, 1439), Some(wide));
        assert_eq!(under(&all, -1, 600), Some(left));
        assert_eq!(under(&all, 400, -1), Some(top));
        assert_eq!(under(&all, -1, -1), Some(wide)); // in no monitor: the nearest
        assert_eq!(under(&all, -5000, 1000), Some(left));
        assert_eq!(under(&[], 0, 0), None);
    }
}
