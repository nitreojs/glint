//! Settings window, run as `afterglow.exe --settings` in the glint look
//! ([`glint_ui::settings`]): it edits config.toml and asks the tray app to apply it.

use eframe::egui::{self, Id, RichText, Ui};
use glint_core::config::CodecName;
use glint_core::events::addon::State;
use glint_core::format::human_duration;
use glint_core::geom::monitor::Output;
use glint_core::icon::icon_rgba;
use glint_core::paths::app_dir_for;
use glint_media::clip::media::{self, Com};
use glint_shell::{explorer, monitors, window};
use windows::Win32::Media::MediaFoundation::MFVideoFormat_HEVC;
use glint_ui::audio::AudioEditor;
use glint_ui::capture::{Recorder, held_names, key_names};
use glint_ui::naming;
use glint_ui::settings::{self as frame, Pages};
use glint_ui::widgets::{self, Hotkey, HotkeyState, Kind, Look, anim, ease, glide, hint, mix, row};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SW_SHOWNORMAL};
use windows::core::{HSTRING, w};

use glint_shell::afterglow::MAIN_CLASS;

use crate::addon::{self, Status};
use crate::app::{AMBER, WM_HOTKEY_PAUSE, WM_PREVIEW_NOTE, WM_RELOAD};
use crate::config::{APP, Config, LENGTHS, MEMORY_CAPS, NAMES, Place, default_output_dir};

pub const SETTINGS_ARG: &str = "--settings";
pub const TITLE: &str = "afterglow settings";
const REPO_URL: &str = "https://github.com/nitreojs/glint";
const FPS_PRESETS: [u32; 5] = [0, 30, 60, 120, 144];

#[derive(Clone, Copy, PartialEq)]
pub enum Page {
    General,
    Video,
    Audio,
    Output,
    Games,
    About,
}

const PAGES: [Page; 6] = [Page::General, Page::Video, Page::Audio, Page::Output, Page::Games, Page::About];
const BETA: &str = "beta";

/// Opens the window, or brings the open one forward.
pub fn run() {
    if let Some(h) = glint_ui::window::find(TITLE) {
        return glint_ui::window::bring_forward(h);
    }
    let (cfg, error) = match Config::load() {
        Ok(c) => (c, None),
        Err(e) => (Config::default(), Some(format!("couldn't read the settings, showing defaults: {e}"))),
    };
    frame::run(TITLE, app_dir_for(APP).join("settings-window.json"), icon_rgba(64, AMBER), Settings::new(cfg, error));
}

fn release_tray_hotkey(release: bool) {
    window::post(MAIN_CLASS, WM_HOTKEY_PAUSE, usize::from(release));
}

pub struct Settings {
    saved: Config,
    cfg: Config,
    error: Option<String>,
    keys: Recorder,
    licences: bool,
    /// The monitors as the dropdown lists them, primary first.
    screens: Vec<Screen>,
    /// The bitrate to go back to after constant quality.
    last_bitrate: u32,
    /// Windows can decode hevc: the clips can be played and trimmed here.
    plays_hevc: bool,
    audio: AudioEditor,
    /// The Deadlock addon as it stood when the window opened or last saved.
    addon: Status,
}

/// A monitor the buffer can keep: what the dropdown calls it, its number in [`Config::monitor`],
/// and what the size estimates need.
struct Screen {
    label: String,
    n: u32,
    size: (u32, u32),
    hz: f64,
}

/// The quality presets: constant qp, as glint's smaller / balanced / sharper.
const PRESETS: [(u32, &str); 3] = [(28, "smaller"), (23, "balanced"), (20, "sharper")];
/// Where "fixed bitrate" starts when there's no earlier bitrate to go back to.
const BITRATE: u32 = 15;

fn length_label(secs: u32) -> String {
    match secs {
        s if s % 60 == 0 => format!("{} min", s / 60),
        s => format!("{s} s"),
    }
}

