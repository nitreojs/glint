//! Programs' own icons, as pixels.

use glint_core::wide::wide;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDIBits,
    GetObjectW, HBITMAP, HDC,
};
use windows::Win32::UI::Shell::SHDefExtractIconW;
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};
use windows::core::PCWSTR;

/// Square, straight-alpha RGBA rows, top first.
pub struct Icon {
    pub size: u32,
    pub rgba: Vec<u8>,
}

/// The first icon in `exe`, `px` pixels square; None when it has none.
pub fn exe_icon(exe: &str, px: u32) -> Option<Icon> {
    let path = wide(exe);
    let mut icon = HICON::default();
    unsafe {
        SHDefExtractIconW(PCWSTR(path.as_ptr()), 0, 0, Some(&mut icon), None, px & 0xFFFF).ok().ok()?;
        if icon.is_invalid() {
            return None;
        }
        let pixels = pixels(icon);
        let _ = DestroyIcon(icon);
        pixels
    }
}

unsafe fn pixels(icon: HICON) -> Option<Icon> {
    unsafe {
        let mut info = ICONINFO::default();
        GetIconInfo(icon, &mut info).ok()?;
        let dc = CreateCompatibleDC(None);
        let color = bits(dc, info.hbmColor);
        let mask = bits(dc, info.hbmMask);
        let _ = DeleteDC(dc);
        for b in [info.hbmColor, info.hbmMask] {
            if !b.is_invalid() {
                let _ = DeleteObject(b.into());
            }
        }
        let (size, mut bgra) = color?;
        // icons without alpha say what's see-through in their mask: white is the background
        if bgra.chunks_exact(4).all(|p| p[3] == 0) {
            let (_, mask) = mask.filter(|(s, m)| *s == size && m.len() == bgra.len())?;
            for (p, m) in bgra.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
                p[3] = if m[0] == 0 { 255 } else { 0 };
            }
        }
        for p in bgra.chunks_exact_mut(4) {
            p.swap(0, 2);
        }
        Some(Icon { size, rgba: bgra })
    }
}

/// A square bitmap's pixels as 32-bit BGRA, top row first.
unsafe fn bits(dc: HDC, bitmap: HBITMAP) -> Option<(u32, Vec<u8>)> {
    if bitmap.is_invalid() {
        return None;
    }
    unsafe {
        let mut bm = BITMAP::default();
        if GetObjectW(bitmap.into(), size_of::<BITMAP>() as i32, Some(&mut bm as *mut BITMAP as _)) == 0 {
            return None;
        }
        let size = bm.bmWidth;
        if size <= 0 || size > 256 {
            return None;
        }
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut out = vec![0u8; (size * size * 4) as usize];
        let rows = GetDIBits(dc, bitmap, 0, size as u32, Some(out.as_mut_ptr().cast()), &mut bmi, DIB_RGB_COLORS);
        (rows == size).then_some((size as u32, out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_give_their_icon() {
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let icon = exe_icon(&format!(r"{system}\explorer.exe"), 32).expect("explorer has an icon");
        assert_eq!((icon.size, icon.rgba.len()), (32, 32 * 32 * 4));
        assert!(icon.rgba.chunks_exact(4).any(|p| p[3] == 255), "some of it is solid");
        assert!(icon.rgba.chunks_exact(4).any(|p| p[3] == 0), "and some see-through");
        assert!(exe_icon(r"C:\nowhere\missing.exe", 32).is_none());
    }
}
