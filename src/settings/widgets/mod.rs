//! Tokens and hand-drawn controls of the settings window (design 1a, "ledger"): warm graphite,
//! labels in a fixed column, the accent only on the mark, the selected section number, active
//! toggles and save.

mod chrome;
mod color;
mod controls;
mod motion;
mod tokens;

pub(crate) use color::{custom_swatch, hue_strip, sv_square};
pub(crate) use chrome::{TitleAction, nav_item, title_bar};
pub(crate) use controls::{
    HotkeyState, Kind, button, checkbox, dropdown, dropdown_detail, hint, hotkey_field, link, row, segmented, slider,
    swatch, text_field, toggle,
};
#[cfg(test)]
pub(crate) use controls::ROWS;
pub(crate) use motion::{anim, glide};
pub(crate) use tokens::{
    BAR_H, CONTENT_X, FOOTER_H, Look, MEDIUM, MONO_MEDIUM, Palette, SEMIBOLD, SIDEBAR_W, WINDOW, medium, mono,
    mono_medium, palette, sans, semibold,
};
