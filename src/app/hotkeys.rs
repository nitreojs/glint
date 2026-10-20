use windows::Win32::UI::Input::KeyboardAndMouse::{HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey};

use super::{App, ClickTarget};
use crate::config::hotkeys::parse_hotkey;
use crate::platform::log::log;

pub(super) const HOTKEY_ID: i32 = 1;
pub(super) const SHOT_HOTKEY_ID: i32 = 2;

impl App {
    /// (Re-)registers one global hotkey; returns the combo it now holds.
    fn grab_hotkey(&mut self, id: i32, held: bool, combo: &str, what: &str) -> Option<String> {
        unsafe {
            if held {
                let _ = UnregisterHotKey(Some(self.hwnd), id);
            }
            let (mods, vk) = parse_hotkey(combo).expect("validated in Config::load");
            if RegisterHotKey(Some(self.hwnd), id, HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT, vk).is_ok() {
                return Some(combo.to_string());
            }
        }
        let text = format!("{combo} is already used by another app. Pick another {what} hotkey in Settings, or use the tray menu.");
        log(&format!("{what} hotkey {combo} is taken by another app"));
        self.notify("glint: hotkey unavailable", &text, true, ClickTarget::None);
        None
    }

    pub(super) fn register_hotkeys(&mut self) {
        let (record, shot) = (self.cfg.hotkey.clone(), self.cfg.screenshot_hotkey.clone());
        self.hotkey = self.grab_hotkey(HOTKEY_ID, self.hotkey.is_some(), &record, "recording");
        self.shot_hotkey = self.grab_hotkey(SHOT_HOTKEY_ID, self.shot_hotkey.is_some(), &shot, "screenshot");
    }

    /// Releases the hotkeys while the settings window records a new one.
    pub(super) fn pause_hotkeys(&mut self) {
        for (id, held) in [(HOTKEY_ID, self.hotkey.take()), (SHOT_HOTKEY_ID, self.shot_hotkey.take())] {
            if held.is_some() {
                unsafe {
                    let _ = UnregisterHotKey(Some(self.hwnd), id);
                }
            }
        }
    }
}
