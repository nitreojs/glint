use std::sync::LazyLock;

use eframe::egui::{Color32, FontFamily, FontId};

pub(crate) const WINDOW: [f32; 2] = [680.0, 600.0];
pub(crate) const BAR_H: f32 = 40.0;
pub(crate) const SIDEBAR_W: f32 = 168.0;
pub(crate) const FOOTER_H: f32 = 56.0;
pub(crate) const CONTENT_X: f32 = 28.0;
pub(super) const LABEL_W: f32 = 132.0;
pub(super) const LABEL_GAP: f32 = 16.0;
/// Window-control colours are Windows' own, in both themes.
pub(super) const CLOSE_HOVER: Color32 = Color32::from_rgb(0xC4, 0x2B, 0x1C);
pub(super) const CLOSE_PRESSED: Color32 = Color32::from_rgb(0xA7, 0x24, 0x17);
pub(super) const MARK: Color32 = Color32::from_rgb(0xE5, 0x40, 0x2F);

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Palette {
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
    pub acc: Color32,
    pub acch: Color32,
    pub accp: Color32,
    /// Accent for text (the selected section number).
    pub acct: Color32,
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

pub(crate) const DARK: Palette = Palette {
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
    acc: rgb(0xCF3D2C),
    acch: rgb(0xDB4A38),
    accp: rgb(0xB53424),
    acct: rgb(0xF06A55),
    warn: rgb(0xE3A33C),
    focus: rgb(0xEDEBE7),
};

pub(crate) const LIGHT: Palette = Palette {
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
    acc: rgb(0xC43A29),
    acch: rgb(0xB23322),
    accp: rgb(0x9E2D1E),
    acct: rgb(0xC43A29),
    warn: rgb(0x9A5F06),
    focus: rgb(0x1C1B19),
};

/// What every control needs to draw itself this frame.
pub(crate) struct Look {
    pub p: Palette,
    /// The last input was Tab / arrows: focus rings show only then.
    pub keyboard: bool,
}

// ---- type -------------------------------------------------------------------------------------

pub(crate) static MEDIUM: LazyLock<FontFamily> = LazyLock::new(|| FontFamily::Name("sans-medium".into()));
pub(crate) static SEMIBOLD: LazyLock<FontFamily> = LazyLock::new(|| FontFamily::Name("sans-semibold".into()));
pub(crate) static MONO_MEDIUM: LazyLock<FontFamily> = LazyLock::new(|| FontFamily::Name("mono-medium".into()));

pub(crate) fn sans(size: f32) -> FontId {
    FontId::proportional(size)
}
pub(crate) fn medium(size: f32) -> FontId {
    FontId::new(size, MEDIUM.clone())
}
pub(crate) fn semibold(size: f32) -> FontId {
    FontId::new(size, SEMIBOLD.clone())
}
pub(crate) fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}
pub(crate) fn mono_medium(size: f32) -> FontId {
    FontId::new(size, MONO_MEDIUM.clone())
}
