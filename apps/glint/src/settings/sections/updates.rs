use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use eframe::egui::{RichText, Ui};
use glint_core::config::UpdateMode;
use glint_core::update::{RELEASES_PAGE, notes_url};
use glint_ui::widgets::{self, Kind, Look, hint, row, sans};

use super::about::open_url;
use crate::app::ipc::{self, WM_UPDATE_CHECK, WM_UPDATE_INSTALL};
use crate::app::updates::{CHECK_BETA, CHECK_DOWNLOAD};
use crate::settings::SettingsApp;
use crate::update::{State, Status};

/// The tray app's update status, re-read whenever its status.json changes.
pub(in crate::settings) struct Updater {
    path: PathBuf,
    stamp: Option<SystemTime>,
    pub(in crate::settings) status: Status,
    /// Why the last button press went nowhere.
    note: Option<String>,
}

impl Updater {
    pub(in crate::settings) fn load() -> Updater {
        Updater::at(Status::path())
    }

    pub(in crate::settings) fn at(path: PathBuf) -> Updater {
        let mut u = Updater { path, stamp: None, status: Status::default(), note: None };
        u.poll();
        u
    }

    fn poll(&mut self) {
        let stamp = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        if stamp != self.stamp {
            self.stamp = stamp;
            self.status = Status::load_from(&self.path);
        }
    }

    /// Sends `msg` to the tray app, which does the work and reports back through status.json.
    fn ask(&mut self, msg: u32, wparam: usize, meanwhile: State) {
        if ipc::post_to_main(msg, wparam) {
            self.status.state = meanwhile;
            self.note = None;
        } else {
            self.note = Some("glint isn't running in the tray; start it first".into());
        }
    }
}

impl SettingsApp {
    pub(in crate::settings) fn updates(&mut self, ui: &mut Ui, look: &Look) {
        let p = look.p;
        self.updater.poll();
        ui.ctx().request_repaint_after(Duration::from_millis(500));
        row(ui, look, "version", 0.0, |ui| {
            let version = concat!("v", env!("CARGO_PKG_VERSION"));
            ui.label(RichText::new(version).font(widgets::mono_medium(13.0)).color(p.t1));
        });
        row(ui, look, "updates", 7.0, |ui| {
            let modes = [(UpdateMode::Auto, "auto"), (UpdateMode::Notify, "notify only"), (UpdateMode::Off, "off")];
            widgets::segmented(ui, look, &mut self.cfg.update_mode, &modes, Some(300.0));
            let about = match self.cfg.update_mode {
                UpdateMode::Auto => "new versions download in the background and install when glint restarts",
                UpdateMode::Notify => "a notification tells you about new versions; clicking it installs",
                UpdateMode::Off => "glint never checks on its own",
            };
            hint(ui, about, p.t3);
            hint(ui, "every download is checked against glint's release signature before it's used", p.t3);
        });
        row(ui, look, "beta updates", 1.0, |ui| {
            widgets::toggle(ui, look, &mut self.cfg.update_beta, "also take pre-releases");
            hint(ui, "early builds of the next version; they can be rough", p.t3);
        });
        row(ui, look, "status", 7.0, |ui| self.update_status(ui, look));
        row(ui, look, "release notes", 0.0, |ui| {
            let (text, url) = match &self.updater.status.state {
                State::Available { version } | State::Downloading { version } | State::Ready { version } => {
                    (format!("what's new in {version} ↗"), notes_url(&format!("v{version}")))
                }
                _ => ("github.com/nitreojs/glint/releases ↗".to_string(), RELEASES_PAGE.to_string()),
            };
            if widgets::link(ui, look, &text).clicked() {
                open_url(&url);
            }
        });
    }

    fn update_status(&mut self, ui: &mut Ui, look: &Look) {
        let p = look.p;
        let state = self.updater.status.state.clone();
        let busy = matches!(state, State::Checking | State::Downloading { .. });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            if widgets::button(ui, look, "check now", Kind::Secondary, 32.0, !busy).clicked() {
                let download = if self.cfg.update_mode == UpdateMode::Auto { CHECK_DOWNLOAD } else { 0 };
                let beta = if self.cfg.update_beta { CHECK_BETA } else { 0 };
                self.updater.ask(WM_UPDATE_CHECK, download | beta, State::Checking);
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                let text = |ui: &mut Ui, text: &str| ui.label(RichText::new(text).font(sans(13.0)).color(p.t2));
                match &state {
                    State::Unknown | State::Failed { .. } => {}
                    State::Checking => {
                        text(ui, "checking…");
                    }
                    State::UpToDate => {
                        text(ui, "up to date");
                    }
                    State::Downloading { version } => {
                        text(ui, &format!("downloading {version}…"));
                    }
                    State::Available { version } => {
                        text(ui, &format!("{version} available ·"));
                        if widgets::link(ui, look, "install").clicked() {
                            self.updater.ask(WM_UPDATE_INSTALL, 0, State::Downloading { version: version.clone() });
                        }
                    }
                    State::Ready { version } => {
                        text(ui, &format!("{version} ready ·"));
                        if widgets::link(ui, look, "restart").clicked() {
                            self.updater.ask(WM_UPDATE_INSTALL, 0, state.clone());
                        }
                    }
                }
            });
        });
        if let State::Failed { error } = &state {
            hint(ui, error.clone(), p.warn);
        }
        if let Some(note) = &self.updater.note {
            hint(ui, note.clone(), p.warn);
        }
        let checked = match &self.updater.status.checked {
            Some(at) => format!("last checked {at}"),
            None => "not checked yet".to_string(),
        };
        hint(ui, checked, p.t3);
    }
}