fn memory_label(mb: u32) -> String {
    match mb {
        m if m >= 1024 && m % 1024 == 0 => format!("{} GB", m / 1024),
        m => format!("{m} MB"),
    }
}

/// `bytes` as the memory cap counts them: MB (tens of them past 100) or GB.
fn size_label(bytes: f64) -> String {
    let mb = bytes / 1_048_576.0;
    if mb >= 1000.0 {
        return format!("{:.1} GB", mb / 1024.0);
    }
    let step = if mb >= 100.0 { 10.0 } else { 1.0 };
    format!("{} MB", (mb / step).round() * step)
}

fn quality_word(q: u32) -> &'static str {
    match q {
        0..=14 => "near lossless",
        15..=20 => "very sharp",
        21..=24 => "sharp",
        25..=29 => "balanced",
        _ => "small files",
    }
}

fn place_label(place: Place) -> &'static str {
    match place {
        Place::TopLeft => "top left",
        Place::Top => "top middle",
        Place::TopRight => "top right",
        Place::BottomLeft => "bottom left",
        Place::Bottom => "bottom middle",
        Place::BottomRight => "bottom right",
    }
}

/// Presets, plus the current value when it's none of them (typed into config.toml).
fn with_current<T: Copy + PartialEq + Ord>(presets: &[T], current: T) -> Vec<T> {
    let mut v = presets.to_vec();
    if !v.contains(&current) {
        v.push(current);
        v.sort();
    }
    v
}

impl Settings {
    fn new(cfg: Config, error: Option<String>) -> Settings {
        let outputs = monitors::enumerate().unwrap_or_default();
        let screen = |label: String, n: u32, o: Option<&Output>| Screen {
            label,
            n,
            size: o.map_or((1920, 1080), |o| (o.rect.w as u32, o.rect.h as u32)),
            hz: o.map_or(60.0, |o| o.refresh_hz),
        };
        let size = |o: &Output| format!("{} × {}", o.rect.w, o.rect.h);
        let primary = outputs.iter().find(|o| o.rect.contains(0, 0));
        let mut screens = vec![screen(primary.map_or("primary".into(), |o| format!("primary ({})", size(o))), 0, primary)];
        screens.extend(outputs.iter().enumerate().map(|(i, o)| screen(format!("{} · {}", i + 1, size(o)), i as u32 + 1, Some(o))));
        let last_bitrate = if cfg.bitrate > 0 { cfg.bitrate } else { BITRATE };
        let keys = Recorder::new(release_tray_hotkey);
        let plays_hevc = {
            let _com = Com::init();
            media::has_decoder(MFVideoFormat_HEVC)
        };
        let audio = AudioEditor::default();
        Settings { saved: cfg.clone(), cfg, error, keys, licences: false, screens, last_bitrate, plays_hevc, audio, addon: addon::status() }
    }

    /// The monitor the buffer keeps: the chosen one, or the primary one it falls back to.
    fn screen(&self) -> &Screen {
        self.screens.iter().find(|s| s.n == self.cfg.monitor).unwrap_or(&self.screens[0])
    }

    fn general(&mut self, ui: &mut Ui, look: &Look) {
        let t3 = look.p.t3;
        self.hotkey_line(ui, look);
        let lengths = with_current(&LENGTHS, self.cfg.length);
        let labels: Vec<String> = lengths.iter().map(|&l| length_label(l)).collect();
        let options: Vec<(u32, &str)> = lengths.iter().copied().zip(labels.iter().map(String::as_str)).collect();
        row(ui, look, "length", 7.0, |ui| {
            widgets::segmented(ui, look, &mut self.cfg.length, &options, None);
        });
        row(ui, look, "notes", 3.0, |ui| {
            if let Some(place) = place_picker(ui, look, self.cfg.note_position) {
                self.cfg.note_position = place;
            }
            hint(ui, format!("{} of the recorded monitor", place_label(self.cfg.note_position)), t3);
        });
        widgets::subheading(ui, look, "at startup");
        let text = if self.cfg.launch_at_startup { "starts in the tray" } else { "only when you open it" };
        widgets::toggle_row(ui, look, "with windows", &mut self.cfg.launch_at_startup, text);
        let text = if self.cfg.buffer { "right away" } else { "waits for resume in the tray" };
        widgets::toggle_row(ui, look, "buffering", &mut self.cfg.buffer, text);
    }

