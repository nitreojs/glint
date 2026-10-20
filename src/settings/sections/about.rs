use eframe::egui::{RichText, Ui};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, w};

use crate::config::Config;
use crate::settings::widgets::{self, Kind, Look, hint, row};
use crate::settings::{Dialog, SettingsApp};

const REPO_URL: &str = "https://github.com/nitreojs/glint";

fn open_url(url: &str) {
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
                self.dialog = Some(Dialog::Licences);
            }
        });
        row(ui, look, "all settings", 5.0, |ui| {
            let any = self.cfg != Config::default();
            if widgets::button(ui, look, "reset everything to defaults", Kind::Secondary, 28.0, any).clicked() {
                self.cfg = Config::default();
            }
            hint(ui, "every section at once. nothing changes until you save.", p.t3);
        });
    }
}
