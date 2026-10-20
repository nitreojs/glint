use eframe::egui::Ui;

use crate::settings::SettingsApp;
use crate::settings::hotkey_capture::{HotkeyField, held_names, key_names};
use crate::settings::widgets::{self, HotkeyState, Look, hint, row};

impl SettingsApp {
    /// One hotkey row: the field, and the reason the last combo can't be used.
    fn hotkey_row(&mut self, ui: &mut Ui, look: &Look, label: &str, field: HotkeyField) {
        let keys = key_names(field.of(&mut self.cfg));
        let held = held_names();
        let mine = self.listening.is_some() && self.listen_for == field;
        let state = match (mine, &self.hotkey_error) {
            (true, Some((keys, _))) => HotkeyState::Error { keys },
            (true, None) => HotkeyState::Listening { held: &held },
            _ => HotkeyState::Idle,
        };
        let mut clicked = false;
        row(ui, look, label, 7.0, |ui| {
            clicked = widgets::hotkey_field(ui, look, &keys, state).clicked();
            if let Some((_, msg)) = self.hotkey_error.as_ref().filter(|_| mine) {
                hint(ui, msg.clone(), look.p.warn);
            }
        });
        if clicked {
            self.stop_listening();
            if !mine {
                self.start_listening(field);
            }
        }
    }

    pub(in crate::settings) fn general(&mut self, ui: &mut Ui, look: &Look) {
        self.hotkey_row(ui, look, "record hotkey", HotkeyField::Record);
        self.hotkey_row(ui, look, "screenshot hotkey", HotkeyField::Screenshot);
        self.hotkey_row(ui, look, "all screens hotkey", HotkeyField::Fullscreen);
        row(ui, look, "launch at startup", 1.0, |ui| {
            let text = if self.cfg.launch_at_startup { "on" } else { "off" };
            widgets::toggle(ui, look, &mut self.cfg.launch_at_startup, text);
            hint(ui, "glint starts in the tray when you sign in to windows", look.p.t3);
        });
        row(ui, look, "freeze screen", 1.0, |ui| {
            let text = if self.cfg.freeze_screen { "on" } else { "off" };
            widgets::toggle(ui, look, &mut self.cfg.freeze_screen, text);
            hint(ui, "while selecting a region, show a still image instead of the live screen", look.p.t3);
        });
    }
}
