pub(crate) const WHITE: [u8; 3] = [0xFF, 0xFF, 0xFF];
pub(crate) const INK: [u8; 3] = [0x1C, 0x1B, 0x19];

pub fn rgb_to_hsv(rgb: [u8; 3]) -> [f32; 3] {
    let [r, g, b] = rgb.map(|c| c as f32 / 255.0);
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    let sextant = if d == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    [sextant / 6.0, if max == 0.0 { 0.0 } else { d / max }, max]
}

pub fn hsv_to_rgb([h, s, v]: [f32; 3]) -> [u8; 3] {
    let h = h.rem_euclid(1.0) * 6.0;
    let channel = |n: f32| {
        let k = (n + h) % 6.0;
        v - v * s * k.min(4.0 - k).clamp(0.0, 1.0)
    };
    [channel(5.0), channel(3.0), channel(1.0)].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)
}

pub fn rgb_to_hsv_near(rgb: [u8; 3], [hue, saturation, _]: [f32; 3]) -> [f32; 3] {
    let [h, s, v] = rgb_to_hsv(rgb);
    if v == 0.0 {
        [hue, saturation, 0.0]
    } else if s == 0.0 {
        [hue, 0.0, v]
    } else {
        [h, s, v]
    }
}

// wcag 2.x relative luminance and contrast ratio
pub(crate) fn luminance(rgb: [u8; 3]) -> f32 {
    let linear = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * linear(rgb[0]) + 0.7152 * linear(rgb[1]) + 0.0722 * linear(rgb[2])
}

pub fn contrast(a: [u8; 3], b: [u8; 3]) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

pub fn text_on(fill: [u8; 3]) -> [u8; 3] {
    // wcag asks 3:1 for large text and ui components
    let white = contrast(WHITE, fill);
    if white >= 3.0 || white >= contrast(INK, fill) { WHITE } else { INK }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_round_trips_and_hits_the_primaries() {
        assert_eq!(hsv_to_rgb([0.0, 1.0, 1.0]), [255, 0, 0]);
        assert_eq!(hsv_to_rgb([1.0 / 3.0, 1.0, 1.0]), [0, 255, 0]);
        assert_eq!(hsv_to_rgb([2.0 / 3.0, 1.0, 1.0]), [0, 0, 255]);
        assert_eq!(hsv_to_rgb([1.0, 1.0, 1.0]), [255, 0, 0]);
        assert_eq!(hsv_to_rgb([0.5, 0.0, 0.5]), [128, 128, 128]);
        assert_eq!(hsv_to_rgb([0.3, 0.7, 0.0]), [0, 0, 0]);
        assert_eq!(rgb_to_hsv([255, 255, 0]), [1.0 / 6.0, 1.0, 1.0]);
        assert_eq!(rgb_to_hsv([0, 0, 0]), [0.0, 0.0, 0.0]);
        assert_eq!(rgb_to_hsv([255, 0, 255])[0], 5.0 / 6.0);
        for r in (0..=255).step_by(15) {
            for g in (0..=255).step_by(17) {
                for b in (0..=255).step_by(51) {
                    assert_eq!(hsv_to_rgb(rgb_to_hsv([r, g, b])), [r, g, b]);
                }
            }
        }
        assert_eq!(rgb_to_hsv_near([0, 0, 0], [0.4, 0.8, 0.9]), [0.4, 0.8, 0.0]);
        assert_eq!(rgb_to_hsv_near([200, 200, 200], [0.4, 0.8, 0.9])[..2], [0.4, 0.0]);
        assert_eq!(rgb_to_hsv_near([0, 0, 255], [0.4, 0.8, 0.9]), [2.0 / 3.0, 1.0, 1.0]);
    }

    #[test]
    fn text_on_the_accent_stays_readable() {
        assert!((contrast(WHITE, [0, 0, 0]) - 21.0).abs() < 0.01);
        assert!((contrast([0x77; 3], WHITE) - 4.48).abs() < 0.01);
        assert_eq!(text_on([0xCF, 0x3D, 0x2C]), WHITE);
        assert_eq!(text_on([0x2B, 0x70, 0xD1]), WHITE);
        assert_eq!(text_on([0, 0, 0]), WHITE);
        assert_eq!(text_on([0xFF, 0xD4, 0x00]), INK);
        assert_eq!(text_on([0xF0, 0xF0, 0xF0]), INK);
        assert_eq!(text_on([0x7F, 0xE0, 0xC0]), INK);
    }
}