    fn games(&mut self, ui: &mut Ui, look: &Look) {
        row(ui, look, "deadlock events", 1.0, |ui| {
            if widgets::toggle(ui, look, &mut self.cfg.deadlock_addon, "markers for your kills, deaths and assists").changed() && self.cfg.deadlock_addon {
                self.cfg.game_events = true;
            }
            let (text, warn) = self.addon_hint();
            hint(ui, text, if warn { look.p.warn } else { look.p.t3 });
        });
    }

    /// The line under "deadlock events": what turning it on takes, or where the addon stands.
    fn addon_hint(&self) -> (&'static str, bool) {
        let (want, saved) = (self.cfg.deadlock_addon, self.saved.deadlock_addon);
        let installed = matches!(&self.addon, Status::State { state: State::Current(_) | State::Outdated(_), .. });
        let running = matches!(self.addon, Status::State { running: true, .. });
        if !want {
            return match (installed, saved) {
                (true, true) => ("the addon comes out when you save", false),
                (true, false) if running => ("the addon comes out when deadlock closes", false),
                _ => ("afterglow puts an addon into deadlock that tells them", false),
            };
        }
        match &self.addon {
            Status::NoGame => ("deadlock isn't installed through steam on this pc", true),
            Status::NoAddons => ("deadlock loads no addons: its gameinfo.gi names no addons folder", true),
            Status::NoLog => ("add -condebug to deadlock's launch options in steam", true),
            Status::State { state: State::Off, .. } => ("turned off in deadlock mod manager", true),
            Status::State { state: State::Current(_), .. } => ("the addon is in: every match is followed", false),
            Status::State { .. } if !saved => ("the addon goes in when you save", false),
            Status::State { running: true, .. } => ("the addon goes in when deadlock closes", false),
            Status::State { .. } => ("the addon goes in while afterglow runs", false),
        }
    }

    /// The save hotkey as a one-line hotkey list; a click records a new combo in its place.
    fn hotkey_line(&mut self, ui: &mut Ui, look: &Look) {
        let keys = key_names(&self.cfg.save_hotkey);
        let held = held_names();
        let listening = self.keys.listening();
        let problem = self.keys.error().cloned();
        let state = match &problem {
            Some((keys, why)) => HotkeyState::Error { keys, why },
            None if listening => HotkeyState::Listening { held: &held },
            None => HotkeyState::Idle,
        };
        if widgets::hotkeys(ui, look, &[Hotkey { action: "save", keys: &keys, state }]).is_some() {
            if listening {
                self.keys.stop();
            } else if let Err(e) = self.keys.start() {
                self.error = Some(e);
            }
        }
    }

