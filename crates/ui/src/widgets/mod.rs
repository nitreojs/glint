//! Tokens and hand-drawn controls of the settings window (design 1a, "ledger"): warm graphite,
//! labels in a fixed column, the accent only on the mark, the selected section number, active
//! toggles and save.

mod chrome;
mod color;
mod controls;
pub mod motion;
mod tokens;

pub use color::{custom_swatch, hue_strip, sv_square};
pub use chrome::{TitleAction, nav_item, title_bar};
pub use controls::{
    HotkeyState, Kind, button, checkbox, dropdown, dropdown_detail, group, hint, hotkey_field, keycaps, link, row,
    segmented, slider, swatch, text_field, toggle,
};
#[cfg(feature = "test-rows")]
pub use controls::ROWS;
pub use motion::{anim, anim_ctx, ease, glide, mix, motion};
pub use tokens::{
    BAR_H, CONTENT_X, FOOTER_H, Look, MEDIUM, MONO_MEDIUM, Palette, SEMIBOLD, SIDEBAR_W, WINDOW, medium, mono,
    mono_medium, palette, sans, semibold,
};
