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
    Hotkey, HotkeyState, Kind, button, checkbox, dropdown, dropdown_detail, hint, hotkeys, keycaps, link, nested_row, note_row,
    preset_row, row, segmented, shortcuts, slider, subheading, swatch, text_field, toggle, toggle_row, tuned_slider,
};
#[cfg(feature = "test-rows")]
pub use controls::ROWS;
pub use motion::{Listed, anim, anim_ctx, ease, glide, mix, motion, reveal, reveal_with, snap};
pub use tokens::{
    BAR_H, CONTENT_X, FOOTER_H, Look, MEDIUM, MONO_MEDIUM, Palette, SEMIBOLD, SIDEBAR_W, WINDOW, medium, mono,
    mono_medium, palette, sans, semibold,
};
