//! Settings window, run as `afterglow.exe --settings` in the glint look
//! ([`glint_ui::settings`]): it edits config.toml and asks the tray app to apply it.

use eframe::egui::{self, Id, RichText, Ui};
use glint_core::config::CodecName;
use glint_core::config::audio::SourceKind;
use glint_core::icon::icon_rgba;
use glint_core::paths::app_dir_for;
use glint_media::clip::media::{self, Com};
use glint_shell::{explorer, monitors, window};
use windows::Win32::Media::MediaFoundation::MFVideoFormat_HEVC;
use glint_ui::capture::{Recorder, held_names, key_names};
use glint_ui::settings::{self as frame, Pages};
use glint_ui::widgets::{self, HotkeyState, Kind, Look, hint, row};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SW_SHOWNORMAL, SetForegroundWindow};
use windows::core::{HSTRING, w};

use crate::app::{AMBER, MAIN_CLASS, WM_HOTKEY_PAUSE, WM_RELOAD};
use crate::config::{APP, Config, LENGTHS, MEMORY_CAPS, default_output_dir};

pub const SETTINGS_ARG: &str = "--settings";
const TITLE: &str = "afterglow settings";
const REPO_URL: &str = "https://github.com/nitreojs/glint";
const FPS_PRESETS: [u32; 5] = [0, 30, 60, 120, 144];

#[derive(Clone, Copy, PartialEq)]
pub enum Page {
    General,
    Video,
    Audio,
    About,
}

const PAGES: [Page; 4] = [Page::General, Page::Video, Page::Audio, Page::About];

