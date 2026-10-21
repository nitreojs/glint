use eframe::egui::{Id, Ui};
use glint_core::config::{ShotFormat, ShotStyle};
use glint_ui::widgets::{self, Look, hint, row};

use crate::settings::SettingsApp;

fn shot_quality_word(q: u32) -> &'static str {
    match q {
        95.. => "near lossless",
        85..=94 => "high",
        70..=84 => "good",
        _ => "small files",
    }
}

impl SettingsApp {
    pub(in crate::settings) fn screenshots(&mut self, ui: &mut Ui, look: &Look) {
        let p = look.p;
        row(ui, look, "style", 7.0, |ui| {
            let styles = [(ShotStyle::Select, "select first"), (ShotStyle::Sharex, "draw first")];
            widgets::segmented(ui, look, &mut self.cfg.screenshot_style, &styles, Some(220.0));
            let frozen = if self.cfg.freeze_screen { "" } else { " · the screen freezes anyway" };
            hint(ui, format!("draw first — like sharex: draw on the screen, then pick a region with Q{frozen}"), p.t3);
        });
        row(ui, look, "format", 7.0, |ui| {
            let formats = [(ShotFormat::Png, "png"), (ShotFormat::Jpg, "jpg")];
            widgets::segmented(ui, look, &mut self.cfg.screenshot_format, &formats, Some(168.0));
            hint(ui, "png — lossless · jpg — smaller files", p.t3);
        });
        // The quality row only matters for jpg; it fades in and out with the choice.
        let jpg = self.cfg.screenshot_format == ShotFormat::Jpg;
        let shown = widgets::glide(ui, Id::unique("jpg-quality"), if jpg { 1.0 } else { 0.0 }, 0.12);
        if shown > 0.0 {
            ui.scope(|ui| {
                ui.multiply_opacity(shown);
                row(ui, look, "quality", 0.0, |ui| {
                    let word = shot_quality_word(self.cfg.screenshot_quality);
                    let q = &mut self.cfg.screenshot_quality;
                    widgets::slider(ui, look, q, 50..=100, word, "50 ← smaller", "sharper → 100");
                });
            });
        }
        row(ui, look, "after capture", 1.0, |ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            widgets::checkbox(ui, look, &mut self.cfg.screenshot_copy, "copy to clipboard");
            widgets::checkbox(ui, look, &mut self.cfg.screenshot_save, "save to the output folder");
            widgets::checkbox(ui, look, &mut self.cfg.screenshot_upload, "upload");
            if !self.cfg.screenshot_copy && !self.cfg.screenshot_save && !self.cfg.screenshot_upload {
                ui.spacing_mut().item_spacing.y = 6.0;
                hint(ui, "with all off, enter still copies", p.warn);
            }
        });
        row(ui, look, "in the editor", 0.0, |ui| {
            hint(ui, "enter does the above · ctrl+c copies · ctrl+s saves · ctrl+u uploads · esc cancels", p.t3);
        });
    }
}
