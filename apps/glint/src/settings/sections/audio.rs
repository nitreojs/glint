use eframe::egui::Ui;
use glint_core::clip::edit::{FULL_VOLUME, MAX_VOLUME};
use glint_ui::widgets::{self, Look};

use crate::settings::SettingsApp;

impl SettingsApp {
    pub(in crate::settings) fn audio(&mut self, ui: &mut Ui, look: &Look) {
        self.audio.show(ui, look, &mut self.cfg.audio_sources, &mut self.cfg.audio_track_names);
        widgets::subheading(ui, look, "editor mix defaults");
        ui.label("match recorded track names · applies to preview and export, not capture");
        let mut remove = None;
        for (name, volume) in &mut self.cfg.editor_track_volumes {
            widgets::row(ui, look, name, 0.0, |ui| {
                ui.horizontal(|ui| {
                    ui.add(eframe::egui::Slider::new(volume, 0..=MAX_VOLUME).suffix("%"));
                    if ui.button("remove").clicked() {
                        remove = Some(name.clone());
                    }
                });
            });
        }
        if let Some(name) = remove {
            self.cfg.editor_track_volumes.remove(&name);
        }
        ui.horizontal(|ui| {
            ui.add(eframe::egui::TextEdit::singleline(&mut self.editor_track_name).hint_text("recorded track name"));
            let name = self.editor_track_name.trim().to_lowercase();
            if ui.add_enabled(!name.is_empty(), eframe::egui::Button::new("add track default")).clicked() {
                self.cfg.editor_track_volumes.entry(name).or_insert(FULL_VOLUME);
                self.editor_track_name.clear();
            }
        });
    }
}
