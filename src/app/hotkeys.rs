use windows::Win32::UI::Input::KeyboardAndMouse::{HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey};

use super::{App, ClickTarget};
use crate::config::Config;
use crate::config::hotkeys::parse_hotkey;
use crate::platform::log::log;

pub(super) const HOTKEY_ID: i32 = 1;
pub(super) const SHOT_HOTKEY_ID: i32 = 2;
pub(super) const FULL_HOTKEY_ID: i32 = 3;

impl App {
    /// (Re-)registers one global hotkey; returns the combo it now holds.
    fn grab_hotkey(&mut self, id: i32, held: bool, combo: &str, what: &str, announce: bool) -> Option<String> {
        unsafe {
            if held {
                let _ = UnregisterHotKey(Some(self.hwnd), id);
            }
            let (mods, vk) = parse_hotkey(combo).expect("validated in Config::load");
            if RegisterHotKey(Some(self.hwnd), id, HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT, vk).is_ok() {
                return Some(combo.to_string());
            }
        }
        if announce {
            let text = format!(
                "{combo} is already used by another app. Pick another {what} hotkey in Settings, or use the tray menu."
            );
            log(&format!("{what} hotkey {combo} is taken by another app"));
            self.notify("glint: hotkey unavailable", &text, true, ClickTarget::None);
        }
        None
    }

    /// Registers the configured hotkeys; a taken one is announced unless `before` had the same combo.
    pub(super) fn register_hotkeys(&mut self, before: Option<&Config>) {
        let cfg = self.cfg.clone();
        let changed = |pick: fn(&Config) -> &String| before.is_none_or(|b| pick(b) != pick(&cfg));
        let announce = [changed(|c| &c.hotkey), changed(|c| &c.screenshot_hotkey), changed(|c| &c.fullscreen_hotkey)];
        self.hotkey = self.grab_hotkey(HOTKEY_ID, self.hotkey.is_some(), &cfg.hotkey, "recording", announce[0]);
        let (held, combo) = (self.shot_hotkey.is_some(), &cfg.screenshot_hotkey);
        self.shot_hotkey = self.grab_hotkey(SHOT_HOTKEY_ID, held, combo, "screenshot", announce[1]);
        let (held, combo) = (self.full_hotkey.is_some(), &cfg.fullscreen_hotkey);
        self.full_hotkey = self.grab_hotkey(FULL_HOTKEY_ID, held, combo, "all screens", announce[2]);
    }

    /// Releases the hotkeys while the settings window records a new one.
    pub(super) fn pause_hotkeys(&mut self) {
        let held = [
            (HOTKEY_ID, self.hotkey.take()),
            (SHOT_HOTKEY_ID, self.shot_hotkey.take()),
            (FULL_HOTKEY_ID, self.full_hotkey.take()),
        ];
        for (id, held) in held {
            if held.is_some() {
                unsafe {
                    let _ = UnregisterHotKey(Some(self.hwnd), id);
                }
            }
        }
    }
}
