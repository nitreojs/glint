use std::path::PathBuf;

use eframe::egui::{Id, Ui};
use glint_core::config::{Config, Expiry};
use glint_core::upload::proxy::{self, Setting};
use glint_core::upload::{BUILT_IN, custom};
use glint_ui::widgets::{self, Kind, Look, hint, row};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH,
};
use windows::core::w;

use crate::app::ipc;
use crate::platform::paths::documents_dir;
use crate::settings::SettingsApp;
use crate::upload::proxy::describe_system;

fn pick_sxcu() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    unsafe {
        let Ok(dialog) = CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) else {
            return paths;
        };
        let filter = [COMDLG_FILTERSPEC { pszName: w!("ShareX custom uploader"), pszSpec: w!("*.sxcu") }];
        let _ = dialog.SetFileTypes(&filter);
        if let Ok(opts) = dialog.GetOptions() {
            let _ = dialog.SetOptions(opts | FOS_ALLOWMULTISELECT | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM);
        }
        if dialog.Show(ipc::find_settings()).is_err() {
            return paths;
        }
        let Ok(items) = dialog.GetResults() else { return paths };
        for i in 0..items.GetCount().unwrap_or(0) {
            let Ok(name) = items.GetItemAt(i).and_then(|item| item.GetDisplayName(SIGDN_FILESYSPATH)) else { continue };
            if let Ok(s) = name.to_string() {
                paths.push(PathBuf::from(s));
            }
            CoTaskMemFree(Some(name.0 as _));
        }
    }
    paths
}