    fn video(&mut self, ui: &mut Ui, look: &Look) {
        let p = look.p;
        row(ui, look, "monitor", 7.0, |ui| {
            let current = self.screens.iter().find(|s| s.n == self.cfg.monitor).map_or_else(
                || format!("{} (not connected)", self.cfg.monitor),
                |s| s.label.clone(),
            );
            let items: Vec<String> = self.screens.iter().map(|s| s.label.clone()).collect();
            if let Some(i) = widgets::dropdown(ui, look, &current, &items, 260.0) {
                self.cfg.monitor = self.screens[i].n;
            }
        });
        let (size, hz) = (self.screen().size, self.screen().hz);
        let fps = with_current(&FPS_PRESETS, self.cfg.fps);
        let labels: Vec<String> = fps.iter().map(|&f| if f == 0 { "native".into() } else { f.to_string() }).collect();
        let options: Vec<(u32, &str)> = fps.iter().copied().zip(labels.iter().map(String::as_str)).collect();
        row(ui, look, "frame rate", 7.0, |ui| {
            widgets::segmented(ui, look, &mut self.cfg.fps, &options, None);
            let hz = hz.round();
            match self.cfg.fps {
                0 => hint(ui, format!("{hz} fps, the monitor's refresh rate"), p.t3),
                f if f as f64 > hz => hint(ui, format!("{hz} fps at most: the monitor refreshes no faster"), p.t3),
                _ => {}
            }
        });
        row(ui, look, "codec", 7.0, |ui| {
            let codecs = [(CodecName::H264, "h.264"), (CodecName::Hevc, "hevc")];
            widgets::segmented(ui, look, &mut self.cfg.codec, &codecs, Some(168.0));
            let text = match self.cfg.codec {
                CodecName::H264 => "plays everywhere, discord included",
                CodecName::Hevc => "smaller files, newer players only",
            };
            hint(ui, text, p.t3);
            let stuck = self.cfg.codec == CodecName::Hevc && !self.plays_hevc;
            widgets::reveal(ui, Id::unique("no-hevc-decoder"), stuck, |ui| {
                hint(ui, "this pc can't play hevc: get \"hevc video extensions\" from the microsoft store", p.warn);
            });
        });
        let current = Some(self.cfg.quality).filter(|q| self.cfg.bitrate == 0 && PRESETS.iter().any(|(p, _)| p == q));
        let (average, busy) = self.cfg.rate(size, hz);
        let minute = size_label(average * 60.0);
        let text = match self.cfg.bitrate {
            0 => format!("≈ {minute} a minute in busy games"),
            _ => format!("≈ {minute} a minute"),
        };
        if let Some(q) = widgets::preset_row(ui, look, "quality", current, &PRESETS, &text, |ui| self.fine_tune(ui, look)) {
            if self.cfg.bitrate > 0 {
                self.last_bitrate = self.cfg.bitrate;
                self.cfg.bitrate = 0;
            }
            self.cfg.quality = q;
        }
        let caps = with_current(&MEMORY_CAPS, self.cfg.max_memory_mb);
        let labels: Vec<String> = caps.iter().map(|&m| memory_label(m)).collect();
        let options: Vec<(u32, &str)> = caps.iter().copied().zip(labels.iter().map(String::as_str)).collect();
        row(ui, look, "memory", 7.0, |ui| {
            widgets::segmented(ui, look, &mut self.cfg.max_memory_mb, &options, None);
            let cap = self.cfg.max_memory_mb as f64 * 1_048_576.0;
            let (least, most) = (cap / busy, cap / average);
            let holds = match human_duration(least) {
                least_text if least_text == human_duration(most) => least_text,
                least_text => format!("{least_text}–{}", human_duration(most)),
            };
            if least >= self.cfg.length as f64 {
                hint(ui, format!("holds ≈ {holds}"), p.t3);
            } else {
                hint(ui, format!("holds ≈ {holds}: saves come out shorter than {}", length_label(self.cfg.length)), p.warn);
            }
        });
    }

    /// The fine-tune panel under quality: constant quality or a fixed bitrate, and its slider.
    fn fine_tune(&mut self, ui: &mut Ui, look: &Look) {
        let mut fixed = self.cfg.bitrate > 0;
        let modes = [(false, "constant quality"), (true, "fixed bitrate")];
        if widgets::segmented(ui, look, &mut fixed, &modes, None).changed() {
            if fixed {
                self.cfg.bitrate = self.last_bitrate;
            } else {
                self.last_bitrate = self.cfg.bitrate;
                self.cfg.bitrate = 0;
            }
        }
        widgets::reveal(ui, Id::unique("bitrate"), fixed, |ui| {
            // folding away under constant quality, it keeps showing the bitrate it comes back with
            let mut leaving = self.last_bitrate;
            let bitrate = if fixed { &mut self.cfg.bitrate } else { &mut leaving };
            widgets::tuned_slider(ui, look, "bitrate", bitrate, 4..=60, "Mbit/s, busy scenes soften", "4 ← smaller", "sharper → 60");
        });
        widgets::reveal(ui, Id::unique("quality"), !fixed, |ui| {
            let word = quality_word(self.cfg.quality);
            widgets::tuned_slider(ui, look, "quality", &mut self.cfg.quality, 12..=36, word, "12 ← sharper", "smaller → 36");
        });
    }

