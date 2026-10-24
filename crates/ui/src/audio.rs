//! Editing what a recording hears: desktop audio, single apps and microphones, each with its
//! volume and tracks, and the tracks' names. glint's and afterglow's settings both show it.

use std::sync::mpsc::{Receiver, TryRecvError};

use eframe::egui::{Context, Id, Ui, vec2};
use glint_core::config::audio::{AudioSource, MAX_TRACKS, MAX_VOLUME, SourceKind};
use glint_core::record::tracks::plan;
use glint_media::record::{App, Microphone, microphones, running_apps};
use crate::widgets::{self, Kind, Look, hint, row};

struct Devices {
    apps: Vec<App>,
    details: Vec<String>,
    scan: Option<Receiver<Vec<App>>>,
    mics: Vec<Microphone>,
    default_mic: Option<String>,
}

impl Devices {
    fn load(ctx: &Context) -> Devices {
        let mut d = Devices { apps: Vec::new(), details: Vec::new(), scan: None, mics: Vec::new(), default_mic: None };
        d.refresh(ctx);
        d
    }

    fn refresh(&mut self, ctx: &Context) {
        (self.mics, self.default_mic) = microphones();
        self.rescan(ctx);
    }

    fn rescan(&mut self, ctx: &Context) {
        if self.scan.is_none() {
            let ctx = ctx.clone();
            self.scan = Some(running_apps(move || ctx.request_repaint()));
        }
    }

