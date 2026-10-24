//! The mouse cursor as a picture: what a screenshot keeps of it, and the arrow the editor adds.

use windows::Win32::Graphics::Gdi::{BITMAP, DeleteObject, GdiFlush, GetObjectW, HBITMAP};
use windows::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, DI_NORMAL, DrawIconEx, GetCursorInfo, GetIconInfo, HCURSOR, HICON, ICONINFO, IDC_ARROW,
    LoadCursorW,
};

use glint_shell::gfx::Dib;

/// A cursor's picture as premultiplied 0xAARRGGBB rows, and its hotspot.
#[derive(Debug, Default)]
pub(crate) struct CursorImage {
    pub w: i32,
    pub h: i32,
    pub hot: (i32, i32),
    pub px: Vec<u32>,
}

/// The cursor on screen now and where its hotspot is (desktop coordinates), while it shows.
pub(crate) fn current() -> Option<(CursorImage, (i32, i32))> {
    let mut info = CURSORINFO { cbSize: size_of::<CURSORINFO>() as u32, ..Default::default() };
    unsafe { GetCursorInfo(&mut info) }.ok()?;
    if info.flags.0 & CURSOR_SHOWING.0 == 0 || info.hCursor.is_invalid() {
        return None;
    }
    Some((image(info.hCursor)?, (info.ptScreenPos.x, info.ptScreenPos.y)))
}

/// The system's normal arrow, in the user's cursor scheme and size.
pub(crate) fn arrow() -> Option<CursorImage> {
    image(unsafe { LoadCursorW(None, IDC_ARROW) }.ok()?)
}

fn image(cursor: HCURSOR) -> Option<CursorImage> {
    let icon = HICON(cursor.0);
    unsafe {
        let mut info = ICONINFO::default();
        GetIconInfo(icon, &mut info).ok()?;
        // a monochrome cursor's mask holds the AND and the XOR halves, one above the other
        let size = bitmap_size(info.hbmColor).or_else(|| bitmap_size(info.hbmMask).map(|(w, h)| (w, h / 2)));
        for b in [info.hbmColor, info.hbmMask] {
            if !b.is_invalid() {
                let _ = DeleteObject(b.into());
            }
        }
        let (w, h) = size.filter(|&(w, h)| w > 0 && h > 0)?;
        // Drawn over black and over white: the difference is how much of the background shows.
        let over = |fill: u32| -> Option<Dib> {
            let mut dib = Dib::new(w, h).ok()?;
            dib.pixels_mut().fill(fill);
            DrawIconEx(dib.dc, 0, 0, icon, w, h, 0, None, DI_NORMAL).ok()?;
            let _ = GdiFlush();
            Some(dib)
        };
        let (black, white) = (over(0)?, over(0x00FF_FFFF)?);
        let px = black.pixels().iter().zip(white.pixels()).map(|(&b, &w)| premultiplied(b, w)).collect();
        Some(CursorImage { w, h, hot: (info.xHotspot as i32, info.yHotspot as i32), px })
    }
}

fn bitmap_size(b: HBITMAP) -> Option<(i32, i32)> {
    if b.is_invalid() {
        return None;
    }
    let mut bm = BITMAP::default();
    let n = unsafe { GetObjectW(b.into(), size_of::<BITMAP>() as i32, Some(&mut bm as *mut BITMAP as _)) };
    (n != 0).then_some((bm.bmWidth, bm.bmHeight))
}

/// One pixel from how it looks over black (`b`) and over white (`w`). Pixels that invert the screen
/// (white over black, black over white) come out white.
fn premultiplied(b: u32, w: u32) -> u32 {
    let ch = |v: u32, shift: u32| (v >> shift) & 0xFF;
    let shows = [0, 8, 16].into_iter().map(|s| ch(w, s).saturating_sub(ch(b, s))).max().unwrap_or(0);
    let a = 255 - shows;
    // a premultiplied channel never exceeds alpha
    let c = |shift: u32| ch(b, shift).min(a) << shift;
    (a << 24) | c(16) | c(8) | c(0)
}

/// Blends `img` over `px` (opaque 0xFFRRGGBB rows `w` wide) with its hotspot at `at`.
pub(crate) fn stamp(px: &mut [u32], w: i32, img: &CursorImage, at: (i32, i32)) {
    let h = px.len() as i32 / w.max(1);
    let (x0, y0) = (at.0 - img.hot.0, at.1 - img.hot.1);
    for y in 0..img.h {
        for x in 0..img.w {
            let (dx, dy) = (x0 + x, y0 + y);
            if dx < 0 || dy < 0 || dx >= w || dy >= h {
                continue;
            }
            let s = img.px[(y * img.w + x) as usize];
            let d = &mut px[(dy * w + dx) as usize];
            *d = over(s, *d);
        }
    }
}

/// Premultiplied `s` over opaque `d`.
fn over(s: u32, d: u32) -> u32 {
    let keep = 255 - (s >> 24);
    let ch = |shift: u32| (((s >> shift) & 0xFF) + (((d >> shift) & 0xFF) * keep + 127) / 255).min(255) << shift;
    0xFF00_0000 | ch(16) | ch(8) | ch(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_keep_how_much_of_the_background_they_cover() {
        assert_eq!(premultiplied(0, 0x00FF_FFFF), 0, "fully transparent");
        assert_eq!(premultiplied(0x0011_2233, 0x0011_2233), 0xFF11_2233, "opaque");
        // half-covering white: 0x80 over black, 0xFF over white
        assert_eq!(premultiplied(0x0080_8080, 0x00FF_FFFF), 0x8080_8080);
        assert_eq!(premultiplied(0x00FF_FFFF, 0), 0xFFFF_FFFF, "inverting pixels show white");
    }

    #[test]
    fn stamping_blends_at_the_hotspot_and_clips_at_the_edges() {
        let img = CursorImage { w: 2, h: 2, hot: (1, 1), px: vec![0xFFFF_0000, 0x8080_8080, 0, 0xFF00_00FF] };
        let mut px = vec![0xFF00_0000; 9];
        stamp(&mut px, 3, &img, (1, 1));
        assert_eq!(px[0], 0xFFFF_0000);
        assert_eq!(px[1], 0xFF80_8080);
        assert_eq!(px[3], 0xFF00_0000);
        assert_eq!(px[4], 0xFF00_00FF);
        let mut corner = vec![0xFFFF_FFFF; 4];
        stamp(&mut corner, 2, &img, (0, 0));
        assert_eq!(corner, [0xFF00_00FF, 0xFFFF_FFFF, 0xFFFF_FFFF, 0xFFFF_FFFF]);
    }

    #[test]
    fn the_arrow_is_solid_where_it_draws_and_see_through_around_it() {
        let arrow = arrow().expect("the system arrow");
        assert_eq!(arrow.px.len(), (arrow.w * arrow.h) as usize);
        let solid = arrow.px.iter().filter(|&&p| p >> 24 == 0xFF).count();
        let clear = arrow.px.iter().filter(|&&p| p >> 24 == 0).count();
        // whatever the cursor scheme, an arrow covers some of its box and leaves most of it
        assert!(solid > 20 && clear > arrow.px.len() / 3, "{solid} solid, {clear} clear of {}", arrow.px.len());
        assert!((0..arrow.w).contains(&arrow.hot.0) && (0..arrow.h).contains(&arrow.hot.1));
    }
}