/// Opens the window, or brings the open one forward.
pub fn run() {
    unsafe {
        if let Ok(h) = FindWindowW(None, &HSTRING::from(TITLE)) {
            let _ = SetForegroundWindow(h);
            return;
        }
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
    /// The monitors as the dropdown lists them: (label, number in [`Config::monitor`]).
    monitors: Vec<(String, u32)>,
    /// The bitrate to go back to after trying constant quality.
    last_bitrate: u32,
    /// Windows can decode hevc: the clips can be played and trimmed here.
    plays_hevc: bool,
}

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

fn quality_word(q: u32) -> &'static str {
    match q {
        0..=14 => "near lossless",
        15..=20 => "very sharp",
        21..=24 => "sharp",
        25..=29 => "balanced",
        _ => "small files",
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
        let size = |r: glint_core::geom::rect::Rect| format!("{} × {}", r.w, r.h);
        let primary = outputs.iter().find(|o| o.rect.contains(0, 0)).map(|o| size(o.rect));
        let mut monitors = vec![(primary.map_or("primary".into(), |s| format!("primary ({s})")), 0)];
        monitors.extend(outputs.iter().enumerate().map(|(i, o)| (format!("{} · {}", i + 1, size(o.rect)), i as u32 + 1)));
        let last_bitrate = if cfg.bitrate > 0 { cfg.bitrate } else { Config::default().bitrate };
        let keys = Recorder::new(release_tray_hotkey);
        let plays_hevc = {
            let _com = Com::init();
            media::has_decoder(MFVideoFormat_HEVC)
        };
        Settings { saved: cfg.clone(), cfg, error, keys, licences: false, monitors, last_bitrate, plays_hevc }
    }

    fn general(&mut self, ui: &mut Ui, look: &Look) {
        let t3 = look.p.t3;
        row(ui, look, "buffer at launch", 1.0, |ui| {
            let text = if self.cfg.buffer { "on" } else { "off" };
            widgets::toggle(ui, look, &mut self.cfg.buffer, text);
            hint(ui, "off waits for \"resume buffer\" in the tray menu", t3);
        });
        let lengths = with_current(&LENGTHS, self.cfg.length);
        let labels: Vec<String> = lengths.iter().map(|&l| length_label(l)).collect();
        let options: Vec<(u32, &str)> = lengths.iter().copied().zip(labels.iter().map(String::as_str)).collect();
        row(ui, look, "length", 7.0, |ui| {
            widgets::segmented(ui, look, &mut self.cfg.length, &options, None);
            hint(ui, "a save covers this much of what just happened", t3);
        });
        self.hotkey_row(ui, look);
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
            ui.add_space(2.0);
            hint(ui, "saved clips go here. leave empty for the default.", t3);
        });
        row(ui, look, "launch at startup", 1.0, |ui| {
            let text = if self.cfg.launch_at_startup { "on" } else { "off" };
            widgets::toggle(ui, look, &mut self.cfg.launch_at_startup, text);
            hint(ui, "afterglow starts in the tray when you sign in to windows", t3);
        });
    }

    fn hotkey_row(&mut self, ui: &mut Ui, look: &Look) {
        let keys = key_names(&self.cfg.save_hotkey);
        let held = held_names();
        let listening = self.keys.listening();
        let problem = self.keys.error().cloned();
        let state = match &problem {
            Some((keys, _)) => HotkeyState::Error { keys },
            None if listening => HotkeyState::Listening { held: &held },
            None => HotkeyState::Idle,
        };
        let mut clicked = false;
        row(ui, look, "save hotkey", 7.0, |ui| {
            clicked = widgets::hotkey_field(ui, look, &keys, state).clicked();
            let error = problem.as_ref().map(|(_, msg)| msg.clone());
            widgets::reveal_with(ui, Id::unique("hotkey-error"), error, |ui, msg| hint(ui, msg, look.p.warn));
            hint(ui, "a click on the tray icon saves too", look.p.t3);
        });
        if clicked {
            if listening {
                self.keys.stop();
            } else if let Err(e) = self.keys.start() {
                self.error = Some(e);
            }
        }
    }

    fn video(&mut self, ui: &mut Ui, look: &Look) {
        let t3 = look.p.t3;
        row(ui, look, "monitor", 7.0, |ui| {
            let current = self.monitors.iter().find(|(_, n)| *n == self.cfg.monitor).map_or_else(
                || format!("{} (not connected)", self.cfg.monitor),
                |(label, _)| label.clone(),
            );
            let items: Vec<String> = self.monitors.iter().map(|(l, _)| l.clone()).collect();
            if let Some(i) = widgets::dropdown(ui, look, &current, &items, 260.0) {
                self.cfg.monitor = self.monitors[i].1;
            }
            hint(ui, "the whole monitor is kept; primary follows whichever is primary", t3);
        });
        let fps = with_current(&FPS_PRESETS, self.cfg.fps);
        let labels: Vec<String> = fps.iter().map(|&f| if f == 0 { "native".into() } else { f.to_string() }).collect();
        let options: Vec<(u32, &str)> = fps.iter().copied().zip(labels.iter().map(String::as_str)).collect();
        row(ui, look, "frame rate", 7.0, |ui| {
            widgets::segmented(ui, look, &mut self.cfg.fps, &options, None);
            hint(ui, "native follows the monitor's refresh rate", t3);
        });
        row(ui, look, "codec", 7.0, |ui| {
            let codecs = [(CodecName::H264, "h.264"), (CodecName::Hevc, "hevc")];
            widgets::segmented(ui, look, &mut self.cfg.codec, &codecs, Some(168.0));
            hint(ui, "hevc looks as good at about 30 % less · h.264 plays everywhere, discord included", t3);
            let stuck = self.cfg.codec == CodecName::Hevc && !self.plays_hevc;
            widgets::reveal(ui, Id::unique("no-hevc-decoder"), stuck, |ui| {
                let text = "this pc can't play hevc back: glint's trimmer and the windows player need \
                            \"hevc video extensions\" from the microsoft store";
                hint(ui, text, look.p.warn);
            });
        });
        let mut fixed = self.cfg.bitrate > 0;
        row(ui, look, "size", 7.0, |ui| {
            let modes = [(true, "bitrate"), (false, "constant quality")];
            if widgets::segmented(ui, look, &mut fixed, &modes, Some(260.0)).changed() {
                if fixed {
                    self.cfg.bitrate = self.last_bitrate;
                } else {
                    self.last_bitrate = self.cfg.bitrate;
                    self.cfg.bitrate = 0;
                }
            }
            let text = if fixed {
                "a known size: busy scenes get softer, calm ones smaller"
            } else {
                "the same look always: busy scenes take more memory, up to the cap below"
            };
            hint(ui, text, t3);
        });
        widgets::reveal(ui, Id::unique("bitrate"), fixed, |ui| {
            row(ui, look, "bitrate", 0.0, |ui| {
                // folding away under constant quality, it keeps showing the bitrate it comes back with
                let mut leaving = self.last_bitrate;
                let bitrate = if fixed { &mut self.cfg.bitrate } else { &mut leaving };
                let word = format!("Mbit/s · ≈ {:.0} MB a minute", *bitrate as f64 * 7.5);
                widgets::slider(ui, look, bitrate, 4..=60, &word, "4 ← smaller", "sharper → 60");
            });
        });
        widgets::reveal(ui, Id::unique("quality"), !fixed, |ui| {
            row(ui, look, "quality", 0.0, |ui| {
                let word = quality_word(self.cfg.quality);
                widgets::slider(ui, look, &mut self.cfg.quality, 12..=36, word, "12 ← sharper, bigger", "smaller, softer → 36");
            });
        });
        let caps = with_current(&MEMORY_CAPS, self.cfg.max_memory_mb);
        let labels: Vec<String> = caps.iter().map(|&m| memory_label(m)).collect();
        let options: Vec<(u32, &str)> = caps.iter().copied().zip(labels.iter().map(String::as_str)).collect();
        row(ui, look, "memory", 7.0, |ui| {
            widgets::segmented(ui, look, &mut self.cfg.max_memory_mb, &options, None);
            let text = match self.cfg.bytes_for(self.cfg.length) {
                Some(b) => {
                    let mb = b as f64 / 1e6;
                    let fits = mb <= self.cfg.max_memory_mb as f64 * 1.048_576;
                    let save = length_label(self.cfg.length);
                    if fits {
                        format!("a {save} buffer takes about {mb:.0} MB, and so does each save")
                    } else {
                        format!("a {save} buffer needs about {mb:.0} MB: past the cap, saves cover less time")
                    }
                }
                None => "the most the buffer holds; past it, a save covers less time".into(),
            };
            hint(ui, text, t3);
        });
    }

    fn audio(&mut self, ui: &mut Ui, look: &Look) {
        let t3 = look.p.t3;
        for (kind, label, about) in [
            (SourceKind::Desktop, "desktop audio", "what your speakers or headphones play"),
            (SourceKind::Mic, "microphone", "your default microphone"),
        ] {
            row(ui, look, label, 1.0, |ui| {
                let mut on = self.cfg.hears(kind);
                let text = if on { "on" } else { "off" };
                if widgets::toggle(ui, look, &mut on, text).changed() {
                    self.cfg.set_hears(kind, on);
                }
                hint(ui, about, t3);
            });
        }
        let both = self.cfg.hears(SourceKind::Desktop) && self.cfg.hears(SourceKind::Mic);
        widgets::reveal(ui, Id::unique("both-tracks"), both, |ui| {
            row(ui, look, "tracks", 0.0, |ui| {
                hint(ui, "track 1 has both mixed; desktop is also on track 2 and the mic on track 3", t3);
            });
        });
        let others = self.cfg.audio_sources.iter().filter(|s| s.kind == SourceKind::App || !s.device.is_empty()).count();
        widgets::reveal(ui, Id::unique("other-sources"), others > 0, |ui| {
            row(ui, look, "more sources", 0.0, |ui| {
                let s = if others == 1 { "" } else { "s" };
                hint(ui, format!("{others} more source{s} set in config.toml (apps, other microphones)"), t3);
            });
        });
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
            hint(ui, "every page at once. nothing changes until you save.", p.t3);
        });
    }

    fn page_changes(page: Page, a: &Config, b: &Config) -> usize {
        let n = |same: bool| usize::from(!same);
        match page {
            Page::General => {
                n(a.buffer == b.buffer)
                    + n(a.length == b.length)
                    + n(a.save_hotkey == b.save_hotkey)
                    + n(a.output_dir == b.output_dir)
                    + n(a.launch_at_startup == b.launch_at_startup)
            }
            Page::Video => {
                n(a.monitor == b.monitor)
                    + n(a.fps == b.fps)
                    + n(a.codec == b.codec)
                    + n(a.bitrate == b.bitrate)
                    + n(a.quality == b.quality)
                    + n(a.max_memory_mb == b.max_memory_mb)
            }
            Page::Audio => n(a.audio_sources == b.audio_sources),
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
            Page::About => "about",
        }
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
                c.output_dir = d.output_dir;
                c.launch_at_startup = d.launch_at_startup;
            }
            Page::Video => {
                c.monitor = d.monitor;
                c.fps = d.fps;
                c.codec = d.codec;
                c.bitrate = d.bitrate;
                c.quality = d.quality;
                c.max_memory_mb = d.max_memory_mb;
            }
            Page::Audio => c.audio_sources = d.audio_sources,
            Page::About => {}
        }
    }

    fn show(&mut self, page: Page, ui: &mut Ui, look: &Look) {
        match page {
            Page::General => self.general(ui, look),
            Page::Video => self.video(ui, look),
            Page::Audio => self.audio(ui, look),
            Page::About => self.about(ui, look),
        }
    }

    fn error(&self) -> Option<String> {
        self.error.clone()
    }

    fn can_save(&self) -> bool {
        !self.keys.listening()
    }

    fn save(&mut self) -> bool {
        match self.cfg.save() {
            Ok(()) => {
                window::post(MAIN_CLASS, WM_RELOAD, 0);
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
    }

    fn tick(&mut self) {
        if let Some(combo) = self.keys.poll(&[]) {
            self.cfg.save_hotkey = combo;
        }
    }

    fn busy(&self) -> bool {
        self.keys.listening()
    }

    fn overlays(&mut self, ctx: &egui::Context, look: &Look) -> bool {
        const CRATES: &str = "egui, eframe, winit, windows-rs, serde, toml";
        if self.licences {
            self.licences = frame::licences(ctx, look, CRATES);
        }
        self.licences
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            cfg.set_hears(SourceKind::Mic, true);
            let mut app = Frame::new(Settings::new(cfg, None), (WINDOW, WINDOW));
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
            }
        }
    }

    #[test]
    fn each_page_counts_and_resets_only_its_own_settings() {
        let mut s = Settings::new(Config::default(), None);
        s.cfg.length = 300;
        s.cfg.quality = 18;
        s.cfg.set_hears(SourceKind::Mic, true);
        let counts: Vec<usize> = PAGES.iter().map(|&p| s.changes(p)).collect();
        assert_eq!(counts, [1, 1, 1, 0]);
        s.reset(Page::Video);
        assert_eq!((s.cfg.quality, s.cfg.length), (23, 300), "video's reset leaves the length alone");
        assert_eq!(s.resettable(Page::General), Some(true));
        assert_eq!(s.resettable(Page::About), None);
    }
}
