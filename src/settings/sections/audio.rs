use eframe::egui::{Ui, vec2};

use crate::config::audio::{AudioSource, MAX_TRACKS, MAX_VOLUME, SourceKind};
use crate::record::tracks::plan;
use crate::record::{Microphone, microphones, session_apps};
use crate::settings::SettingsApp;
use crate::settings::widgets::{self, Kind, Look, hint, row};

pub(in crate::settings) struct Devices {
    apps: Vec<String>,
    mics: Vec<Microphone>,
    default_mic: Option<String>,
}

impl Devices {
    fn load() -> Devices {
        let (mics, default_mic) = microphones();
        Devices { apps: session_apps(), mics, default_mic }
    }

    fn mic(&self, device: &str) -> Option<&Microphone> {
        let id = if device.is_empty() { self.default_mic.as_deref()? } else { device };
        self.mics.iter().find(|m| m.id == id)
    }

    fn label(&self, s: &AudioSource) -> String {
        match s.kind {
            SourceKind::App if s.app.trim().is_empty() => "app (no name yet)".into(),
            SourceKind::Mic if !s.device.is_empty() && self.mic(&s.device).is_none() => "mic (not connected)".into(),
            _ => s.label(self.mic(&s.device).map(|m| m.label.as_str())),
        }
    }
}

fn row_label(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Desktop => "desktop audio",
        SourceKind::App => "app audio",
        SourceKind::Mic => "microphone",
    }
}

fn volume_word(v: u32) -> &'static str {
    match v {
        0 => "% · muted",
        1..=99 => "% · quieter",
        100 => "%",
        _ => "% · louder",
    }
}

impl SettingsApp {
    pub(in crate::settings) fn audio(&mut self, ui: &mut Ui, look: &Look) {
        let p = look.p;
        let devices = self.audio_devices.get_or_insert_with(Devices::load);
        let mut removed = None;
        for (i, s) in self.cfg.audio_sources.iter_mut().enumerate() {
            row(ui, look, row_label(s.kind), 1.0, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 16.0;
                    let label = devices.label(s);
                    widgets::toggle(ui, look, &mut s.enabled, &label);
                    if widgets::link(ui, look, "remove").clicked() {
                        removed = Some(i);
                    }
                });
                ui.add_space(4.0);
                match s.kind {
                    SourceKind::Desktop => {
                        hint(ui, "what your default output device plays, minus one app if you name it", p.t3);
                        app_picker(ui, look, &mut s.exclude, &devices.apps, "leave out an app, like game.exe", false);
                    }
                    SourceKind::App => {
                        hint(ui, "only this app and what it starts; picked up if it starts mid-recording", p.t3);
                        let warn = s.app.trim().is_empty();
                        app_picker(ui, look, &mut s.app, &devices.apps, "name.exe", warn);
                    }
                    SourceKind::Mic => {
                        let default = match devices.mic("") {
                            Some(m) => format!("default ({})", m.name),
                            None => "default device".into(),
                        };
                        let mut items = vec![default.clone()];
                        items.extend(devices.mics.iter().map(|m| m.name.clone()));
                        let current = match devices.mic(&s.device) {
                            _ if s.device.is_empty() => default,
                            Some(m) => m.name.clone(),
                            None => "not connected".into(),
                        };
                        let w = ui.available_width();
                        match widgets::dropdown(ui, look, &current, &items, w) {
                            Some(0) => s.device.clear(),
                            Some(k) => s.device = devices.mics[k - 1].id.clone(),
                            None => {}
                        }
                    }
                }
                ui.add_space(4.0);
                let word = volume_word(s.volume);
                widgets::slider(ui, look, &mut s.volume, 0..=MAX_VOLUME, word, "0", "200 %");
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    hint(ui, "tracks", p.t2);
                    for n in 1..=MAX_TRACKS {
                        let mut on = s.tracks.contains(&n);
                        if widgets::checkbox(ui, look, &mut on, &n.to_string()).changed() {
                            s.tracks.retain(|&t| t != n);
                            if on {
                                s.tracks.push(n);
                                s.tracks.sort_unstable();
                            }
                        }
                    }
                });
            });
        }
        if let Some(i) = removed {
            self.cfg.audio_sources.remove(i);
        }
        row(ui, look, "add", 7.0, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                for kind in [SourceKind::Desktop, SourceKind::App, SourceKind::Mic] {
                    if widgets::button(ui, look, kind.name(), Kind::Secondary, 28.0, true).clicked() {
                        let n = self.cfg.audio_sources.len();
                        self.cfg.audio_sources.push(AudioSource::new(kind, n));
                    }
                }
                if widgets::button(ui, look, "refresh apps", Kind::Text, 28.0, true).clicked() {
                    self.audio_devices = Some(Devices::load());
                }
            });
            hint(ui, "app lists show programs that have played sound since they started", p.t3);
        });
        let devices = self.audio_devices.get_or_insert_with(Devices::load);
        let live: Vec<&AudioSource> = self.cfg.audio_sources.iter().filter(|s| s.enabled && s.records()).collect();
        let names: Vec<String> = live.iter().map(|s| devices.label(s)).collect();
        let routes: Vec<&[u32]> = live.iter().map(|s| s.tracks.as_slice()).collect();
        let tracks = plan(&names, &routes);
        row(ui, look, "tracks", 1.0, |ui| {
            if tracks.is_empty() {
                hint(ui, "no audio: turn a source on and give it a track", p.t3);
                return;
            }
            for (n, t) in tracks.iter().enumerate() {
                let mut line = format!("{} · {}", n + 1, t.name);
                if t.name == "mix" {
                    let parts: Vec<&str> = t.sources.iter().map(|&i| names[i].as_str()).collect();
                    line += &format!(" ({})", parts.join(" + "));
                }
                hint(ui, line, p.t1);
            }
            hint(ui, "empty tracks and tracks that repeat an earlier one are left out of the file", p.t3);
        });
    }
}

fn app_picker(ui: &mut Ui, look: &Look, exe: &mut String, apps: &[String], placeholder: &str, warn: bool) {
    let w = ui.available_width();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let field = (w - 8.0) * 0.55;
        ui.allocate_ui(vec2(field, 32.0), |ui| widgets::text_field(ui, look, exe, placeholder, warn));
        let mut items = vec!["none".to_string()];
        items.extend(apps.iter().cloned());
        match widgets::dropdown(ui, look, "running apps", &items, w - 8.0 - field) {
            Some(0) => exe.clear(),
            Some(k) => *exe = items[k].clone(),
            None => {}
        }
    });
}
