use eframe::egui::{Color32, Ui, vec2};

use crate::config::{UiElements, hex, parse_hex};
use crate::settings::SettingsApp;
use crate::settings::hotkey_capture::{HotkeyField, held_names, key_names};
use crate::settings::widgets::{self, HotkeyState, Look, hint, row};

const ACCENTS: [[u8; 3]; 8] = [
    [0xE5, 0x40, 0x2F],
    [0xE8, 0x6A, 0x1F],
    [0xD9, 0x43, 0x7A],
    [0x8E, 0x5B, 0xD9],
    [0x2F, 0x7B, 0xE5],
    [0x1F, 0x9E, 0x8F],
    [0x3E, 0x9E, 0x4A],
    [0x7A, 0x77, 0x72],
];

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
        self.hotkey_row(ui, look, "pause hotkey", HotkeyField::Pause);
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
        row(ui, look, "ui elements", 7.0, |ui| {
            let modes = [(UiElements::Shift, "hold shift"), (UiElements::Always, "always"), (UiElements::Off, "off")];
            widgets::segmented(ui, look, &mut self.cfg.ui_elements, &modes, Some(260.0));
            hint(ui, "hold shift while selecting to pick buttons and panels inside apps", look.p.t3);
        });
        row(ui, look, "notifications", 7.0, |ui| {
            let choices = [(true, "all"), (false, "errors only")];
            widgets::segmented(ui, look, &mut self.cfg.notify_success, &choices, Some(200.0));
            hint(ui, "errors only — no popup after every screenshot or recording", look.p.t3);
        });
        self.accent_row(ui, look);
    }

    fn accent_row(&mut self, ui: &mut Ui, look: &Look) {
        let current = self.cfg.accent_rgb();
        row(ui, look, "accent", 3.0, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for rgb in ACCENTS {
                    let color = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                    if widgets::swatch(ui, look, color, rgb == current).clicked() {
                        self.cfg.accent = hex(rgb);
                        self.accent_text = self.cfg.accent.clone();
                    }
                }
            });
            let valid = parse_hex(&self.accent_text).is_some();
            let field = ui.allocate_ui(vec2(120.0, 32.0), |ui| {
                widgets::text_field(ui, look, &mut self.accent_text, "#E5402F", !valid)
            });
            let typed = parse_hex(&self.accent_text);
            if let Some(rgb) = typed.filter(|_| field.inner.changed()) {
                self.cfg.accent = hex(rgb);
            } else if !field.inner.has_focus() && typed != Some(self.cfg.accent_rgb()) {
                self.accent_text = self.cfg.accent.clone();
            }
            if valid {
                hint(ui, "a preset or any #rrggbb colour, for these windows and the tray icon", look.p.t3);
            } else {
                hint(ui, "type a colour as #rrggbb, like #e5402f", look.p.warn);
            }
        });
    }
}
