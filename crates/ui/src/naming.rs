//! The file name rows of an app's settings: the pattern with an example, and switches that name
//! files after the window and sort them into a folder per app.

use eframe::egui::{self, RichText, Ui};
use glint_core::history::Kind as HistoryKind;
use glint_core::naming::{Names, Naming, Source, Subject, pattern_problem};
use glint_shell::time::date_time;

use crate::widgets::{self, Look, hint, mono, row};

/// What the switches' hints say about this app's captures.
pub struct About {
    /// What "name files after the window" does.
    pub window: &'static str,
    /// What "sort into folders by app" does.
    pub folders: &'static str,
}

pub fn rows(ui: &mut Ui, look: &Look, names: &Names, pattern: &mut String, about: &About) {
    let p = look.p;
    let problem = pattern_problem(pattern);
    row(ui, look, "file names", 7.0, |ui| {
        widgets::text_field(ui, look, pattern, names.default, problem.is_some());
        let (date, time) = date_time();
        let sample = Source { exe: "Telegram.exe".into(), app: "Telegram Desktop".into(), title: "Saved Messages".into() };
        let naming = Naming { date: &date, time: &time, kind: HistoryKind::Recording, subject: Subject::Window(&sample) };
        let example = format!("{}.mp4", names.expand(pattern, &naming).display());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(RichText::new("→").font(mono(12.0)).color(p.t3));
            ui.add(egui::Label::new(RichText::new(example).font(mono(12.0)).color(p.t2)).truncate());
        });
        match problem {
            Some(msg) => hint(ui, msg, p.warn),
            None => hint(ui, "{date} {time} {app} {title} {kind} are filled in, / makes folders", p.t3),
        }
    });
    row(ui, look, "name files after the window", 1.0, |ui| {
        let mut on = names.names_after_window(pattern);
        let text = if on { "on" } else { "off" };
        if widgets::toggle(ui, look, &mut on, text).changed() {
            *pattern = names.with_window_names(pattern, on);
        }
        hint(ui, about.window, p.t3);
    });
    row(ui, look, "sort into folders by app", 1.0, |ui| {
        let mut on = names.sorted_by_app(pattern);
        let text = if on { "on" } else { "off" };
        if widgets::toggle(ui, look, &mut on, text).changed() {
            *pattern = names.with_app_folders(pattern, on);
        }
        hint(ui, about.folders, p.t3);
    });
}