    fn audio(&mut self, ui: &mut Ui, look: &Look) {
        self.audio.show(ui, look, &mut self.cfg.audio_sources, &mut self.cfg.audio_track_names);
    }

    fn output(&mut self, ui: &mut Ui, look: &Look) {
        row(ui, look, "folder", 7.0, |ui| {
            let placeholder = default_output_dir().display().to_string();
            widgets::text_field(ui, look, &mut self.cfg.output_dir, &placeholder, false);
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let window = unsafe { FindWindowW(None, &HSTRING::from(TITLE)).ok() };
                if widgets::button(ui, look, "browse…", Kind::Secondary, 28.0, true).clicked()
                    && let Some(dir) = explorer::pick_folder(&self.cfg.output_dir(), window)
                {
                    self.cfg.output_dir = dir.display().to_string();
                }
                if widgets::button(ui, look, "open folder", Kind::Secondary, 28.0, true).clicked() {
                    let dir = self.cfg.output_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    open(&dir.display().to_string());
                }
            });
        });
        naming::rows(ui, look, &NAMES, &mut self.cfg.file_name);
    }

    fn about(&mut self, ui: &mut Ui, look: &Look) {
        let p = look.p;
        row(ui, look, "version", 0.0, |ui| {
            let version = concat!("v", env!("CARGO_PKG_VERSION"));
            ui.label(RichText::new(version).font(widgets::mono_medium(13.0)).color(p.t1));
        });
        row(ui, look, "source code", 0.0, |ui| {
            if widgets::link(ui, look, "github.com/nitreojs/glint ↗").clicked() {
                open(REPO_URL);
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

    fn page_changes(page: Page, a: &Config, b: &Config) -> usize {
        let n = |same: bool| usize::from(!same);
        match page {
            Page::General => {
                n(a.buffer == b.buffer)
                    + n(a.length == b.length)
                    + n(a.save_hotkey == b.save_hotkey)
                    + n(a.launch_at_startup == b.launch_at_startup)
                    + n(a.note_position == b.note_position)
            }
            Page::Video => {
                n(a.monitor == b.monitor)
                    + n(a.fps == b.fps)
                    + n(a.codec == b.codec)
                    + n(a.bitrate == b.bitrate)
                    + n(a.quality == b.quality)
                    + n(a.max_memory_mb == b.max_memory_mb)
            }
            Page::Audio => n(a.audio_sources == b.audio_sources) + n(a.audio_track_names == b.audio_track_names),
            Page::Output => n(a.output_dir == b.output_dir) + n(a.file_name == b.file_name),
            Page::Games => n(a.deadlock_addon == b.deadlock_addon) + n(a.game_events == b.game_events),
            Page::About => 0,
        }
    }
}

fn open(target: &str) {
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(target), None, None, SW_SHOWNORMAL);
    }
}

impl Pages for Settings {
    type Page = Page;
    const APP: &'static str = APP;

    fn pages(&self) -> &'static [Page] {
        &PAGES
    }

    fn name(&self, page: Page) -> &'static str {
        match page {
            Page::General => "general",
            Page::Video => "video",
            Page::Audio => "audio",
            Page::Output => "output",
            Page::Games => "games",
            Page::About => "about",
        }
    }

    fn badge(&self, page: Page) -> Option<&'static str> {
        (page == Page::Games).then_some(BETA)
    }

    fn accent(&self) -> [u8; 3] {
        AMBER
    }

    fn changes(&self, page: Page) -> usize {
        Settings::page_changes(page, &self.cfg, &self.saved)
    }

    fn resettable(&self, page: Page) -> Option<bool> {
        (page != Page::About).then(|| Settings::page_changes(page, &self.cfg, &Config::default()) > 0)
    }

    fn reset(&mut self, page: Page) {
        let d = Config::default();
        let c = &mut self.cfg;
        match page {
            Page::General => {
                c.buffer = d.buffer;
                c.length = d.length;
                c.save_hotkey = d.save_hotkey;
                c.launch_at_startup = d.launch_at_startup;
                c.note_position = d.note_position;
            }
            Page::Video => {
                c.monitor = d.monitor;
                c.fps = d.fps;
                c.codec = d.codec;
                c.bitrate = d.bitrate;
                c.quality = d.quality;
                c.max_memory_mb = d.max_memory_mb;
            }
            Page::Audio => {
                c.audio_sources = d.audio_sources;
                c.audio_track_names = d.audio_track_names;
            }
            Page::Output => {
                c.output_dir = d.output_dir;
                c.file_name = d.file_name;
            }
            Page::Games => {
                c.deadlock_addon = d.deadlock_addon;
                c.game_events = d.game_events;
            }
            Page::About => {}
        }
    }

    fn show(&mut self, page: Page, ui: &mut Ui, look: &Look) {
        match page {
            Page::General => self.general(ui, look),
            Page::Video => self.video(ui, look),
            Page::Audio => self.audio(ui, look),
            Page::Output => self.output(ui, look),
            Page::Games => self.games(ui, look),
            Page::About => self.about(ui, look),
        }
    }

    fn error(&self) -> Option<String> {
        self.error.clone()
    }

    fn can_save(&self) -> bool {
        !self.keys.listening() && glint_core::naming::pattern_problem(&self.cfg.file_name).is_none()
    }

    fn save(&mut self) -> bool {
        match self.cfg.save() {
            Ok(()) => {
                window::post(MAIN_CLASS, WM_RELOAD, 0);
                addon::poke();
                self.addon = addon::status();
                self.saved = self.cfg.clone();
                self.error = None;
                true
            }
            Err(e) => {
                self.error = Some(e);
                false
            }
        }
    }

    fn revert(&mut self) {
        self.cfg = self.saved.clone();
    }

    fn leave(&mut self) {
        self.keys.stop();
        self.audio.leave();
    }

    fn tick(&mut self) {
        if let Some(combo) = self.keys.poll(&[]) {
            self.cfg.save_hotkey = combo;
        }
    }

    fn busy(&self) -> bool {
        self.keys.listening() || self.audio.busy()
    }

    fn overlays(&mut self, ctx: &egui::Context, look: &Look) -> bool {
        const CRATES: &str = "egui, eframe, winit, windows-rs, serde, toml";
        if self.licences {
            self.licences = frame::licences(ctx, look, CRATES);
        }
        self.licences
    }
}

