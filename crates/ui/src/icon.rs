use glint_core::config::color::contrast;

/// The recording dot stays red whatever the accent.
pub const RECORDING_RED: [u8; 3] = [0xE5, 0x40, 0x2F];

/// The glint mark as straight-alpha ARGB pixels: a disc in `color` with a white highlight in
/// the upper right ("a glint on a lens"); without the highlight it's the plain recording dot.
/// Geometry is snapped to whole pixels so it stays crisp at 16 px.
pub fn icon_pixels(size: i32, highlight: bool, color: [u8; 3]) -> Vec<u32> {
    let s = size as f32;
    let unit = |n: f32| (n * s / 16.0).round();
    let (c, r) = (s / 2.0, s / 2.0 - unit(1.0).max(1.0)); // 14 px disc on a 16 px grid
    // 3 px highlight whose box ends 2 px inside the disc's top-right corner.
    let hd = unit(3.0);
    let (hx, hy) = (s - unit(3.0) - hd / 2.0, unit(3.0) + hd / 2.0);
    let cover = |d: f32, r: f32| (r - d + 0.5).clamp(0.0, 1.0);
    let rim = rim_for(color);
    let rim_w = unit(1.0).max(1.0);
    let mut px = vec![0u32; (size * size) as usize];
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let a = cover(((fx - c).powi(2) + (fy - c).powi(2)).sqrt(), r);
            if a <= 0.0 {
                continue;
            }
            let w = if highlight { cover(((fx - hx).powi(2) + (fy - hy).powi(2)).sqrt(), hd / 2.0) } else { 0.0 };
            let inside = rim.map_or(1.0, |_| cover(((fx - c).powi(2) + (fy - c).powi(2)).sqrt(), r - rim_w));
            let base = |i: usize| {
                let edge = rim.map_or(color[i], |rim| rim[i]) as f32;
                edge + (color[i] as f32 - edge) * inside
            };
            let ch = |i: usize| (base(i) + (255.0 - base(i)) * w).round() as u32;
            px[(y * size + x) as usize] =
                ((a * 255.0).round() as u32) << 24 | ch(0) << 16 | ch(1) << 8 | ch(2);
        }
    }
    px
}

fn rim_for(color: [u8; 3]) -> Option<[u8; 3]> {
    // windows 11 taskbars are roughly #eeeeee (light) and #1c1c1c (dark)
    let toward = |to: f32| color.map(|c| (c as f32 + (to - c as f32) * 0.55).round() as u8);
    if contrast(color, [0xEE; 3]) < 1.6 {
        Some(toward(0.0))
    } else if contrast(color, [0x1C; 3]) < 1.6 {
        Some(toward(255.0))
    } else {
        None
    }
}

/// `icon_pixels` as RGBA bytes, for egui window icons.
pub fn icon_rgba(size: i32, color: [u8; 3]) -> Vec<u8> {
    let px = icon_pixels(size, true, color);
    px.into_iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mark_takes_the_accent_and_the_recording_dot_stays_red() {
        let size = 16;
        let at = |px: &[u32], x: i32, y: i32| px[(y * size + x) as usize];
        let blue = icon_pixels(size, true, [0x2F, 0x7B, 0xE5]);
        assert_eq!(at(&blue, 8, 8), 0xFF2F_7BE5);
        assert_eq!(at(&blue, 0, 0) >> 24, 0);
        let (hx, hy) = (11, 4);
        assert_eq!(at(&blue, hx, hy) & 0xFF_FFFF, 0xFF_FFFF);
        let rec = icon_pixels(size, false, RECORDING_RED);
        assert_eq!(at(&rec, 8, 8), 0xFFE5_402F);
        assert_eq!(at(&rec, hx, hy), 0xFFE5_402F);
        let rgba = icon_rgba(size, [0x2F, 0x7B, 0xE5]);
        assert_eq!(rgba[((8 * size + 8) * 4) as usize..][..4], [0x2F, 0x7B, 0xE5, 0xFF]);
    }

    #[test]
    fn too_light_or_dark_marks_get_a_rim() {
        let size = 16;
        let at = |px: &[u32], x: i32, y: i32| px[(y * size + x) as usize] & 0xFF_FFFF;
        let white = icon_pixels(size, true, [0xFF; 3]);
        assert_eq!(at(&white, 8, 8), 0xFF_FFFF);
        assert!(at(&white, 8, 1) & 0xFF < 0xC0);
        let black = icon_pixels(size, true, [0; 3]);
        assert_eq!(at(&black, 8, 8), 0);
        assert!(at(&black, 8, 1) & 0xFF > 0x40);
        let blue = icon_pixels(size, true, [0x2F, 0x7B, 0xE5]);
        assert_eq!(at(&blue, 8, 1), 0x2F_7BE5);
    }
}
