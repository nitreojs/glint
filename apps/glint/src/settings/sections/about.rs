use eframe::egui::{RichText, Ui};
use glint_core::config::Config;
use glint_ui::widgets::{self, Kind, Look, row};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, w};

use crate::settings::SettingsApp;

const REPO_URL: &str = "https://github.com/nitreojs/glint";

pub(super) fn open_url(url: &str) {
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(url), None, None, SW_SHOWNORMAL);
    }
}

impl SettingsApp {
    pub(in crate::settings) fn about(&mut self, ui: &mut Ui, look: &Look) {
        let p = look.p;
        row(ui, look, "version", 0.0, |ui| {
            let version = concat!("v", env!("CARGO_PKG_VERSION"));
            ui.label(RichText::new(version).font(widgets::mono_medium(13.0)).color(p.t1));
        });
        row(ui, look, "source code", 0.0, |ui| {
            if widgets::link(ui, look, "github.com/nitreojs/glint ↗").clicked() {
                open_url(REPO_URL);
            }
        });
        row(ui, look, "licences", 0.0, |ui| {
            if widgets::link(ui, look, "open-source licences").clicked() {
                self.licences = true;
            }
        });
        row(ui, look, "all settings", 5.0, |ui| {
            let any = self.cfg != Config::default();
            if widgets::button(ui, look, "reset everything to defaults", Kind::Secondary, 28.0, any).clicked() {
                self.cfg = Config::default();
            }
        });
    }
}
