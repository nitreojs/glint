use eframe::egui::{Id, Ui};
use glint_core::config::{ClickMode, CodecName, Config, IndicatorInside, Keystrokes};
use glint_core::record::keys::Position;
use glint_ui::widgets::{self, Look, hint, nested_row, row, toggle_row};

use crate::settings::SettingsApp;

const FPS_PRESETS: [u32; 6] = [0, 30, 60, 120, 144, 240];
const COUNTDOWN_PRESETS: [u32; 3] = [0, 3, 5];

/// The quality row's presets, each a (quality, encoder effort) pair; balanced is the default.
#[derive(Clone, Copy, PartialEq)]
enum Preset {
    Smaller,
    Balanced,
    Sharper,
}

impl Preset {
    const ALL: [(Preset, &'static str); 3] =
        [(Preset::Smaller, "smaller"), (Preset::Balanced, "balanced"), (Preset::Sharper, "sharper")];

    fn values(self) -> (u32, u32) {
        match self {
            Preset::Smaller => (28, 50),
            Preset::Balanced => {
                let d = Config::default();
                (d.quality, d.effort)
            }
            Preset::Sharper => (20, 100),
        }
    }

    fn of(cfg: &Config) -> Option<Preset> {
        Preset::ALL.into_iter().map(|(p, _)| p).find(|p| p.values() == (cfg.quality, cfg.effort))
    }

    fn hint(self) -> &'static str {
        match self {
            Preset::Smaller => "smaller files, fast to encode",
            Preset::Balanced => "good for most clips",
            Preset::Sharper => "very sharp, larger files, more gpu load",
        }
    }
}

fn fps_label(fps: u32) -> String {
    match fps {
        0 => "native".into(),
        n => n.to_string(),
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

fn effort_word(e: u32) -> &'static str {
    match e {
        0..=24 => "fastest",
        25..=49 => "fast",
        50..=69 => "balanced",
        _ => "best compression",
    }
}

/// `choices` plus `current` if it isn't one of them (a hand-edited value stays selectable).
fn with_current(choices: &[u32], current: u32) -> Vec<u32> {
    let mut all = choices.to_vec();
    if !all.contains(&current) {
        all.push(current);
        all.sort_unstable();
    }
    all
}

impl SettingsApp {
    pub(in crate::settings) fn recording(&mut self, ui: &mut Ui, look: &Look) {
        let t3 = look.p.t3;
        let presets = with_current(&FPS_PRESETS, self.cfg.fps);
        let labels: Vec<String> = presets.iter().map(|&f| fps_label(f)).collect();
        let fps: Vec<(u32, &str)> = presets.iter().copied().zip(labels.iter().map(String::as_str)).collect();
        row(ui, look, "frame rate", 7.0, |ui| {
            widgets::segmented(ui, look, &mut self.cfg.fps, &fps, None);
            let text = match self.cfg.fps {
                0 => "follows each monitor's refresh rate".to_string(),
                n => format!("{n} fps, or less on slower monitors"),
            };
            hint(ui, text, t3);
        });
        row(ui, look, "codec", 7.0, |ui| {
            let codecs = [(CodecName::H264, "h.264"), (CodecName::Hevc, "hevc")];
            widgets::segmented(ui, look, &mut self.cfg.codec, &codecs, Some(168.0));
            let text = match self.cfg.codec {
                CodecName::H264 => "plays everywhere",
                CodecName::Hevc => "~30 % smaller files, newer players only",
            };
            hint(ui, text, t3);
        });
        let preset = Preset::of(&self.cfg);
        let custom = format!("quality {}, effort {}", self.cfg.quality, self.cfg.effort);
        let text = match preset {
            Some(p) => p.hint(),
            None => custom.as_str(),
        };
        let (quality, effort) = (&mut self.cfg.quality, &mut self.cfg.effort);
        let picked = widgets::preset_row(ui, look, "quality", preset, &Preset::ALL, text, |ui| {
            let word = quality_word(*quality);
            widgets::tuned_slider(ui, look, "quality", quality, 12..=36, word, "12 ← sharper", "smaller → 36");
            let word = effort_word(*effort);
            widgets::tuned_slider(ui, look, "encoder effort", effort, 0..=100, word, "0 ← faster", "smaller files → 100");
        });
        if let Some(p) = picked {
            (self.cfg.quality, self.cfg.effort) = p.values();
        }

        widgets::subheading(ui, look, "while recording");
        let counts = with_current(&COUNTDOWN_PRESETS, self.cfg.countdown);
        let labels: Vec<String> = counts.iter().map(|&s| if s == 0 { "off".into() } else { format!("{s} s") }).collect();
        let choices: Vec<(u32, &str)> = counts.iter().copied().zip(labels.iter().map(String::as_str)).collect();
        row(ui, look, "countdown", 7.0, |ui| {
            widgets::segmented(ui, look, &mut self.cfg.countdown, &choices, Some(200.0));
            widgets::reveal(ui, Id::unique("countdown-cancel"), self.cfg.countdown > 0, |ui| {
                hint(ui, "esc or the record hotkey cancels it", t3);
            });
        });
        row(ui, look, "frame overlay", 7.0, |ui| {
            let choices = [(IndicatorInside::Hide, "hide"), (IndicatorInside::Show, "show")];
            widgets::segmented(ui, look, &mut self.cfg.indicator_inside, &choices, Some(168.0));
            let text = match self.cfg.indicator_inside {
                IndicatorInside::Hide => "nothing is drawn around the recorded area",
                IndicatorInside::Show => {
                    "kept out of the video, but nvidia instant replay and some capture apps pause while it's visible"
                }
            };
            hint(ui, text, t3);
        });
        let text = if self.cfg.draw_cursor { "shown in captures" } else { "hidden" };
        toggle_row(ui, look, "mouse cursor", &mut self.cfg.draw_cursor, text);
        toggle_row(ui, look, "when it stops", &mut self.cfg.open_trimmer, "open the trimmer");
    }

    /// Click highlights and keystrokes: what's drawn over the video rather than how it's encoded.
    pub(in crate::settings) fn overlays_page(&mut self, ui: &mut Ui, look: &Look) {
        let t3 = look.p.t3;
        let text = if self.cfg.click_highlights { "a ripple on every click" } else { "off" };
        toggle_row(ui, look, "click highlights", &mut self.cfg.click_highlights, text);
        row(ui, look, "keystrokes", 7.0, |ui| {
            let levels = [(Keystrokes::Off, "off"), (Keystrokes::Shortcuts, "shortcuts"), (Keystrokes::All, "all keys")];
            widgets::segmented(ui, look, &mut self.cfg.keystrokes, &levels, Some(240.0));
            let text = match self.cfg.keystrokes {
                Keystrokes::Off => "no keys are shown",
                Keystrokes::Shortcuts => "only combos with ctrl, alt or win, and special keys",
                Keystrokes::All => "everything you type, except password fields",
            };
            hint(ui, text, t3);
        });
        let keys = self.cfg.keystrokes != Keystrokes::Off;
        widgets::reveal(ui, Id::unique("keystroke-position"), keys, |ui| {
            nested_row(ui, look, "position", 7.0, |ui| {
                let places = [(Position::Left, "left"), (Position::Centre, "centre"), (Position::Right, "right")];
                widgets::segmented(ui, look, &mut self.cfg.keystroke_position, &places, Some(200.0));
            });
        });
        let clicks = self.cfg.click_highlights;
        widgets::reveal(ui, Id::unique("overlays-in-video"), clicks || keys, |ui| self.in_the_video(ui, look, clicks, keys));
        widgets::reveal(ui, Id::unique("overlays-off"), !clicks && !keys, |ui| {
            widgets::note_row(ui, look, "more options appear when click highlights or keystrokes are on");
        });
    }

    /// One editable / burn in choice for both overlays; it sets clicks' and keystrokes' modes
    /// together. `clicks` and `keys` say which overlays are on.
    fn in_the_video(&mut self, ui: &mut Ui, look: &Look, clicks: bool, keys: bool) {
        let t3 = look.p.t3;
        row(ui, look, "in the video", 7.0, |ui| {
            let mut mode = if clicks || !keys { self.cfg.clicks } else { self.cfg.keystrokes_mode };
            let modes = [(ClickMode::Editable, "editable"), (ClickMode::BurnIn, "burn in")];
            if widgets::segmented(ui, look, &mut mode, &modes, Some(168.0)).changed() {
                (self.cfg.clicks, self.cfg.keystrokes_mode) = (mode, mode);
            }
            let word = |m: ClickMode| if m == ClickMode::Editable { "editable" } else { "burnt in" };
            let what = match (clicks, keys) {
                (true, true) => "clicks and keys",
                (true, false) => "clicks",
                _ => "keys",
            };
            let text = if clicks && keys && self.cfg.clicks != self.cfg.keystrokes_mode {
                format!("clicks {}, keys {}", word(self.cfg.clicks), word(self.cfg.keystrokes_mode))
            } else if mode == ClickMode::Editable {
                format!("{what} can be moved or removed in the trimmer")
            } else {
                "drawn into the video; can't be changed later".to_string()
            };
            hint(ui, text, t3);
        });
    }
}