fn about(destination: &str) -> &'static str {
    match destination {
        "catbox" => "catbox.moe · kept for good · up to 200 MB, gifs up to 20 MB",
        "litterbox" => "litterbox.catbox.moe · deleted after the time below · up to 1 GB",
        "uguu" => "uguu.se · deleted after 3 hours · up to 128 MB",
        "pomf" => "pomf.lain.la · up to 1 GB",
        _ => "a custom uploader (sharex .sxcu)",
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Pick {
    Host(usize),
    Custom,
}

#[derive(Clone, Copy, PartialEq)]
enum ProxyPick {
    System,
    Direct,
    Custom,
}

pub(in crate::settings) struct Uploaders {
    names: Vec<String>,
    note: Option<(String, bool)>,
    system_proxy: String,
    custom_proxy: bool,
}

impl Uploaders {
    pub(in crate::settings) fn load() -> Uploaders {
        Uploaders { names: custom::names(), note: None, system_proxy: describe_system(), custom_proxy: false }
    }

    fn refresh(&mut self, note: String, warn: bool) {
        self.names = custom::names();
        self.note = Some((note, warn));
    }
}

impl SettingsApp {
    pub(in crate::settings) fn uploads(&mut self, ui: &mut Ui, look: &Look) {
        let p = look.p;
        let current = BUILT_IN.iter().position(|b| *b == self.cfg.upload_destination);
        let mut pick = current.map_or(Pick::Custom, Pick::Host);
        let mut options: Vec<(Pick, &str)> = BUILT_IN.iter().enumerate().map(|(i, b)| (Pick::Host(i), *b)).collect();
        if !self.uploaders.names.is_empty() || current.is_none() {
            options.push((Pick::Custom, "custom"));
        }
        row(ui, look, "destination", 7.0, |ui| {
            if widgets::segmented(ui, look, &mut pick, &options, None).changed() {
                self.cfg.upload_destination = match pick {
                    Pick::Host(i) => BUILT_IN[i].to_string(),
                    Pick::Custom => self.uploaders.names.first().cloned().unwrap_or_default(),
                };
            }
            hint(ui, about(&self.cfg.upload_destination), p.t3);
            hint(ui, "uploads are public: anyone with the link can open the file", p.warn);
        });
        let litterbox = self.cfg.upload_destination == "litterbox";
        let shown = widgets::glide(ui, Id::unique("litterbox-expiry"), if litterbox { 1.0 } else { 0.0 }, 0.12);
        if shown > 0.0 {
            ui.scope(|ui| {
                ui.multiply_opacity(shown);
                row(ui, look, "keep for", 7.0, |ui| {
                    let times = [
                        (Expiry::Hour, "1 hour"),
                        (Expiry::HalfDay, "12 hours"),
                        (Expiry::Day, "1 day"),
                        (Expiry::ThreeDays, "3 days"),
                    ];
                    widgets::segmented(ui, look, &mut self.cfg.litterbox_expiry, &times, None);
                });
            });
        }
        row(ui, look, "custom uploaders", 1.0, |ui| self.custom_list(ui, look));
        row(ui, look, "after upload", 1.0, |ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            widgets::toggle(ui, look, &mut self.cfg.upload_copy_link, "copy the link");
            widgets::toggle(ui, look, &mut self.cfg.recording_upload, "upload every recording once it's saved");
        });
        row(ui, look, "proxy", 7.0, |ui| self.proxy_row(ui, look));
    }

    fn proxy_row(&mut self, ui: &mut Ui, look: &Look) {
        let p = look.p;
        let parsed = proxy::parse(&self.cfg.upload_proxy);
        let typing = self.uploaders.custom_proxy && self.cfg.upload_proxy.trim().is_empty();
        let mut pick = match parsed {
            Ok(Setting::System) if !typing => ProxyPick::System,
            Ok(Setting::None) => ProxyPick::Direct,
            _ => ProxyPick::Custom,
        };
        let options = [(ProxyPick::System, "system"), (ProxyPick::Direct, "none"), (ProxyPick::Custom, "custom")];
        if widgets::segmented(ui, look, &mut pick, &options, None).changed() {
            self.uploaders.custom_proxy = pick == ProxyPick::Custom;
            self.cfg.upload_proxy = match pick {
                ProxyPick::System => "system".into(),
                ProxyPick::Direct => "none".into(),
                ProxyPick::Custom => String::new(),
            };
        }
        match pick {
            ProxyPick::System => hint(ui, self.uploaders.system_proxy.clone(), p.t3),
            ProxyPick::Direct => hint(ui, "uploads connect straight to the host", p.t3),
            ProxyPick::Custom => {
                let problem = proxy::parse(&self.cfg.upload_proxy).err();
                widgets::text_field(ui, look, &mut self.cfg.upload_proxy, "http://host:port", problem.is_some());
                let (text, color) = match problem {
                    Some(e) => (e, p.warn),
                    None if self.cfg.upload_proxy.trim().is_empty() => ("empty uses the system proxy".into(), p.t3),
                    None => ("http proxies only: socks can't carry uploads".into(), p.t3),
                };
                hint(ui, text, color);
            }
        }
    }

    fn custom_list(&mut self, ui: &mut Ui, look: &Look) {
        let p = look.p;
        ui.spacing_mut().item_spacing.y = 8.0;
        let mut removed = None;
        for name in &self.uploaders.names {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                let mut on = self.cfg.upload_destination == *name;
                if widgets::checkbox(ui, look, &mut on, name).clicked() {
                    self.cfg.upload_destination = if on { name.clone() } else { Config::default().upload_destination };
                }
                if widgets::link(ui, look, "remove").clicked() {
                    removed = Some(name.clone());
                }
            });
        }
        if self.uploaders.names.is_empty() {
            hint(ui, "sharex .sxcu files; imported ones show up here", p.t3);
        }
        if let Some(name) = removed {
            match custom::remove(&name) {
                Ok(()) => {
                    if self.cfg.upload_destination == name {
                        self.cfg.upload_destination = Config::default().upload_destination;
                    }
                    self.uploaders.refresh(format!("removed {name}"), false);
                }
                Err(e) => self.uploaders.note = Some((e, true)),
            }
        }
        let sharex = documents_dir().and_then(custom::sharex_config);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if widgets::button(ui, look, "import .sxcu…", Kind::Secondary, 28.0, true).clicked() {
                self.import_files();
            }
            if let Some(config) = &sharex
                && widgets::button(ui, look, "import from sharex", Kind::Secondary, 28.0, true).clicked()
            {
                let note = match custom::import_sharex(config) {
                    Ok((done, skipped)) if done.is_empty() && skipped.is_empty() => {
                        ("sharex has no custom uploaders".to_string(), false)
                    }
                    Ok((done, skipped)) => {
                        let mut note = format!("imported {}", done.len());
                        if !skipped.is_empty() {
                            note += &format!(", skipped {}: {}", skipped.len(), skipped.join("; "));
                        }
                        (note, !skipped.is_empty())
                    }
                    Err(e) => (e, true),
                };
                self.uploaders.refresh(note.0, note.1);
            }
        });
        if let Some((note, warn)) = &self.uploaders.note {
            hint(ui, note.clone(), if *warn { p.warn } else { p.t3 });
        }
    }

    fn import_files(&mut self) {
        let files = pick_sxcu();
        if files.is_empty() {
            return;
        }
        let (mut done, mut failed) = (Vec::new(), Vec::new());
        for file in files {
            match custom::import(&file) {
                Ok(name) => done.push(name),
                Err(e) => failed.push(e),
            }
        }
        if let [name] = done.as_slice() {
            self.cfg.upload_destination = name.clone();
        }
        let mut note = if done.is_empty() { String::new() } else { format!("imported {}", done.join(", ")) };
        if !failed.is_empty() {
            note += &format!("{}{}", if note.is_empty() { "" } else { " · " }, failed.join("; "));
        }
        self.uploaders.refresh(note, !failed.is_empty());
    }
}
