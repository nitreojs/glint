//! The hotkeys the general section edits, recorded with [`glint_ui::capture::Recorder`] while
//! the tray app lets go of its own.

use glint_core::config::Config;
pub use glint_ui::capture::{CAPTURE_ARG, capture_keys};

use super::SettingsApp;
use crate::app::ipc::{self, WM_HOTKEY_PAUSE};

/// Asks the tray app to let go of its hotkeys (`true`) or take them back.
pub(super) fn release_tray_hotkeys(release: bool) {
    ipc::post_to_main(WM_HOTKEY_PAUSE, usize::from(release));
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum HotkeyField {
    Record,
    Screenshot,
    Fullscreen,
    Monitor,
    Pause,
}

impl HotkeyField {
    const ALL: [HotkeyField; 5] = [
        HotkeyField::Record,
        HotkeyField::Screenshot,
        HotkeyField::Fullscreen,
        HotkeyField::Monitor,
        HotkeyField::Pause,
    ];

    fn name(self) -> &'static str {
        match self {
            HotkeyField::Record => "record",
            HotkeyField::Screenshot => "screenshot",
            HotkeyField::Fullscreen => "all screens",
            HotkeyField::Monitor => "monitor",
            HotkeyField::Pause => "pause",
        }
    }

    pub(super) fn of(self, cfg: &mut Config) -> &mut String {
        match self {
            HotkeyField::Record => &mut cfg.hotkey,
            HotkeyField::Screenshot => &mut cfg.screenshot_hotkey,
            HotkeyField::Fullscreen => &mut cfg.fullscreen_hotkey,
            HotkeyField::Monitor => &mut cfg.monitor_hotkey,
            HotkeyField::Pause => &mut cfg.pause_hotkey,
        }
    }
}

impl SettingsApp {
    pub(super) fn start_listening(&mut self, field: HotkeyField) {
        self.listen_for = field;
        if let Err(e) = self.keys.start() {
            self.error = Some(e);
        }
    }

    pub(super) fn stop_listening(&mut self) {
        self.keys.stop();
    }

    pub(super) fn poll_capture(&mut self) {
        let field = self.listen_for;
        let mut cfg = self.cfg.clone();
        let others: Vec<(&str, String)> =
            HotkeyField::ALL.into_iter().filter(|&f| f != field).map(|f| (f.name(), f.of(&mut cfg).clone())).collect();
        let others: Vec<(&str, &str)> = others.iter().map(|(n, c)| (*n, c.as_str())).collect();
        if let Some(combo) = self.keys.poll(&others) {
            *field.of(&mut self.cfg) = combo;
        }
    }
}
