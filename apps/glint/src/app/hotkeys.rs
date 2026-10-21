use glint_core::config::Config;
use glint_core::config::hotkeys::parse_hotkey;
use glint_core::log::log;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey, VK_ESCAPE,
};

use super::{App, ClickTarget, State};

pub(super) const HOTKEY_ID: i32 = 1;
pub(super) const SHOT_HOTKEY_ID: i32 = 2;
pub(super) const FULL_HOTKEY_ID: i32 = 3;
pub(super) const PAUSE_HOTKEY_ID: i32 = 4;
pub(super) const ESC_HOTKEY_ID: i32 = 5;

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
        if matches!(self.state, State::Recording { .. }) {
            self.grab_pause_hotkey();
        }
    }

    pub(super) fn grab_pause_hotkey(&mut self) {
        let combo = self.cfg.pause_hotkey.clone();
        self.pause_hotkey = self.grab_hotkey(PAUSE_HOTKEY_ID, self.pause_hotkey.is_some(), &combo, "pause", false);
        if self.pause_hotkey.is_none() {
            log(&format!("pause hotkey {combo} is taken by another app"));
        }
    }

    pub(super) fn release_pause_hotkey(&mut self) {
        if self.pause_hotkey.take().is_some() {
            unsafe {
                let _ = UnregisterHotKey(Some(self.hwnd), PAUSE_HOTKEY_ID);
            }
        }
    }

    pub(super) fn grab_escape(&self) {
        unsafe {
            if RegisterHotKey(Some(self.hwnd), ESC_HOTKEY_ID, MOD_NOREPEAT, VK_ESCAPE.0 as u32).is_err() {
                log("esc is taken by another app; the record hotkey cancels the countdown");
            }
        }
    }

    pub(super) fn release_escape(&self) {
        unsafe {
            let _ = UnregisterHotKey(Some(self.hwnd), ESC_HOTKEY_ID);
        }
    }

    /// Releases the hotkeys while the settings window records a new one.
    pub(super) fn pause_hotkeys(&mut self) {
        let held = [
            (HOTKEY_ID, self.hotkey.take()),
            (SHOT_HOTKEY_ID, self.shot_hotkey.take()),
            (FULL_HOTKEY_ID, self.full_hotkey.take()),
            (PAUSE_HOTKEY_ID, self.pause_hotkey.take()),
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
