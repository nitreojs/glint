#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerKind {
    Color,
    Monochrome,
    MaskedColor,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PointerShape {
    pub kind: PointerKind,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Bitmaps {
    pub width: usize,
    pub height: usize,
    pub normal: Vec<u32>,
    pub invert: Option<Vec<u32>>,
}

impl PointerShape {
    pub fn size(&self) -> (i32, i32) {
        let h = if self.kind == PointerKind::Monochrome { self.height / 2 } else { self.height };
        (self.width as i32, h as i32)
    }

    pub fn bitmaps(&self) -> Option<Bitmaps> {
        let w = self.width as usize;
        let pitch = self.pitch as usize;
        let buf = &self.data;
        let (h, normal, invert) = match self.kind {
            PointerKind::Color => {
                let h = self.height as usize;
                let mut px = vec![0u32; w * h];
                for y in 0..h {
                    for x in 0..w {
                        let o = y * pitch + x * 4;
                        px[y * w + x] = u32::from_le_bytes(buf[o..o + 4].try_into().unwrap());
                    }
                }
                (h, px, None)
            }
            PointerKind::Monochrome => {
                // Top half is the AND mask, bottom half the XOR mask, 1 bit per pixel.
                let h = self.height as usize / 2;
                let bit = |row: usize, x: usize| buf[row * pitch + x / 8] & (0x80 >> (x % 8)) != 0;
                let mut normal = vec![0u32; w * h];
                let mut invert = vec![0u32; w * h];
                for y in 0..h {
                    for x in 0..w {
                        let (and, xor) = (bit(y, x), bit(y + h, x));
                        normal[y * w + x] = match (and, xor) {
                            (false, false) => 0xFF000000,
                            (false, true) => 0xFFFFFFFF,
                            _ => 0,
                        };
                        if and && xor {
                            invert[y * w + x] = 0xFFFFFFFF;
                        }
                    }
                }
                (h, normal, Some(invert))
            }
            PointerKind::MaskedColor => {
                // Alpha 0x00: replace with RGB; alpha 0xFF: XOR RGB with the screen.
                let h = self.height as usize;
                let mut normal = vec![0u32; w * h];
                let mut invert = vec![0u32; w * h];
                for y in 0..h {
                    for x in 0..w {
                        let o = y * pitch + x * 4;
                        let p = u32::from_le_bytes(buf[o..o + 4].try_into().unwrap());
                        if p >> 24 == 0 {
                            normal[y * w + x] = p | 0xFF000000;
                        } else {
                            invert[y * w + x] = p | 0xFF000000;
                        }
                    }
                }
                (h, normal, Some(invert))
            }
        };
        if w == 0 || h == 0 {
            return None;
        }
        let invert = invert.filter(|px| px.iter().any(|&p| p != 0));
        Some(Bitmaps { width: w, height: h, normal, invert })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monochrome_splits_the_masks() {
        let shape = PointerShape {
            kind: PointerKind::Monochrome,
            width: 4,
            height: 2,
            pitch: 1,
            data: vec![0b0011_0000, 0b0101_0000],
        };
        assert_eq!(shape.size(), (4, 1));
        let b = shape.bitmaps().unwrap();
        assert_eq!((b.width, b.height), (4, 1));
        assert_eq!(b.normal, vec![0xFF000000, 0xFFFFFFFF, 0, 0]);
        assert_eq!(b.invert, Some(vec![0, 0, 0, 0xFFFFFFFF]));
    }

    #[test]
    fn masked_color_and_color() {
        let px = |v: u32| v.to_le_bytes();
        let data = [px(0x00112233), px(0xFF445566), px(0), px(0)].concat();
        let masked = PointerShape { kind: PointerKind::MaskedColor, width: 2, height: 1, pitch: 16, data: data.clone() };
        let b = masked.bitmaps().unwrap();
        assert_eq!(b.normal, vec![0xFF112233, 0]);
        assert_eq!(b.invert, Some(vec![0, 0xFF445566]));
        let color = PointerShape { kind: PointerKind::Color, width: 2, height: 1, pitch: 16, data };
        let b = color.bitmaps().unwrap();
        assert_eq!(b.normal, vec![0x00112233, 0xFF445566]);
        assert_eq!(b.invert, None);
    }

    #[test]
    fn empty_shapes_draw_nothing() {
        let shape = PointerShape { kind: PointerKind::Monochrome, width: 8, height: 1, pitch: 1, data: vec![0] };
        assert_eq!(shape.bitmaps(), None);
        let shape = PointerShape { kind: PointerKind::Monochrome, width: 1, height: 2, pitch: 1, data: vec![0, 0] };
        assert_eq!(shape.bitmaps().unwrap().invert, None);
    }
}
