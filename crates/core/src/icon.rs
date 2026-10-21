use crate::config::color::{INK, WHITE, contrast};

/// The recording dot stays red whatever the accent.
pub const RECORDING_RED: [u8; 3] = [0xE5, 0x40, 0x2F];
/// Frames of the exe's icon, in pixels.
pub const EXE_SIZES: [i32; 7] = [16, 20, 24, 32, 48, 64, 256];

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

fn cover(d: f32, r: f32) -> f32 {
    (r - d + 0.5).clamp(0.0, 1.0)
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

/// Puts the "update ready" badge on a mark from [`icon_pixels`]: a dot in the lower right,
/// white or ink (whichever stands out more on `accent`) with a ring of the other, so it shows
/// on the accent and on light and dark taskbars alike.
pub fn add_badge(px: &mut [u32], size: i32, accent: [u8; 3]) {
    let s = size as f32;
    let unit = |n: f32| (n * s / 16.0).round();
    let r = unit(3.0).max(3.0);
    let ring = unit(1.0).max(1.0);
    let c = s - r;
    let (fill, edge) = if contrast(accent, WHITE) >= contrast(accent, INK) { (WHITE, INK) } else { (INK, WHITE) };
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 + 0.5 - c).powi(2) + (y as f32 + 0.5 - c).powi(2)).sqrt();
            let a = cover(d, r);
            if a <= 0.0 {
                continue;
            }
            let inner = cover(d, r - ring);
            let top = |i: usize| edge[i] as f32 + (fill[i] as f32 - edge[i] as f32) * inner;
            let p = &mut px[(y * size + x) as usize];
            let below_a = (*p >> 24) as f32 / 255.0;
            let out_a = a + below_a * (1.0 - a);
            let ch = |i: usize| {
                let below = (*p >> (16 - 8 * i) & 0xFF) as f32;
                ((top(i) * a + below * below_a * (1.0 - a)) / out_a).round() as u32
            };
            *p = ((out_a * 255.0).round() as u32) << 24 | ch(0) << 16 | ch(1) << 8 | ch(2);
        }
    }
}

/// `icon_pixels` as RGBA bytes, for egui window icons.
pub fn icon_rgba(size: i32, color: [u8; 3]) -> Vec<u8> {
    let px = icon_pixels(size, true, color);
    px.into_iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8]).collect()
}

/// The mark in `color` as a Windows `.ico` with a 32-bit frame per size in `sizes`.
pub fn ico(sizes: &[i32], color: [u8; 3]) -> Vec<u8> {
    let frames: Vec<Vec<u8>> = sizes.iter().map(|&size| ico_frame(size, &icon_pixels(size, true, color))).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(frames.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * frames.len();
    for (&size, frame) in sizes.iter().zip(&frames) {
        let side = if size >= 256 { 0 } else { size as u8 };
        out.extend_from_slice(&[side, side, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += frame.len();
    }
    for frame in frames {
        out.extend_from_slice(&frame);
    }
    out
}

/// One `.ico` image: a BITMAPINFOHEADER, bottom-up BGRA rows, then the 1-bit transparency mask.
fn ico_frame(size: i32, px: &[u32]) -> Vec<u8> {
    let side = size as usize;
    let mask_row = side.div_ceil(32) * 4;
    let mut out = Vec::with_capacity(40 + side * side * 4 + mask_row * side);
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&(size * 2).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&((side * side * 4 + mask_row * side) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    for row in px.chunks_exact(side).rev() {
        for p in row {
            out.extend_from_slice(&p.to_le_bytes());
        }
    }
    for row in px.chunks_exact(side).rev() {
        let mut bits = vec![0u8; mask_row];
        for (x, p) in row.iter().enumerate() {
            if p >> 24 == 0 {
                bits[x / 8] |= 0x80 >> (x % 8);
            }
        }
        out.extend_from_slice(&bits);
    }
    out
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

    #[test]
    fn the_update_badge_sits_lower_right_and_stands_out() {
        for size in [16, 20, 24, 32] {
            let at = |px: &[u32], x: i32, y: i32| px[(y * size + x) as usize];
            let plain = icon_pixels(size, true, RECORDING_RED);
            let mut badged = plain.clone();
            add_badge(&mut badged, size, RECORDING_RED);
            let r = (3.0 * size as f32 / 16.0).round() as i32;
            let (cx, cy) = (size - r, size - r);
            let middle = at(&badged, cx, cy);
            assert_eq!(middle >> 24, 0xFF, "{size}");
            assert!(middle & 0xFF_FFFF == 0xFF_FFFF || middle & 0xFF_FFFF == 0x1C_1B19, "{size}: {middle:08X}");
            assert_eq!(at(&badged, size / 2, size / 2), at(&plain, size / 2, size / 2));
            assert_eq!(at(&badged, size - 3, 2), at(&plain, size - 3, 2));
        }
        let mut light = icon_pixels(16, true, [0xF0, 0xE0, 0x60]);
        add_badge(&mut light, 16, [0xF0, 0xE0, 0x60]);
        assert_eq!(light[13 * 16 + 13] & 0xFF_FFFF, 0x1C_1B19);
    }

    #[test]
    fn the_exe_icon_holds_every_size() {
        let color = RECORDING_RED;
        let ico = ico(&EXE_SIZES, color);
        let u16_at = |i: usize| u16::from_le_bytes([ico[i], ico[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes(ico[i..i + 4].try_into().unwrap());
        assert_eq!((u16_at(0), u16_at(2), u16_at(4)), (0, 1, EXE_SIZES.len() as u16));
        let mut end = 6 + 16 * EXE_SIZES.len();
        for (i, &size) in EXE_SIZES.iter().enumerate() {
            let e = 6 + 16 * i;
            assert_eq!(ico[e] as i32, size % 256);
            assert_eq!((u16_at(e + 4), u16_at(e + 6)), (1, 32));
            let (len, at) = (u32_at(e + 8) as usize, u32_at(e + 12) as usize);
            assert_eq!(at, end);
            end += len;
            let side = size as usize;
            assert_eq!(len, 40 + side * side * 4 + side.div_ceil(32) * 4 * side);
            assert_eq!((u32_at(at), u32_at(at + 4), u32_at(at + 8)), (40, size as u32, 2 * size as u32));
            // the bottom row comes first; the centre pixel is the accent, the corner clear
            let px = icon_pixels(size, true, color);
            let row = at + 40 + (side - 1 - side / 2) * side * 4;
            assert_eq!(u32_at(row + side / 2 * 4), px[side / 2 * side + side / 2]);
            let mask = at + 40 + side * side * 4;
            assert_eq!(ico[mask] & 0x80, 0x80);
        }
        assert_eq!(end, ico.len());
    }
}
