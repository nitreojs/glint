//! The file name rows of an app's settings: the pattern with an example, and switches that name
//! files after the window and sort them into a folder per app. The example shows what either
//! switch does, so they carry no hints.

use eframe::egui::{self, RichText, Ui};
use glint_core::history::Kind as HistoryKind;
use glint_core::naming::{Names, Naming, Source, Subject, pattern_problem};
use glint_shell::time::date_time;

use crate::widgets::{self, Look, hint, mono, row, toggle_row};

pub fn rows(ui: &mut Ui, look: &Look, names: &Names, pattern: &mut String) {
    let p = look.p;
    let problem = pattern_problem(pattern);
    row(ui, look, "file names", 7.0, |ui| {
        widgets::text_field(ui, look, pattern, names.default, problem.is_some());
        let (date, time) = date_time();
        let sample = Source { exe: "Telegram.exe".into(), app: "Telegram Desktop".into(), title: "Saved Messages".into(), path: String::new() };
        let naming = Naming { date: &date, time: &time, kind: HistoryKind::Recording, subject: Subject::Window(&sample) };
        let example = format!("{}.mp4", names.expand(pattern, &naming).display());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(RichText::new("→").font(mono(12.0)).color(p.t3));
            ui.add(egui::Label::new(RichText::new(example).font(mono(12.0)).color(p.t2)).truncate());
        });
        match problem {
            Some(msg) => hint(ui, msg, p.warn),
            None => hint(ui, "{date} {time} {app} {title} {kind} · / makes a folder", p.t3),
        }
    });
    let mut on = names.names_after_window(pattern);
    let text = if on { "on" } else { "off" };
    if toggle_row(ui, look, "app in the name", &mut on, text).changed() {
        *pattern = names.with_window_names(pattern, on);
    }
    let mut on = names.sorted_by_app(pattern);
    let text = if on { "on" } else { "off" };
    if toggle_row(ui, look, "folder per app", &mut on, text).changed() {
        *pattern = names.with_app_folders(pattern, on);
    }
}
