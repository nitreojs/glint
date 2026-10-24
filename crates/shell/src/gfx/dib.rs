use windows::Win32::Graphics::Gdi::*;

/// Top-down 32-bit DIB selected into its own memory DC.
pub struct Dib {
    pub dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    pub bits: *mut u32,
    pub w: i32,
    pub h: i32,
}

impl Dib {
    pub fn new(w: i32, h: i32) -> Result<Dib, String> {
        unsafe {
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let screen = GetDC(None);
            let mut bits = std::ptr::null_mut();
            let bmp = CreateDIBSection(Some(screen), &bmi, DIB_RGB_COLORS, &mut bits, None, 0);
            let dc = CreateCompatibleDC(Some(screen));
            ReleaseDC(None, screen);
            let bmp = bmp.map_err(|e| {
                let _ = DeleteDC(dc);
                format!("CreateDIBSection {w}x{h}: {e}")
            })?;
            let old = SelectObject(dc, bmp.into());
            Ok(Dib { dc, bmp, old, bits: bits as *mut u32, w, h })
        }
    }

    pub fn pixels(&self) -> &[u32] {
        unsafe { std::slice::from_raw_parts(self.bits, (self.w * self.h) as usize) }
    }

    pub fn pixel(&self, x: i32, y: i32) -> Option<u32> {
        let inside = (0..self.w).contains(&x) && (0..self.h).contains(&y);
        inside.then(|| self.pixels()[(y * self.w + x) as usize])
    }

    pub fn pixels_mut(&mut self) -> &mut [u32] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize) }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_are_read_where_they_are_and_nowhere_else() {
        let mut dib = Dib::new(4, 3).unwrap();
        dib.pixels_mut()[2 * 4 + 1] = 0x0012_3456;
        assert_eq!(dib.pixel(1, 2), Some(0x0012_3456));
        assert_eq!(dib.pixel(0, 0), Some(0));
        assert_eq!((dib.pixel(4, 0), dib.pixel(0, 3), dib.pixel(-1, 1)), (None, None, None));
    }
}
