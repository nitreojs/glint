use eframe::egui::Ui;
use glint_ui::widgets::Look;

use crate::settings::SettingsApp;

impl SettingsApp {
    pub(in crate::settings) fn audio(&mut self, ui: &mut Ui, look: &Look) {
        self.audio.show(ui, look, &mut self.cfg.audio_sources, &mut self.cfg.audio_track_names);
    }
}