/// How long a hovered spot's sample note stays up before it's shown again, under its own hold.
const SAMPLE_AGAIN: f64 = 2.0;

/// A little screen with the six places a note can go; the one clicked, if one was. Hovering a
/// spot shows a sample note there on the recorded monitor, and leaving the screen takes it away.
/// Spots are dots that swell under the pointer; the chosen one stretches into a tiny note.
fn place_picker(ui: &mut Ui, look: &Look, current: Place) -> Option<Place> {
    let p = look.p;
    let amber = egui::Color32::from_rgb(AMBER[0], AMBER[1], AMBER[2]);
    let (screen, _) = ui.allocate_exact_size(egui::vec2(150.0, 84.0), egui::Sense::hover());
    ui.painter().rect(screen, 6.0, p.field, egui::Stroke::new(1.0, p.line2), egui::StrokeKind::Inside);
    let area = glint_core::geom::rect::Rect { x: 0, y: 0, w: screen.width() as i32, h: screen.height() as i32 };
    let margin = 10.0;
    let mut picked = None;
    let mut hovered = None;
    for place in Place::ALL {
        // where the spot sits: the middle of where a full-size note would go
        let full = place.at(area, (34, 9), margin as i32);
        let middle = screen.min + egui::vec2(full.x as f32 + 17.0, full.y as f32 + 4.5);
        let id = Id::unique(("note-place", place.name()));
        let hit = egui::Rect::from_center_size(middle, egui::vec2(48.0, 22.0));
        let r = ui.interact(hit, id, egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
        let on = place == current;
        r.widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, true, on, place_label(place)));
        let chosen = glide(ui, id.with("on"), if on { 1.0 } else { 0.0 }, 0.22);
        let hover = ease(anim(ui, id.with("hov"), r.hovered() && !on, 0.12));
        // a dot that swells toward a pill under the pointer and, chosen, stretches into a note
        // held to the edge it's at: corners grow inward, middles both ways
        let h = 7.0 + 2.0 * chosen + 2.0 * hover * (1.0 - chosen);
        let w = h + (34.0 - 9.0) * chosen + 8.0 * hover * (1.0 - chosen);
        let held = match place {
            Place::TopLeft | Place::BottomLeft => screen.left() + margin,
            Place::Top | Place::Bottom => middle.x - w / 2.0,
            Place::TopRight | Place::BottomRight => screen.right() - margin - w,
        };
        let x = egui::lerp((middle.x - w / 2.0)..=held, chosen);
        let spot = egui::Rect::from_min_size(egui::pos2(x, middle.y - h / 2.0), egui::vec2(w, h));
        let fill = mix(mix(p.line3, p.t3, hover), amber, chosen);
        ui.painter().rect_filled(spot, h / 2.0, fill);
        if r.hovered() {
            hovered = Some(place);
        }
        if r.clicked() {
            picked = Some(place);
        }
    }
    let inside = ui.rect_contains_pointer(screen);
    sample(ui, hovered, inside);
    picked
}

