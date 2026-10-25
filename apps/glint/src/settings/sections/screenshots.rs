use eframe::egui::{Id, Ui};
use glint_core::config::{ShotFormat, ShotStyle};
use glint_ui::widgets::{self, Look, hint, row, toggle_row};

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
            let text = match self.cfg.screenshot_style {
                ShotStyle::Select => "pick an area, then edit it".to_string(),
                ShotStyle::Sharex => {
                    let frozen = if self.cfg.freeze_screen { "" } else { " · freezes the screen" };
                    format!("like sharex: release to capture, hold shift to edit{frozen}")
                }
            };
            hint(ui, text, p.t3);
        });
        row(ui, look, "format", 7.0, |ui| {
            let formats = [(ShotFormat::Png, "png"), (ShotFormat::Jpg, "jpg")];
            widgets::segmented(ui, look, &mut self.cfg.screenshot_format, &formats, Some(168.0));
        });
        // The quality row only matters for jpg.
        let jpg = self.cfg.screenshot_format == ShotFormat::Jpg;
        widgets::reveal(ui, Id::unique("jpg-quality"), jpg, |ui| {
            row(ui, look, "quality", 0.0, |ui| {
                let word = shot_quality_word(self.cfg.screenshot_quality);
                let q = &mut self.cfg.screenshot_quality;
                widgets::slider(ui, look, q, 50..=100, word, "50 ← smaller", "sharper → 100");
            });
        });
        let text = if self.cfg.screenshot_cursor { "kept, as a mark you can move" } else { "left out" };
        toggle_row(ui, look, "mouse cursor", &mut self.cfg.screenshot_cursor, text);
        row(ui, look, "after capture", 1.0, |ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            widgets::checkbox(ui, look, &mut self.cfg.screenshot_copy, "copy to clipboard");
            widgets::checkbox(ui, look, &mut self.cfg.screenshot_save, "save to the output folder");
            widgets::checkbox(ui, look, &mut self.cfg.screenshot_upload, "upload");
            let none = !self.cfg.screenshot_copy && !self.cfg.screenshot_save && !self.cfg.screenshot_upload;
            ui.spacing_mut().item_spacing.y = 6.0;
            widgets::reveal(ui, Id::unique("no-capture-action"), none, |ui| hint(ui, "with all off, enter still copies", p.warn));
        });
        row(ui, look, "in the editor", 1.0, |ui| {
            let keys =
                [("enter", "runs the above"), ("ctrl+c", "copy"), ("ctrl+s", "save"), ("ctrl+u", "upload"), ("esc", "cancel")];
            widgets::shortcuts(ui, look, &keys);
        });
    }
}