    fn poll(&mut self) {
        let Some(rx) = &self.scan else { return };
        match rx.try_recv() {
            Ok(apps) => {
                self.details = apps.iter().map(App::detail).collect();
                self.apps = apps;
                self.scan = None;
            }
            Err(TryRecvError::Disconnected) => self.scan = None,
            Err(TryRecvError::Empty) => {}
        }
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

/// Which source row is which from frame to frame, so added sources open and removed ones fold.
#[derive(Default)]
struct SourceRows {
    keys: Vec<u64>,
    next: u64,
    listed: widgets::Listed<u64, AudioSource>,
}

impl SourceRows {
    /// Keys for `n` sources; a list that changed size some other way (reset, cancel) gets new ones.
    fn sync(&mut self, n: usize) {
        if self.keys.len() != n {
            self.keys = (self.next..self.next + n as u64).collect();
            self.next += n as u64;
        }
    }

    fn push(&mut self) {
        self.keys.push(self.next);
        self.next += 1;
    }
}

fn source_id(key: &u64) -> Id {
    Id::unique(("audio-source", *key))
}

/// One source's row; whether it asked for an app rescan, and whether "remove" was clicked.
fn source_row(ui: &mut Ui, look: &Look, s: &mut AudioSource, devices: &Devices) -> (bool, bool) {
    let p = look.p;
    let (mut rescan, mut remove) = (false, false);
    row(ui, look, row_label(s.kind), 1.0, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            let label = devices.label(s);
            widgets::toggle(ui, look, &mut s.enabled, &label);
            remove = widgets::link(ui, look, "remove").clicked();
        });
        ui.add_space(4.0);
        match s.kind {
            SourceKind::Desktop => {
                hint(ui, "what your default output device plays, minus one app if you name it", p.t3);
                rescan |= app_picker(ui, look, &mut s.exclude, devices, "leave out an app, like game.exe", false);
            }
            SourceKind::App => {
                hint(ui, "only this app and what it starts; picked up if it starts mid-recording", p.t3);
                let warn = s.app.trim().is_empty();
                rescan |= app_picker(ui, look, &mut s.app, devices, "name.exe", warn);
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
    (rescan, remove)
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

/// The sources' rows, an "add" row and the tracks' names. Keeps the running apps and microphones
/// it lists, and which row is which between frames.
#[derive(Default)]
pub struct AudioEditor {
    devices: Option<Devices>,
    rows: SourceRows,
}

impl AudioEditor {
    pub fn show(&mut self, ui: &mut Ui, look: &Look, sources: &mut Vec<AudioSource>, track_names: &mut Vec<String>) {
        let p = look.p;
        let ctx = ui.ctx().clone();
        let devices = self.devices.get_or_insert_with(|| Devices::load(&ctx));
        devices.poll();
        let rows = &mut self.rows;
        rows.sync(sources.len());
        let current = rows.keys.iter().copied().zip(sources.iter().cloned()).collect();
        let mut rescan = false;
        let mut removed = None;
        for (key, mut leaving, present) in rows.listed.update(&ctx, source_id, current) {
            let at = rows.keys.iter().position(|&k| k == key);
            let drawn = widgets::reveal(ui, source_id(&key), present, |ui| {
                // a removed source folds away drawn from its last copy; clicks on it do nothing
                let s = match at {
                    Some(i) => &mut sources[i],
                    None => &mut leaving,
                };
                let (scan, remove) = source_row(ui, look, s, devices);
                rescan |= scan && present;
                if remove && present {
                    removed = at;
                }
            });
            if drawn.is_none() {
                rows.listed.folded(&key);
            }
        }
        if rescan {
            devices.rescan(&ctx);
        }
        if let Some(i) = removed {
            sources.remove(i);
            rows.keys.remove(i);
        }
        row(ui, look, "add", 7.0, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                for kind in [SourceKind::Desktop, SourceKind::App, SourceKind::Mic] {
                    if widgets::button(ui, look, kind.name(), Kind::Secondary, 28.0, true).clicked() {
                        let n = sources.len();
                        sources.push(AudioSource::new(kind, n));
                        rows.push();
                    }
                }
                if widgets::button(ui, look, "refresh apps", Kind::Text, 28.0, true).clicked()
                    && let Some(d) = &mut self.devices
                {
                    d.refresh(&ctx);
                }
            });
            hint(ui, "app lists show programs with a window, ♪ ones that have played sound first", p.t3);
        });
        let devices = self.devices.get_or_insert_with(|| Devices::load(&ctx));
        let live: Vec<&AudioSource> = sources.iter().filter(|s| s.enabled && s.records()).collect();
        let names: Vec<String> = live.iter().map(|s| devices.label(s)).collect();
        let tracks = plan(&names, &live, track_names);
        let custom = track_names;
        row(ui, look, "tracks", 1.0, |ui| {
            if tracks.is_empty() {
                hint(ui, "no audio: turn a source on and give it a track", p.t3);
                return;
            }
            custom.resize(MAX_TRACKS as usize, String::new());
            for t in &tracks {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    hint(ui, t.number.to_string(), p.t2);
                    let field = (ui.available_width() * 0.4).min(220.0);
                    let name = &mut custom[t.number as usize - 1];
                    ui.allocate_ui(vec2(field, 32.0), |ui| widgets::text_field(ui, look, name, &t.auto, false));
                    let mut about = Vec::new();
                    if t.sources.len() > 1 {
                        about.push(t.sources.iter().map(|&i| names[i].as_str()).collect::<Vec<_>>().join(" + "));
                    }
                    about.extend(t.note(&names));
                    if !about.is_empty() {
                        hint(ui, about.join(" · "), p.t3);
                    }
                });
            }
            hint(ui, "names are what players list; leave one empty to name it after its sources", p.t3);
            hint(ui, "empty tracks and tracks that repeat an earlier one are left out of the file", p.t3);
        });
    }
}

fn app_picker(ui: &mut Ui, look: &Look, exe: &mut String, devices: &Devices, placeholder: &str, warn: bool) -> bool {
    let w = ui.available_width();
    let mut opened = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let field = (w - 8.0) * 0.55;
        ui.allocate_ui(vec2(field, 32.0), |ui| widgets::text_field(ui, look, exe, placeholder, warn));
        let mut rows = vec![("none", "")];
        rows.extend(devices.apps.iter().zip(&devices.details).map(|(a, d)| (a.exe.as_str(), d.as_str())));
        let text = if devices.scan.is_some() { "looking for apps…" } else { "running apps" };
        let pick = widgets::dropdown_detail(ui, look, text, &rows, w - 8.0 - field, w);
        match pick.picked {
            Some(0) => exe.clear(),
            Some(k) => *exe = devices.apps[k - 1].exe.clone(),
            None => {}
        }
        opened = pick.opened;
    });
    opened
}
