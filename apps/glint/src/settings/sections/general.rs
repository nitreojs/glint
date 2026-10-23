use eframe::egui::{
    self, Align, Color32, Id, Layout, Margin, Popup, PopupCloseBehavior, RectAlign, Response, Sense, Stroke, StrokeKind,
    Ui, vec2,
};
use glint_core::config::color::{hsv_to_rgb, rgb_to_hsv, rgb_to_hsv_near};
use glint_core::config::{DEFAULT_ACCENT, UiElements, hex, parse_hex};
use glint_ui::widgets::{self, HotkeyState, Look, hint, row};

use crate::settings::SettingsApp;
use crate::settings::hotkey_capture::{HotkeyField, held_names, key_names};

pub(in crate::settings) const ACCENTS: [[u8; 3]; 8] = [
    [0xE5, 0x40, 0x2F],
    [0xE8, 0x6A, 0x1F],
    [0xD9, 0x43, 0x7A],
    [0x8E, 0x5B, 0xD9],
    [0x2F, 0x7B, 0xE5],
    [0x1F, 0x9E, 0x8F],
    [0x3E, 0x9E, 0x4A],
    [0x7A, 0x77, 0x72],
];
const PICKER_W: f32 = 232.0;

pub(in crate::settings) fn accent_popup_id() -> Id {
    Id::unique("accent-picker")
}

fn color(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

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
            let error = self.hotkey_error.as_ref().filter(|_| mine).map(|(_, msg)| msg.clone());
            widgets::reveal_with(ui, Id::unique(("hotkey-error", label)), error, |ui, msg| hint(ui, msg, look.p.warn));
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
        self.hotkey_row(ui, look, "monitor hotkey", HotkeyField::Monitor);
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
        let preset = ACCENTS.contains(&current);
        let custom = self.cfg.accent_custom_rgb().or((!preset).then_some(current));
        let open = Popup::is_id_open(ui.ctx(), accent_popup_id());
        row(ui, look, "accent", 3.0, |ui| {
            let swatch = ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for rgb in ACCENTS {
                    if widgets::swatch(ui, look, color(rgb), rgb == current && !open).clicked() {
                        self.cfg.accent = hex(rgb);
                    }
                }
                widgets::custom_swatch(ui, look, custom.map(color), open || !preset)
            });
            if let Some(rgb) = custom.filter(|_| swatch.inner.clicked() && !open) {
                self.pick_accent(ui, rgb);
            }
            self.accent_picker(look, &swatch.inner);
            hint(ui, "a preset, or your own colour from the last swatch, for these windows and the tray icon", look.p.t3);
        });
    }

    fn accent_picker(&mut self, look: &Look, anchor: &Response) {
        let p = look.p;
        let id = accent_popup_id();
        let frame = egui::Frame::new()
            .fill(p.surf)
            .stroke(Stroke::new(1.0, p.line2))
            .corner_radius(8)
            .inner_margin(Margin::same(12))
            .shadow(egui::epaint::Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(80) });
        let popup = Popup::from_toggle_button_response(anchor)
            .id(id)
            .accessible_name("custom accent colour")
            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
            .align(RectAlign::BOTTOM_START)
            .align_alternatives(&[RectAlign::TOP_START, RectAlign::BOTTOM_END, RectAlign::TOP_END])
            .gap(8.0)
            .frame(frame);
        popup.show(|ui| {
            ui.set_width(PICKER_W);
            ui.spacing_mut().item_spacing = vec2(8.0, 10.0);
            let saved = ui.data(|d| d.get_temp::<[f32; 3]>(id));
            let start = self.cfg.accent_rgb();
            let mut hsv = match saved {
                Some(hsv) if hsv_to_rgb(hsv) == start => hsv,
                Some(hsv) => rgb_to_hsv_near(start, hsv),
                None => rgb_to_hsv(start),
            };
            let sv = widgets::sv_square(ui, look, &mut hsv, vec2(PICKER_W, 148.0));
            let hue = widgets::hue_strip(ui, look, &mut hsv, vec2(PICKER_W, 12.0));
            if sv.changed() || hue.changed() {
                self.pick_accent(ui, hsv_to_rgb(hsv));
                self.accent_text = self.cfg.accent.clone();
            }
            let current = self.cfg.accent_rgb();
            let valid = parse_hex(&self.accent_text).is_some();
            ui.horizontal(|ui| {
                let (chip, _) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::hover());
                ui.painter().rect(chip, 5, color(current), Stroke::new(1.0, p.line2), StrokeKind::Inside);
                let field = ui.allocate_ui(vec2(112.0, 32.0), |ui| {
                    widgets::text_field(ui, look, &mut self.accent_text, DEFAULT_ACCENT, !valid)
                });
                let typed = parse_hex(&self.accent_text);
                if let Some(rgb) = typed.filter(|_| field.inner.changed()) {
                    self.pick_accent(ui, rgb);
                    hsv = rgb_to_hsv_near(rgb, hsv);
                } else if !field.inner.has_focus() && typed != Some(current) {
                    self.accent_text = self.cfg.accent.clone();
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::link(ui, look, "reset").clicked() {
                        self.cfg.accent = DEFAULT_ACCENT.into();
                        self.cfg.accent_custom = None;
                        self.accent_text = self.cfg.accent.clone();
                        hsv = rgb_to_hsv(self.cfg.accent_rgb());
                        ui.ctx().request_repaint();
                    }
                });
            });
            if !valid {
                hint(ui, "type a colour as #rrggbb, like #e5402f", p.warn);
            } else if p.on_acc != Color32::WHITE {
                hint(ui, "a light colour: buttons on it get dark text", p.t3);
            }
            ui.data_mut(|d| d.insert_temp(id, hsv));
        });
    }

    fn pick_accent(&mut self, ui: &Ui, rgb: [u8; 3]) {
        self.cfg.accent = hex(rgb);
        self.cfg.accent_custom = Some(hex(rgb));
        ui.ctx().request_repaint();
    }
}
