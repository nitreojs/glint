use std::sync::LazyLock;

use eframe::egui::{Color32, FontFamily, FontId};
use glint_core::config::color::{contrast, text_on};

pub const WINDOW: [f32; 2] = [680.0, 640.0];
pub const BAR_H: f32 = 40.0;
pub const SIDEBAR_W: f32 = 168.0;
pub const FOOTER_H: f32 = 56.0;
pub const CONTENT_X: f32 = 28.0;
pub(super) const LABEL_W: f32 = 132.0;
pub(super) const LABEL_GAP: f32 = 16.0;
/// Window-control colours are Windows' own, in both themes.
pub(super) const CLOSE_HOVER: Color32 = Color32::from_rgb(0xC4, 0x2B, 0x1C);
pub(super) const CLOSE_PRESSED: Color32 = Color32::from_rgb(0xA7, 0x24, 0x17);

#[derive(Clone, Copy, PartialEq)]
pub struct Palette {
    /// Title bar and sidebar.
    pub side: Color32,
    pub surf: Color32,
    /// Inset fields, segmented tracks, off toggles.
    pub field: Color32,
    pub ctl: Color32,
    pub ctlh: Color32,
    pub ctlp: Color32,
    pub hov: Color32,
    pub prs: Color32,
    pub sel: Color32,
    pub line: Color32,
    pub line2: Color32,
    pub line3: Color32,
    pub t1: Color32,
    pub t2: Color32,
    pub t3: Color32,
    pub t4: Color32,
    /// The accent itself: the mark in the title bar.
    pub mark: Color32,
    pub acc: Color32,
    pub acch: Color32,
    pub accp: Color32,
    /// Accent for text (the selected section number).
    pub acct: Color32,
    pub on_acc: Color32,
    pub mark_ring: Color32,
    pub warn: Color32,
    pub focus: Color32,
}

