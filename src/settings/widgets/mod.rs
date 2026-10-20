//! Tokens and hand-drawn controls of the settings window (design 1a, "ledger"): warm graphite,
//! labels in a fixed column, red only on the mark, the selected section number, active
//! toggles and save.

mod chrome;
mod controls;
mod motion;
mod tokens;

pub(super) use chrome::{TitleAction, nav_item, title_bar};
pub(super) use controls::{
    HotkeyState, Kind, button, checkbox, hint, hotkey_field, link, row, segmented, slider, text_field, toggle,
};
pub(super) use motion::{anim, glide};
pub(super) use tokens::{
    BAR_H, CONTENT_X, DARK, FOOTER_H, LIGHT, Look, MEDIUM, MONO_MEDIUM, Palette, SEMIBOLD, SIDEBAR_W, WINDOW, medium,
    mono, mono_medium, sans, semibold,
};