/// Asks the tray app for a sample note at the `hovered` spot, again every [`SAMPLE_AGAIN`] seconds
/// while it stays hovered, and to take it away once the pointer has left the little screen.
fn sample(ui: &Ui, hovered: Option<Place>, inside: bool) {
    let id = Id::unique("note-sample");
    let now = ui.input(|i| i.time);
    let last: Option<(Place, f64)> = ui.data(|d| d.get_temp(id)).flatten();
    match (hovered, last) {
        (Some(place), Some((was, at))) if place == was && now - at < SAMPLE_AGAIN => {}
        (Some(place), _) => {
            let at = Place::ALL.iter().position(|&p| p == place).unwrap_or(0);
            window::post(MAIN_CLASS, WM_PREVIEW_NOTE, at);
            ui.data_mut(|d| d.insert_temp(id, Some((place, now))));
        }
        (None, Some(_)) if !inside => {
            window::post(MAIN_CLASS, WM_PREVIEW_NOTE, Place::ALL.len());
            ui.data_mut(|d| d.insert_temp(id, None::<(Place, f64)>));
        }
        (None, _) => {}
    }
    if hovered.is_some() {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(SAMPLE_AGAIN));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glint_core::config::audio::{AudioSource, SourceKind};
    use eframe::App;
    use eframe::egui::{Rect, pos2, vec2};
    use glint_ui::settings::Frame;
    use glint_ui::theme::fonts;
    use glint_ui::widgets::{CONTENT_X, ROWS, WINDOW};

    fn frames(ctx: &egui::Context, app: &mut Frame<Settings>, width: f32, n: usize, time: &mut f64) {
        let mut frame = eframe::Frame::_new_kittest();
        for _ in 0..n {
            *time += 0.016;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(width, WINDOW[1]))),
                time: Some(*time),
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| app.ui(ui, &mut frame));
        }
    }

    #[test]
    fn every_page_keeps_labels_clear_of_their_controls() {
        for width in [WINDOW[0], 1200.0] {
            let ctx = egui::Context::default();
            fonts(&ctx);
            let mut cfg = Config::default();
            cfg.audio_sources.push(AudioSource { app: "game.exe".into(), ..AudioSource::new(SourceKind::App, 1) });
            cfg.audio_sources.push(AudioSource::new(SourceKind::Mic, 2));
            let mut app = Frame::new(Settings::new(cfg.clone(), None), (WINDOW, WINDOW));
            let mut time = 0.0;
            for page in PAGES {
                app.go(page);
                frames(&ctx, &mut app, width, 30, &mut time);
                ROWS.with_borrow_mut(Vec::clear);
                frames(&ctx, &mut app, width, 1, &mut time);
                let rows = ROWS.take();
                assert!(!rows.is_empty());
                for (label, l, c) in rows {
                    let at = format!("{label} at {width}: label {l:?}, control {c:?}");
                    assert!(l.right() < c.left(), "{at}");
                    assert!(c.right() <= width - CONTENT_X + 0.5, "{at}");
                }
                assert_eq!(app.pages.cfg, cfg, "showing a page changes nothing");
            }
        }
    }

    #[test]
    fn each_page_counts_and_resets_only_its_own_settings() {
        let mut s = Settings::new(Config::default(), None);
        s.cfg.length = 300;
        s.cfg.note_position = Place::Bottom;
        s.cfg.quality = 18;
        s.cfg.audio_sources.push(AudioSource::new(SourceKind::Mic, 1));
        s.cfg.output_dir = r"D:\clips".into();
        let counts: Vec<usize> = PAGES.iter().map(|&p| s.changes(p)).collect();
        assert_eq!(counts, [2, 1, 1, 1, 0, 0]);
        s.reset(Page::Video);
        assert_eq!((s.cfg.quality, s.cfg.length), (23, 300), "video's reset leaves the length alone");
        s.reset(Page::General);
        assert_eq!((s.cfg.note_position, s.cfg.output_dir.as_str()), (Place::TopRight, r"D:\clips"), "the note goes back, the folder stays");
        assert_eq!(s.resettable(Page::Output), Some(true));
        assert_eq!(s.resettable(Page::About), None);
    }

    #[test]
    fn the_deadlock_hint_says_what_stands_in_the_way() {
        let mut s = Settings::new(Config::default(), None);
        let installed = |running| Status::State { state: State::Current("pak99_dir.vpk".into()), running };
        let missing = |running| Status::State { state: State::Missing, running };
        let hint = |s: &mut Settings, want: bool, saved: bool, status: Status| {
            s.cfg.deadlock_addon = want;
            s.saved.deadlock_addon = saved;
            s.addon = status;
            s.addon_hint()
        };
        assert!(!hint(&mut s, false, false, missing(false)).1, "off: what it does");
        assert_eq!(hint(&mut s, true, false, missing(false)).0, "the addon goes in when you save");
        assert_eq!(hint(&mut s, true, true, missing(true)).0, "the addon goes in when deadlock closes");
        assert_eq!(hint(&mut s, true, true, installed(true)).0, "the addon is in: every match is followed");
        assert_eq!(hint(&mut s, false, true, installed(false)).0, "the addon comes out when you save");
        assert_eq!(hint(&mut s, false, false, installed(true)).0, "the addon comes out when deadlock closes");
        assert_eq!(hint(&mut s, true, true, Status::NoLog), ("add -condebug to deadlock's launch options in steam", true));
        assert!(hint(&mut s, true, true, Status::NoGame).1);
        assert!(hint(&mut s, true, true, Status::State { state: State::Off, running: false }).1);

        s.saved = Config::default();
        s.cfg = Config::default();
        assert_eq!(s.changes(Page::Games), 0);
        s.cfg.deadlock_addon = true;
        s.cfg.game_events = true;
        assert_eq!((s.changes(Page::Games), s.changes(Page::General)), (2, 0), "both are the games page's");
        assert_eq!((s.badge(Page::Games), s.badge(Page::General)), (Some("beta"), None));
        s.reset(Page::Games);
        assert!(!s.cfg.deadlock_addon && !s.cfg.game_events);
    }
}