const fn rgb(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// White at `a` alpha (premultiplied), usable in constants.
const fn white(a: u8) -> Color32 {
    Color32::from_rgba_premultiplied(a, a, a, a)
}

/// `rgb` moved toward white (`permille` > 0) or black (< 0).
fn tone(rgb: [u8; 3], permille: i32) -> Color32 {
    let to = if permille > 0 { 255 } else { 0 };
    let ch = |v: u8| (v as i32 + ((to - v as i32) * permille.abs() + 500) / 1000) as u8;
    Color32::from_rgb(ch(rgb[0]), ch(rgb[1]), ch(rgb[2]))
}

fn rgb3(c: Color32) -> [u8; 3] {
    [c.r(), c.g(), c.b()]
}

fn readable(accent: [u8; 3], permille: i32, on: Color32) -> Color32 {
    let mut p = permille;
    let mut c = tone(accent, p);
    while contrast(rgb3(c), rgb3(on)) < 3.0 && p.abs() < 1000 {
        p = (p + 50 * p.signum()).clamp(-1000, 1000);
        c = tone(accent, p);
    }
    c
}

/// The theme's colours with accent shades made from `accent`; the offsets keep the default red
/// within a few levels of its hand-tuned shades.
pub fn palette(light: bool, accent: [u8; 3]) -> Palette {
    let mark = Color32::from_rgb(accent[0], accent[1], accent[2]);
    let mut p = if light {
        Palette {
            side: rgb(0xF1F0ED),
            surf: rgb(0xFAF9F7),
            field: rgb(0xFFFFFF),
            ctl: rgb(0xFFFFFF),
            ctlh: rgb(0xF4F3F0),
            ctlp: rgb(0xECEBE7),
            hov: Color32::from_black_alpha(11),
            prs: Color32::from_black_alpha(20),
            sel: rgb(0xE5E3DF),
            line: Color32::from_black_alpha(20),
            line2: Color32::from_black_alpha(38),
            line3: Color32::from_black_alpha(77),
            t1: rgb(0x1C1B19),
            t2: rgb(0x4F4C47),
            t3: rgb(0x68655F),
            t4: rgb(0xA29E97),
            mark,
            acc: tone(accent, -145),
            acch: tone(accent, -220),
            accp: tone(accent, -310),
            acct: readable(accent, -145, rgb(0xE5E3DF)),
            on_acc: Color32::WHITE,
            mark_ring: Color32::TRANSPARENT,
            warn: rgb(0x9A5F06),
            focus: rgb(0x1C1B19),
        }
    } else {
        Palette {
            side: rgb(0x1A1A19),
            surf: rgb(0x222221),
            field: rgb(0x171716),
            ctl: rgb(0x2C2C2B),
            ctlh: rgb(0x353534),
            ctlp: rgb(0x262625),
            hov: white(11),
            prs: white(20),
            sel: rgb(0x2C2C2B),
            line: white(18),
            line2: white(33),
            line3: white(66),
            t1: rgb(0xEDEBE7),
            t2: rgb(0xB0ADA7),
            t3: rgb(0x8E8B85),
            t4: rgb(0x5C5A56),
            mark,
            acc: tone(accent, -90),
            acch: tone(accent, -40),
            accp: tone(accent, -210),
            acct: readable(accent, 200, rgb(0x2C2C2B)),
            on_acc: Color32::WHITE,
            mark_ring: Color32::TRANSPARENT,
            warn: rgb(0xE3A33C),
            focus: rgb(0xEDEBE7),
        }
    };
    let [r, g, b] = text_on(rgb3(p.acc));
    p.on_acc = Color32::from_rgb(r, g, b);
    if contrast(accent, rgb3(p.side)) < 1.6 {
        p.mark_ring = p.line3;
    }
    p
}

/// What every control needs to draw itself this frame.
pub struct Look {
    pub p: Palette,
    /// The last input was Tab / arrows: focus rings show only then.
    pub keyboard: bool,
}

// ---- type -------------------------------------------------------------------------------------

pub static MEDIUM: LazyLock<FontFamily> = LazyLock::new(|| FontFamily::Name("sans-medium".into()));
pub static SEMIBOLD: LazyLock<FontFamily> = LazyLock::new(|| FontFamily::Name("sans-semibold".into()));
pub static MONO_MEDIUM: LazyLock<FontFamily> = LazyLock::new(|| FontFamily::Name("mono-medium".into()));

pub fn sans(size: f32) -> FontId {
    FontId::proportional(size)
}
pub fn medium(size: f32) -> FontId {
    FontId::new(size, MEDIUM.clone())
}
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, SEMIBOLD.clone())
}
pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}
pub fn mono_medium(size: f32) -> FontId {
    FontId::new(size, MONO_MEDIUM.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_red_keeps_the_hand_tuned_shades() {
        let near = |a: Color32, hex: u32| {
            let b = rgb(hex);
            [a.r().abs_diff(b.r()), a.g().abs_diff(b.g()), a.b().abs_diff(b.b())].into_iter().all(|d| d <= 4)
        };
        let dark = palette(false, [0xE5, 0x40, 0x2F]);
        assert!(near(dark.acc, 0xCF3D2C) && near(dark.accp, 0xB53424) && near(dark.mark, 0xE5402F));
        let light = palette(true, [0xE5, 0x40, 0x2F]);
        assert!(near(light.acc, 0xC43A29) && near(light.acch, 0xB23322) && near(light.accp, 0x9E2D1E));
        let blue = palette(false, [0x2F, 0x7B, 0xE5]);
        assert_eq!(blue.mark, rgb(0x2F7BE5));
        assert!(blue.acc.b() > blue.acc.r() && blue.acct.b() > blue.acc.b());
        assert_eq!(blue.surf, dark.surf);
    }

    #[test]
    fn extreme_accents_keep_text_and_the_mark_readable() {
        for light in [false, true] {
            let red = palette(light, [0xE5, 0x40, 0x2F]);
            assert_eq!((red.on_acc, red.acct, red.mark_ring), (Color32::WHITE, tone([0xE5, 0x40, 0x2F], if light { -145 } else { 200 }), Color32::TRANSPARENT));
            for accent in [[0xFF, 0xFF, 0xFF], [0xFF, 0xF2, 0x40], [0, 0, 0], [0x10, 0x10, 0x30]] {
                let p = palette(light, accent);
                assert!(contrast(rgb3(p.acct), rgb3(p.sel)) >= 3.0, "{accent:?} light {light}");
                assert!(contrast(rgb3(p.on_acc), rgb3(p.acc)) >= 3.0, "{accent:?} light {light}");
            }
        }
        assert_eq!(palette(false, [0x10, 0x10, 0x10]).mark_ring, palette(false, [0; 3]).line3);
        assert_eq!(palette(true, [0xF8, 0xF8, 0xF8]).mark_ring, palette(true, [0; 3]).line3);
        assert_eq!(palette(true, [0x10, 0x10, 0x10]).mark_ring, Color32::TRANSPARENT);
        assert_eq!(palette(true, [0xFF, 0xF2, 0x40]).on_acc, Color32::from_rgb(0x1C, 0x1B, 0x19));
    }
}
